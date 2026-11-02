//! The board server: one per machine, plain HTTP on 127.0.0.1, JSON API plus SSE.
//! Guarded by `board.lock` (pid) in the data dir and a fixed port. All card writes
//! happen under one mutex, inside this process.

use super::store::{self, Err, NewCard, Patch};
use super::{client, now_secs, paths, rand_hex};
use crate::{config::Config, procs, state};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const UI: &str = include_str!("ui.html");
const IDLE_DEFAULT_SECS: u64 = 600;
/// A card handed to the push transport but never acked becomes takeable by hooks again after this.
const INFLIGHT_SECS: u64 = 60;

fn lock_path() -> PathBuf {
    state::root().join("board.lock")
}

fn state_path() -> PathBuf {
    state::root().join("board-state.json")
}

/// Pid in the lock file (the board's own pid), if any.
pub fn lock_pid() -> Option<u32> {
    std::fs::read_to_string(lock_path())
        .ok()?
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Project {
    pub id: String,
    pub key: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Session {
    pub session: String,
    pub pid: u32,
    pub project: String,
    pub cwd: String,
    pub registered: u64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Sent {
    pub seq: u64,
    pub card: String,
    pub project: String,
    pub title: String,
    pub body: String,
    pub at: u64,
    /// When the push transport took it (0 = not taken). Set until acked.
    #[serde(default)]
    pub inflight: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Inner {
    pub projects: BTreeMap<String, Project>,
    pub sessions: BTreeMap<String, Session>,
    pub queue: BTreeMap<String, Vec<Sent>>,
    pub next_seq: u64,
    #[serde(skip, default = "Instant::now")]
    pub idle_since: Instant,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            projects: BTreeMap::new(),
            sessions: BTreeMap::new(),
            queue: BTreeMap::new(),
            next_seq: 0,
            idle_since: Instant::now(),
        }
    }
}

/// How a sent card reaches a session. `QueueDelivery` parks it for the hooks
/// (UserPromptSubmit / Stop) and for the MCP channel pusher, which long-polls the inbox.
pub trait Delivery: Send + Sync {
    fn deliver(&self, inner: &mut Inner, session: &str, item: Sent);
}

pub struct QueueDelivery;

impl Delivery for QueueDelivery {
    fn deliver(&self, inner: &mut Inner, session: &str, item: Sent) {
        inner
            .queue
            .entry(session.to_owned())
            .or_default()
            .push(item);
    }
}

struct Shared {
    inner: Mutex<Inner>,
    subs: Mutex<Vec<Sender<String>>>,
    token: String,
    columns: Vec<String>,
    delivery: Box<dyn Delivery>,
}

fn save_state(inner: &Inner) {
    if let Ok(s) = serde_json::to_string(inner) {
        let _ = store::atomic_write(&state_path(), &s);
    }
}

/// For `brood gc`: drop claim locks whose session is gone.
pub fn sweep_dead_locks() -> usize {
    let Some(inner) = std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Inner>(&s).ok())
    else {
        return 0;
    };
    let pids: Vec<u32> = inner.sessions.values().map(|s| s.pid).collect();
    let live = procs::alive_pids(&pids);
    let alive = |sid: &str| {
        inner
            .sessions
            .get(sid)
            .is_some_and(|s| s.pid == 0 || live.contains(&s.pid))
    };
    inner
        .projects
        .values()
        .map(|p| store::sweep_locks(&paths::native(&p.key), &alive))
        .sum()
}

// ---------- HTTP plumbing ----------

struct Req {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn read_req(stream: &TcpStream) -> Option<Req> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut r = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    r.by_ref().take(8192).read_line(&mut line).ok()?;
    let mut it = line.split_whitespace();
    let method = it.next()?.to_owned();
    let target = it.next()?.to_owned();
    let mut headers = HashMap::new();
    let mut total = 0;
    loop {
        let mut h = String::new();
        let n = r.by_ref().take(8192).read_line(&mut h).ok()?;
        total += n;
        if n == 0 || total > 32 * 1024 {
            return None;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if len > 1 << 20 {
        return None;
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    let (path, q) = target.split_once('?').unwrap_or((&target, ""));
    let query = q
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    Some(Req {
        method,
        path: path.to_owned(),
        query,
        headers,
        body,
    })
}

fn write_resp(mut s: &TcpStream, status: u16, ctype: &str, body: &str) {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Error",
    };
    let extra = if ctype.starts_with("text/html") {
        "Referrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'\r\n"
    } else {
        ""
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{extra}Connection: close\r\n\r\n",
        body.len()
    );
    let _ = s.write_all(head.as_bytes());
    let _ = s.write_all(body.as_bytes());
}

fn host_ok(host: Option<&String>) -> bool {
    host.is_some_and(|h| {
        let name = h.rsplit_once(':').map_or(h.as_str(), |(n, _)| n);
        name == "127.0.0.1" || name == "localhost"
    })
}

fn token_ok(given: &str, want: &str) -> bool {
    given.len() == want.len()
        && given
            .bytes()
            .zip(want.bytes())
            .fold(0u8, |a, (x, y)| a | (x ^ y))
            == 0
}

fn handle_conn(sh: Arc<Shared>, stream: TcpStream) {
    let Some(req) = read_req(&stream) else {
        return;
    };
    // DNS-rebinding defence: only our own loopback names.
    if !host_ok(req.headers.get("host")) {
        write_resp(&stream, 403, "application/json", r#"{"error":"bad host"}"#);
        return;
    }
    if req.method == "GET" && req.path == "/api/health" {
        write_resp(
            &stream,
            200,
            "application/json",
            r#"{"ok":true,"name":"brood-board"}"#,
        );
        return;
    }
    let given = req
        .headers
        .get("x-brood-token")
        .or_else(|| req.query.get("token"))
        .map_or("", String::as_str);
    if !token_ok(given, &sh.token) {
        write_resp(&stream, 401, "application/json", r#"{"error":"bad token"}"#);
        return;
    }
    if req.method == "GET" && req.path == "/" {
        write_resp(&stream, 200, "text/html; charset=utf-8", UI);
        return;
    }
    if req.method == "GET" && req.path == "/api/events" {
        sse(&sh, &stream);
        return;
    }
    let body: Value = if req.body.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice(&req.body) {
            Ok(v) => v,
            Err(_) => {
                write_resp(&stream, 400, "application/json", r#"{"error":"bad json"}"#);
                return;
            }
        }
    };
    let (status, out) = route(&sh, &req, &body);
    write_resp(&stream, status, "application/json", &out.to_string());
}

fn sse(sh: &Shared, mut s: &TcpStream) {
    let (tx, rx) = channel::<String>();
    sh.subs.lock().unwrap().push(tx);
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n: hello\n\n";
    if s.write_all(head.as_bytes()).is_err() {
        return;
    }
    loop {
        let msg = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(m) => format!("data: {m}\n\n"),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => ": ka\n\n".to_owned(),
            Err(_) => return,
        };
        if s.write_all(msg.as_bytes()).and_then(|_| s.flush()).is_err() {
            return;
        }
    }
}

fn broadcast(sh: &Shared, ev: Value) {
    let msg = ev.to_string();
    sh.subs
        .lock()
        .unwrap()
        .retain(|tx| tx.send(msg.clone()).is_ok());
}

// ---------- API ----------

fn err(status: u16, msg: &str) -> (u16, Value) {
    (status, json!({"error": msg}))
}

fn store_err(e: Err) -> (u16, Value) {
    match e {
        Err::NotFound => err(404, "no such card"),
        Err::BadRequest(m) => err(400, &m),
        Err::Conflict(c) => (409, json!({"error": "rev mismatch", "card": c})),
        Err::Claimed(o) => (409, json!({"error": "claimed", "owner": o})),
        Err::Io(m) => err(500, &m),
    }
}

fn s(v: &Value, k: &str) -> Option<String> {
    v[k].as_str().map(str::to_owned)
}

fn open_project(inner: &mut Inner, path: &str) -> Result<Project, String> {
    let key = paths::normalize(path);
    if !paths::native(&key).is_dir() {
        return Err(format!("not a directory: {path}"));
    }
    let id = paths::project_id(&key);
    Ok(inner
        .projects
        .entry(id.clone())
        .or_insert_with(|| Project {
            id,
            name: paths::project_name(&key),
            key,
        })
        .clone())
}

fn route(sh: &Arc<Shared>, req: &Req, body: &Value) -> (u16, Value) {
    let segs: Vec<&str> = req.path.trim_matches('/').split('/').collect();
    let cols = &sh.columns;
    match (req.method.as_str(), segs.as_slice()) {
        ("GET", ["api", "projects"]) => {
            let g = sh.inner.lock().unwrap();
            let list: Vec<Value> = g
                .projects
                .values()
                .map(|p| {
                    let n = g.sessions.values().filter(|x| x.project == p.id).count();
                    json!({"id": p.id, "name": p.name, "path": p.key, "sessions": n})
                })
                .collect();
            (200, json!({ "projects": list, "columns": cols }))
        }
        ("POST", ["api", "projects", "open"]) => {
            let Some(path) = s(body, "path") else {
                return err(400, "path required");
            };
            let mut g = sh.inner.lock().unwrap();
            match open_project(&mut g, &path) {
                Ok(p) => {
                    save_state(&g);
                    drop(g);
                    broadcast(sh, json!({"type": "projects"}));
                    (
                        200,
                        json!({"id": p.id, "name": p.name, "path": p.key, "columns": cols}),
                    )
                }
                Err(m) => err(400, &m),
            }
        }
        ("GET", ["api", "sessions"]) => {
            let g = sh.inner.lock().unwrap();
            (
                200,
                json!({"sessions": g.sessions.values().collect::<Vec<_>>()}),
            )
        }
        ("POST", ["api", "sessions", "register"]) => {
            let (Some(sid), Some(path)) = (s(body, "session"), s(body, "path")) else {
                return err(400, "session and path required");
            };
            let mut g = sh.inner.lock().unwrap();
            let p = match open_project(&mut g, &path) {
                Ok(p) => p,
                Err(m) => return err(400, &m),
            };
            g.sessions.insert(
                sid.clone(),
                Session {
                    session: sid,
                    pid: body["pid"].as_u64().unwrap_or(0) as u32,
                    project: p.id,
                    cwd: s(body, "cwd").unwrap_or_default(),
                    registered: now_secs(),
                },
            );
            save_state(&g);
            drop(g);
            broadcast(sh, json!({"type": "sessions"}));
            (200, json!({"ok": true}))
        }
        ("POST", ["api", "sessions", "unregister"]) => {
            let Some(sid) = s(body, "session") else {
                return err(400, "session required");
            };
            let mut g = sh.inner.lock().unwrap();
            if let Some(sess) = g.sessions.remove(&sid) {
                release_all(&g, cols, &sess);
            }
            g.queue.remove(&sid);
            save_state(&g);
            drop(g);
            broadcast(sh, json!({"type": "sessions"}));
            (200, json!({"ok": true}))
        }
        // Hook delivery: take sent cards (skips ones the push transport has in flight).
        ("POST", ["api", "sessions", sid, "take"]) => {
            let max = body["max"].as_u64().unwrap_or(100) as usize;
            let now = now_secs();
            let mut g = sh.inner.lock().unwrap();
            let q = g.queue.entry((*sid).to_owned()).or_default();
            let mut taken = vec![];
            q.retain(|it| {
                let free = it.inflight == 0 || now.saturating_sub(it.inflight) > INFLIGHT_SECS;
                if free && taken.len() < max {
                    taken.push(it.clone());
                    false
                } else {
                    true
                }
            });
            if !taken.is_empty() {
                save_state(&g);
            }
            (200, json!({"cards": taken}))
        }
        // Push delivery: long-poll for cards not yet handed out. They stay queued, in flight, until acked.
        ("GET", ["api", "sessions", sid, "inbox"]) => {
            let wait: u64 = req
                .query
                .get("wait")
                .and_then(|w| w.parse().ok())
                .unwrap_or(0);
            let end = Instant::now() + Duration::from_secs(wait.min(30));
            loop {
                {
                    let mut g = sh.inner.lock().unwrap();
                    let now = now_secs();
                    let mut out = vec![];
                    if let Some(q) = g.queue.get_mut(*sid) {
                        for it in q.iter_mut().filter(|i| i.inflight == 0) {
                            it.inflight = now;
                            out.push(it.clone());
                        }
                    }
                    if !out.is_empty() {
                        save_state(&g);
                        return (200, json!({"cards": out}));
                    }
                }
                if Instant::now() >= end {
                    return (200, json!({"cards": []}));
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        }
        ("POST", ["api", "sessions", sid, "ack"]) => {
            let seqs: Vec<u64> = body["seqs"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_u64).collect())
                .unwrap_or_default();
            let mut g = sh.inner.lock().unwrap();
            if let Some(q) = g.queue.get_mut(*sid) {
                q.retain(|i| !seqs.contains(&i.seq));
            }
            save_state(&g);
            (200, json!({"ok": true}))
        }
        ("POST", ["api", "shutdown"]) => {
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(150));
                exit_cleanup();
                std::process::exit(0);
            });
            (200, json!({"ok": true}))
        }
        ("GET", ["api", "projects", p, "sessions"]) => {
            let g = sh.inner.lock().unwrap();
            let l: Vec<&Session> = g.sessions.values().filter(|x| x.project == *p).collect();
            (200, json!({"sessions": l}))
        }
        ("GET", ["api", "projects", p, "cards"]) => {
            let Some(root) = project_root(sh, p) else {
                return err(404, "no such project");
            };
            let _g = sh.inner.lock().unwrap();
            (
                200,
                json!({"columns": cols, "cards": store::list(&root, cols)}),
            )
        }
        ("POST", ["api", "projects", p, "cards"]) => {
            let Some(root) = project_root(sh, p) else {
                return err(404, "no such project");
            };
            let Some(title) = s(body, "title") else {
                return err(400, "title required");
            };
            let _g = sh.inner.lock().unwrap();
            let r = store::create(
                &root,
                cols,
                NewCard {
                    title,
                    body: s(body, "body").unwrap_or_default(),
                    column: s(body, "column"),
                    priority: s(body, "priority"),
                    tags: tags_of(&body["tags"]).unwrap_or_default(),
                },
            );
            drop(_g);
            changed(sh, p, r.map(|c| (201, json!(c))))
        }
        ("PATCH", ["api", "projects", p, "cards", id]) => {
            let Some(root) = project_root(sh, p) else {
                return err(404, "no such project");
            };
            let Some(rev) = body["rev"].as_u64() else {
                return err(400, "rev required");
            };
            let patch = Patch {
                title: s(body, "title"),
                body: s(body, "body"),
                priority: s(body, "priority"),
                tags: tags_of(&body["tags"]),
                owner: None,
            };
            let _g = sh.inner.lock().unwrap();
            let r = store::update(&root, cols, id, rev, patch);
            drop(_g);
            changed(sh, p, r.map(|c| (200, json!(c))))
        }
        ("POST", ["api", "projects", p, "cards", id, "move"]) => {
            let Some(root) = project_root(sh, p) else {
                return err(404, "no such project");
            };
            let (Some(rev), Some(col)) = (body["rev"].as_u64(), s(body, "column")) else {
                return err(400, "rev and column required");
            };
            let _g = sh.inner.lock().unwrap();
            let r = store::move_card(&root, cols, id, rev, &col);
            drop(_g);
            changed(sh, p, r.map(|c| (200, json!(c))))
        }
        ("POST", ["api", "projects", p, "cards", id, "claim"]) => {
            let Some(root) = project_root(sh, p) else {
                return err(404, "no such project");
            };
            let Some(sid) = s(body, "session") else {
                return err(400, "session required");
            };
            let g = sh.inner.lock().unwrap();
            let Some((_, card)) = store::find(&root, cols, id) else {
                return err(404, "no such card");
            };
            let alive = |o: &str| g.sessions.contains_key(o);
            if let Err(owner) = store::try_lock(&root, id, &sid, &alive) {
                return store_err(Err::Claimed(owner));
            }
            let r = store::update(
                &root,
                cols,
                id,
                card.meta.rev,
                Patch {
                    owner: Some(sid),
                    ..Patch::default()
                },
            );
            drop(g);
            changed(sh, p, r.map(|c| (200, json!(c))))
        }
        ("POST", ["api", "projects", p, "cards", id, "release"]) => {
            let Some(root) = project_root(sh, p) else {
                return err(404, "no such project");
            };
            let sid = s(body, "session").unwrap_or_default();
            let force = body["force"].as_bool().unwrap_or(false);
            let g = sh.inner.lock().unwrap();
            let Some((_, card)) = store::find(&root, cols, id) else {
                return err(404, "no such card");
            };
            if !store::unlock(&root, id, &sid, force) && store::lock_owner(&root, id).is_some() {
                return store_err(Err::Claimed(
                    store::lock_owner(&root, id).unwrap_or_default(),
                ));
            }
            let r = store::update(
                &root,
                cols,
                id,
                card.meta.rev,
                Patch {
                    owner: Some(String::new()),
                    ..Patch::default()
                },
            );
            drop(g);
            changed(sh, p, r.map(|c| (200, json!(c))))
        }
        ("POST", ["api", "projects", p, "cards", id, "send"]) => {
            let Some(root) = project_root(sh, p) else {
                return err(404, "no such project");
            };
            let Some(sid) = s(body, "session") else {
                return err(400, "session required");
            };
            let mut g = sh.inner.lock().unwrap();
            if !g.sessions.contains_key(&sid) {
                return err(404, "session is not live");
            }
            let Some((_, card)) = store::find(&root, cols, id) else {
                return err(404, "no such card");
            };
            g.next_seq += 1;
            let item = Sent {
                seq: g.next_seq,
                card: card.meta.id,
                project: (*p).to_owned(),
                title: card.meta.title,
                body: card.body,
                at: now_secs(),
                inflight: 0,
            };
            sh.delivery.deliver(&mut g, &sid, item);
            save_state(&g);
            (200, json!({"queued": true}))
        }
        _ => err(404, "not found"),
    }
}

fn tags_of(v: &Value) -> Option<Vec<String>> {
    v.as_array().map(|a| {
        a.iter()
            .filter_map(|t| t.as_str().map(|t| t.trim().to_owned()))
            .filter(|t| !t.is_empty())
            .collect()
    })
}

fn project_root(sh: &Shared, id: &str) -> Option<PathBuf> {
    let g = sh.inner.lock().unwrap();
    g.projects.get(id).map(|p| paths::native(&p.key))
}

fn release_all(inner: &Inner, cols: &[String], sess: &Session) {
    let Some(p) = inner.projects.get(&sess.project) else {
        return;
    };
    let root = paths::native(&p.key);
    for id in store::locks_of(&root, &sess.session) {
        store::unlock(&root, &id, &sess.session, false);
        if let Some((_, c)) = store::find(&root, cols, &id) {
            let _ = store::update(
                &root,
                cols,
                &id,
                c.meta.rev,
                Patch {
                    owner: Some(String::new()),
                    ..Patch::default()
                },
            );
        }
    }
}

fn changed(sh: &Shared, project: &str, r: Result<(u16, Value), Err>) -> (u16, Value) {
    match r {
        Ok(v) => {
            broadcast(sh, json!({"type": "cards", "project": project}));
            v
        }
        Err(e) => store_err(e),
    }
}

// ---------- lifecycle ----------

fn exit_cleanup() {
    if lock_pid() == Some(std::process::id()) {
        let _ = std::fs::remove_file(lock_path());
    }
}

fn dir_signature(root: &std::path::Path, cols: &[String]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for col in cols {
        let Ok(rd) = std::fs::read_dir(store::dir(root).join(col)) else {
            continue;
        };
        let mut v: Vec<_> = rd
            .flatten()
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                let t = m
                    .modified()
                    .ok()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?;
                Some((e.file_name(), t.as_nanos(), m.len()))
            })
            .collect();
        v.sort();
        col.hash(&mut h);
        v.hash(&mut h);
    }
    h.finish()
}

/// Every ~1.5 s: notice outside edits (by mtime), reap dead sessions, watch the idle timer.
fn monitor(sh: Arc<Shared>, idle_limit: Duration) {
    let mut sigs: HashMap<String, u64> = HashMap::new();
    let mut tick = 0u64;
    loop {
        std::thread::sleep(Duration::from_millis(1500));
        tick += 1;
        let projects: Vec<Project> = sh
            .inner
            .lock()
            .unwrap()
            .projects
            .values()
            .cloned()
            .collect();
        for p in &projects {
            let sig = dir_signature(&paths::native(&p.key), &sh.columns);
            if sigs.insert(p.id.clone(), sig).is_some_and(|old| old != sig) {
                broadcast(&sh, json!({"type": "cards", "project": p.id}));
            }
        }
        if tick.is_multiple_of(3) {
            let pids: Vec<u32> = {
                let g = sh.inner.lock().unwrap();
                g.sessions
                    .values()
                    .map(|x| x.pid)
                    .filter(|p| *p != 0)
                    .collect()
            };
            let live = procs::alive_pids(&pids);
            let mut g = sh.inner.lock().unwrap();
            let now = now_secs();
            let dead: Vec<String> = g
                .sessions
                .values()
                .filter(|x| {
                    if x.pid == 0 {
                        now.saturating_sub(x.registered) > 24 * 3600
                    } else {
                        !live.contains(&x.pid)
                    }
                })
                .map(|x| x.session.clone())
                .collect();
            if !dead.is_empty() {
                for sid in &dead {
                    if let Some(sess) = g.sessions.remove(sid) {
                        release_all(&g, &sh.columns, &sess);
                    }
                    g.queue.remove(sid);
                }
                save_state(&g);
            }
            if g.sessions.is_empty() {
                if g.idle_since.elapsed() >= idle_limit {
                    drop(g);
                    exit_cleanup();
                    std::process::exit(0);
                }
            } else {
                g.idle_since = Instant::now();
            }
            drop(g);
            if !dead.is_empty() {
                broadcast(&sh, json!({"type": "sessions"}));
            }
        }
    }
}

fn write_private(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    let mut o = OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(content.as_bytes())
}

/// Entry for `brood board`. Returns the process exit code.
pub fn run(cfg: &Config, ensure: bool) -> i32 {
    if ensure {
        client::ensure_board(cfg);
        return 0;
    }
    let port = cfg.kanban_port;
    let root = state::root();
    let _ = std::fs::create_dir_all(&root);

    // Lock: a live holder means a board is already running.
    let mut got = false;
    for _ in 0..3 {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path())
        {
            Ok(mut f) => {
                let _ = write!(f, "{}\n{port}\n", std::process::id());
                got = true;
                break;
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                if let Some(pid) = lock_pid() {
                    let running = pid != std::process::id()
                        && procs::alive_pids(&[pid]).contains(&pid)
                        && (client::health(port)
                            || procs::pid_cmd(pid).is_some_and(|c| c.contains("brood")));
                    if running {
                        return 0;
                    }
                }
                let _ = std::fs::remove_file(lock_path()); // stale: take over
            }
            Err(e) => {
                eprintln!("brood board: lock: {e}");
                return 1;
            }
        }
    }
    if !got {
        return 0; // lost a race with another starter
    }
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            exit_cleanup();
            if client::health(port) {
                return 0;
            }
            eprintln!("brood board: cannot bind 127.0.0.1:{port}: {e}");
            return 1;
        }
    };
    let token = rand_hex(32);
    if let Err(e) = write_private(&client::token_path(), &token) {
        exit_cleanup();
        eprintln!("brood board: token: {e}");
        return 1;
    }

    let mut inner: Inner = std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    inner.idle_since = Instant::now();
    let sh = Arc::new(Shared {
        inner: Mutex::new(inner),
        subs: Mutex::new(vec![]),
        token,
        columns: cfg.kanban_columns.clone(),
        delivery: Box::new(QueueDelivery),
    });
    let idle = std::env::var("BROOD_BOARD_IDLE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(IDLE_DEFAULT_SECS);
    {
        let sh = sh.clone();
        std::thread::spawn(move || monitor(sh, Duration::from_secs(idle)));
    }
    for stream in listener.incoming().flatten() {
        let sh = sh.clone();
        std::thread::spawn(move || handle_conn(sh, stream));
    }
    exit_cleanup();
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_check() {
        assert!(host_ok(Some(&"127.0.0.1:47777".to_string())));
        assert!(host_ok(Some(&"localhost".to_string())));
        assert!(!host_ok(Some(&"evil.example:47777".to_string())));
        assert!(!host_ok(Some(&"127.0.0.1.evil.com".to_string())));
        assert!(!host_ok(None));
    }

    #[test]
    fn token_compare() {
        assert!(token_ok("abc", "abc"));
        assert!(!token_ok("abd", "abc"));
        assert!(!token_ok("", "abc"));
    }

    #[test]
    fn queue_delivery_parks_items() {
        let mut inner = Inner::default();
        let item = Sent {
            seq: 1,
            card: "c".into(),
            project: "p".into(),
            title: "t".into(),
            body: "b".into(),
            at: 0,
            inflight: 0,
        };
        QueueDelivery.deliver(&mut inner, "s1", item);
        assert_eq!(inner.queue["s1"].len(), 1);
    }
}
