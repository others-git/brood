//! The board server: one per machine, plain HTTP on 127.0.0.1, JSON API plus SSE.
//! Guarded by `board.lock` (pid) in the data dir and a fixed port. All card writes
//! happen under one mutex, inside this process.

use super::store::{self, Err, NewCard, Patch};
use super::{client, now_secs, paths, rand_hex, valid_sid};
use crate::{config::Config, procs, state};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const UI: &str = include_str!("ui.html");
const IDLE_DEFAULT_SECS: u64 = 600;
/// Columns where a session-created card is not claimed unless asked (`claim: true`).
const NO_CLAIM_ON_CREATE: [&str; 3] = ["backlog", "ready", "done"];
/// Moving a card into one of these columns releases its claim.
const RELEASE_ON_MOVE: [&str; 2] = ["backlog", "done"];

/// A pushed card that is still unclaimed after this is delivered by the hooks too.
const PUSH_GRACE_SECS: u64 = 60;
const MAX_CONNS: usize = 64;

/// Held (OS file lock) for the whole life of the board: the single-instance guard.
fn lock_path() -> PathBuf {
    state::root().join("board.lock")
}

/// Pid and port of the running board. For gc to read; never used to decide who runs.
fn pid_path() -> PathBuf {
    state::root().join("board.pid")
}

/// Port of the running board from its pid file, else the configured port.
fn running_port(cfg: &Config) -> u16 {
    std::fs::read_to_string(pid_path())
        .ok()
        .and_then(|s| s.lines().nth(1)?.trim().parse().ok())
        .filter(|p| *p != 0)
        .unwrap_or(cfg.kanban_port)
}

/// Take the board's OS lock. None: another board holds it.
pub fn acquire_lock(path: &std::path::Path) -> std::io::Result<Option<File>> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match f.try_lock() {
        Ok(()) => Ok(Some(f)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// Is a board running right now (does someone hold the lock)?
pub fn board_running() -> bool {
    matches!(acquire_lock(&lock_path()), Ok(None))
}

fn state_path() -> PathBuf {
    state::root().join("board-state.json")
}

/// Pid in the lock file (the board's own pid), if any.
pub fn lock_pid() -> Option<u32> {
    std::fs::read_to_string(pid_path())
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
    /// The card's review decision when it was sent.
    #[serde(default)]
    pub decision: String,
    pub at: u64,
    /// When the push transport got it (0 = not pushed). It stays queued until the card is claimed.
    #[serde(default)]
    pub pushed: u64,
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

/// A sent card is delivered once the card is claimed (owner set) or the hooks took it.
/// The push path (`pull_for_push`) only marks it; the hooks (`take_for_hook`) deliver cards
/// never pushed, or pushed more than `PUSH_GRACE_SECS` ago and still unclaimed.
fn hook_takeable(it: &Sent, now: u64) -> bool {
    it.pushed == 0 || now.saturating_sub(it.pushed) > PUSH_GRACE_SECS
}

pub fn take_for_hook(
    inner: &mut Inner,
    sid: &str,
    now: u64,
    max: usize,
    settled: &dyn Fn(&Sent) -> bool,
) -> Vec<Sent> {
    let Some(q) = inner.queue.get_mut(sid) else {
        return vec![];
    };
    let mut out = vec![];
    q.retain(|it| {
        if settled(it) {
            return false;
        }
        if hook_takeable(it, now) && out.len() < max {
            out.push(it.clone());
            return false;
        }
        true
    });
    out
}

pub fn pull_for_push(
    inner: &mut Inner,
    sid: &str,
    now: u64,
    settled: &dyn Fn(&Sent) -> bool,
) -> Vec<Sent> {
    let Some(q) = inner.queue.get_mut(sid) else {
        return vec![];
    };
    q.retain(|it| !settled(it));
    q.iter_mut()
        .filter(|it| it.pushed == 0)
        .map(|it| {
            it.pushed = now;
            it.clone()
        })
        .collect()
}

/// A card was claimed: it needs no delivery to anyone.
pub fn forget_card(inner: &mut Inner, project: &str, card: &str) {
    for q in inner.queue.values_mut() {
        q.retain(|i| !(i.card == card && i.project == project));
    }
}

/// Gone or claimed (by anyone): nothing left to deliver.
fn card_settled(roots: &HashMap<String, PathBuf>, cols: &[String], it: &Sent) -> bool {
    roots
        .get(&it.project)
        .and_then(|r| store::find(r, cols, &it.card))
        .is_none_or(|(_, c)| !c.meta.owner.is_empty())
}

fn roots_of(inner: &Inner) -> HashMap<String, PathBuf> {
    inner
        .projects
        .values()
        .map(|p| (p.id.clone(), paths::native(&p.key)))
        .collect()
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
    /// Persisted in the data dir; re-read by the monitor so `--rotate-token` takes effect.
    token: Mutex<String>,
    columns: Vec<String>,
    delivery: Box<dyn Delivery>,
    conns: AtomicUsize,
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
        503 => "Service Unavailable",
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

const COOKIE: &str = "brood_token";

fn cookie_value(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then(|| v.to_owned())
    })
}

/// The persisted board token: reused on every start. `rotate` (or a missing or bad file)
/// makes a new one.
pub fn token_from(path: &std::path::Path, rotate: bool) -> std::io::Result<String> {
    if !rotate && let Ok(t) = std::fs::read_to_string(path) {
        let t = t.trim();
        if t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(t.to_owned());
        }
    }
    let t = rand_hex(32);
    write_private(path, &t)?;
    Ok(t)
}

fn token_ok(given: &str, want: &str) -> bool {
    given.len() == want.len()
        && given
            .bytes()
            .zip(want.bytes())
            .fold(0u8, |a, (x, y)| a | (x ^ y))
            == 0
}

fn origin_ok(origin: Option<&String>) -> bool {
    origin.is_none_or(|o| {
        o.strip_prefix("http://")
            .is_some_and(|h| host_ok(Some(&h.to_owned())))
    })
}

struct ConnGuard<'a>(&'a AtomicUsize);
impl Drop for ConnGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn handle_conn(sh: Arc<Shared>, stream: TcpStream) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    if sh.conns.fetch_add(1, Ordering::SeqCst) >= MAX_CONNS {
        sh.conns.fetch_sub(1, Ordering::SeqCst);
        write_resp(&stream, 503, "application/json", r#"{"error":"busy"}"#);
        return;
    }
    let _guard = ConnGuard(&sh.conns);
    let Some(req) = read_req(&stream) else {
        return;
    };
    // DNS-rebinding defence: only our own loopback names, and no foreign page origins.
    if !host_ok(req.headers.get("host")) || !origin_ok(req.headers.get("origin")) {
        write_resp(
            &stream,
            403,
            "application/json",
            r#"{"error":"bad host or origin"}"#,
        );
        return;
    }
    if req.method == "GET" && req.path == "/api/health" {
        // Challenge: prove knowledge of the token without revealing it.
        let nonce = req.query.get("nonce").filter(|n| {
            !n.is_empty() && n.len() <= 128 && n.bytes().all(|b| b.is_ascii_hexdigit())
        });
        let out = match nonce {
            Some(n) => {
                json!({"ok": true, "name": "brood-board", "proof": client::proof(&sh.token.lock().unwrap(), n)})
            }
            None => json!({"ok": true, "name": "brood-board"}),
        };
        write_resp(&stream, 200, "application/json", &out.to_string());
        return;
    }
    // Header for API clients; cookie for the browser; `?token=` only on the page load and the event stream.
    let query_ok = req.path == "/" || req.path == "/api/events";
    let want = sh.token.lock().unwrap().clone();
    let cookie = req
        .headers
        .get("cookie")
        .and_then(|c| cookie_value(c, COOKIE));
    let from_query = req.query.get("token").filter(|_| query_ok);
    let ok = [
        req.headers.get("x-brood-token"),
        from_query,
        cookie.as_ref(),
    ]
    .into_iter()
    .flatten()
    .any(|g| token_ok(g, &want));
    if !ok {
        write_resp(&stream, 401, "application/json", r#"{"error":"bad token"}"#);
        return;
    }
    if req.method == "GET" && req.path == "/" {
        // First visit with the token in the URL: set the cookie and go to the bare URL.
        if from_query.is_some_and(|g| token_ok(g, &want)) {
            let head = format!(
                "HTTP/1.1 302 Found\r\nLocation: /\r\nSet-Cookie: {COOKIE}={want}; HttpOnly; SameSite=Strict; Path=/; Max-Age=315360000\r\nReferrer-Policy: no-referrer\r\nCache-Control: no-store\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = (&stream).write_all(head.as_bytes());
            return;
        }
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
        Err::Duplicate(c) => (
            409,
            json!({"error": "a card with this id already exists in the target column", "card": c}),
        ),
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
            if !valid_sid(&sid) {
                return err(400, "bad session id");
            }
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
        // Hook delivery: sent cards never pushed, or pushed long ago and still unclaimed.
        ("POST", ["api", "sessions", sid, "take"]) => {
            if !valid_sid(sid) {
                return err(400, "bad session id");
            }
            let max = body["max"].as_u64().unwrap_or(100) as usize;
            let mut g = sh.inner.lock().unwrap();
            let roots = roots_of(&g);
            let settled = |it: &Sent| card_settled(&roots, cols, it);
            let taken = take_for_hook(&mut g, sid, now_secs(), max, &settled);
            save_state(&g);
            (200, json!({"cards": taken}))
        }
        // Push delivery: long-poll for cards not yet pushed. They stay queued until the card is claimed.
        ("GET", ["api", "sessions", sid, "inbox"]) => {
            if !valid_sid(sid) {
                return err(400, "bad session id");
            }
            let wait: u64 = req
                .query
                .get("wait")
                .and_then(|w| w.parse().ok())
                .unwrap_or(0);
            let end = Instant::now() + Duration::from_secs(wait.min(30));
            loop {
                {
                    let mut g = sh.inner.lock().unwrap();
                    let roots = roots_of(&g);
                    let settled = |it: &Sent| card_settled(&roots, cols, it);
                    let out = pull_for_push(&mut g, sid, now_secs(), &settled);
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
            let g = sh.inner.lock().unwrap();
            let mut r = store::create(
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
            // A card a session creates for work it is doing now is claimed for it, by the same
            // lock as `claim`. Default: any column except backlog, ready and done.
            if let (Ok(card), Some(sid)) = (&r, s(body, "session")) {
                let want = body["claim"]
                    .as_bool()
                    .unwrap_or_else(|| !NO_CLAIM_ON_CREATE.contains(&card.column.as_str()));
                if want && g.sessions.contains_key(&sid) {
                    let id = card.meta.id.clone();
                    let alive = |o: &str| g.sessions.contains_key(o);
                    if store::try_lock(&root, &id, &sid, &alive).is_ok() {
                        let patch = Patch {
                            owner: Some(sid),
                            ..Patch::default()
                        };
                        if let Ok(c) = store::update(&root, cols, &id, card.meta.rev, None, patch) {
                            r = Ok(c);
                        }
                    }
                }
            }
            drop(g);
            changed(sh, p, r.map(|c| (201, json!(c))))
        }
        ("PATCH", ["api", "projects", p, "cards", id]) => {
            let Some(root) = project_root(sh, p) else {
                return err(404, "no such project");
            };
            let (Some(rev), Some(ver)) = (body["rev"].as_u64(), s(body, "ver")) else {
                return err(400, "rev and ver required");
            };
            let patch = Patch {
                title: s(body, "title"),
                body: s(body, "body"),
                decision: s(body, "decision"),
                priority: s(body, "priority"),
                tags: tags_of(&body["tags"]),
                owner: None,
            };
            let _g = sh.inner.lock().unwrap();
            let r = store::update(&root, cols, id, rev, Some(&ver), patch);
            drop(_g);
            changed(sh, p, r.map(|c| (200, json!(c))))
        }
        ("POST", ["api", "projects", p, "cards", id, "move"]) => {
            let Some(root) = project_root(sh, p) else {
                return err(404, "no such project");
            };
            let (Some(rev), Some(ver), Some(col)) =
                (body["rev"].as_u64(), s(body, "ver"), s(body, "column"))
            else {
                return err(400, "rev, ver and column required");
            };
            let _g = sh.inner.lock().unwrap();
            let mut r = store::move_card(&root, cols, id, rev, Some(&ver), &col);
            // Done or back to backlog: the work is over, so the claim goes with it.
            if let Ok(card) = &r
                && RELEASE_ON_MOVE.contains(&col.as_str())
                && !card.meta.owner.is_empty()
            {
                store::unlock(&root, id, &card.meta.owner, true);
                let patch = Patch {
                    owner: Some(String::new()),
                    ..Patch::default()
                };
                if let Ok(c) = store::update(&root, cols, id, card.meta.rev, None, patch) {
                    r = Ok(c);
                }
            }
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
            let mut g = sh.inner.lock().unwrap();
            let Some((_, card)) = store::find(&root, cols, id) else {
                return err(404, "no such card");
            };
            if s(body, "ver").is_some_and(|v| v != card.ver) {
                return store_err(Err::Conflict(Box::new(card)));
            }
            let alive = |o: &str| g.sessions.contains_key(o);
            if let Err(owner) = store::try_lock(&root, id, &sid, &alive) {
                return store_err(Err::Claimed(owner));
            }
            let r = store::update(
                &root,
                cols,
                id,
                card.meta.rev,
                None,
                Patch {
                    owner: Some(sid),
                    ..Patch::default()
                },
            );
            forget_card(&mut g, p, id);
            save_state(&g);
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
            if s(body, "ver").is_some_and(|v| v != card.ver) {
                return store_err(Err::Conflict(Box::new(card)));
            }
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
                None,
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
            if !valid_sid(&sid) || !g.sessions.contains_key(&sid) {
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
                decision: card.decision,
                at: now_secs(),
                pushed: 0,
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
                None,
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
        let _ = std::fs::remove_file(pid_path());
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
        if let Some(t) = client::read_token() {
            *sh.token.lock().unwrap() = t;
        }
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
                    // Exit while still holding the mutex: a register that has not run yet
                    // fails (connection reset) and its client starts a new board and retries.
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

#[cfg(unix)]
unsafe extern "C" {
    fn setsid() -> i32;
}

/// Entry for `brood board`. Returns the process exit code.
pub fn run(cfg: &Config, ensure: bool, detach: bool, rotate: bool, url: bool) -> i32 {
    if rotate || url {
        let t = match token_from(&client::token_path(), rotate) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("brood board: token: {e}");
                return 1;
            }
        };
        if rotate {
            eprintln!("token rotated; open the board with `brood board --url`");
        }
        if url {
            println!("http://127.0.0.1:{}/?token={t}", running_port(cfg));
        }
        return 0;
    }
    if ensure {
        client::ensure_board(cfg);
        return 0;
    }
    if detach {
        // Intermediate of the double spawn: new session, start the real board, exit at once.
        #[cfg(unix)]
        // SAFETY: setsid takes no arguments and only changes this process's session.
        unsafe {
            setsid();
        }
        let Ok(exe) = std::env::current_exe() else {
            return 1;
        };
        return match client::board_command(&exe, std::env::vars().map(|(k, _)| k)).spawn() {
            Ok(_) => 0,
            Err(_) => 1,
        };
    }
    let port = cfg.kanban_port;
    let root = state::root();
    let _ = std::fs::create_dir_all(&root);

    // The OS lock is the single-instance guard; it is released when this process dies.
    let _lock = match acquire_lock(&lock_path()) {
        Ok(Some(f)) => f,
        Ok(None) => return 0, // a board is running
        Err(e) => {
            eprintln!("brood board: lock: {e}");
            return 1;
        }
    };
    let _ = std::fs::remove_file(pid_path()); // we hold the lock; any pid file is stale
    let token = match token_from(&client::token_path(), false) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("brood board: token: {e}");
            return 1;
        }
    };
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            if client::board_up(cfg) {
                return 0;
            }
            eprintln!("brood board: cannot bind 127.0.0.1:{port}: {e}");
            return 1;
        }
    };
    // Record the bound port (differs from `port` when it is 0, as in tests).
    let bound = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    let _ = std::fs::write(pid_path(), format!("{}\n{bound}\n", std::process::id()));

    let mut inner: Inner = std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    inner.idle_since = Instant::now();
    let sh = Arc::new(Shared {
        inner: Mutex::new(inner),
        subs: Mutex::new(vec![]),
        token: Mutex::new(token),
        columns: cfg.kanban_columns.clone(),
        delivery: Box::new(QueueDelivery),
        conns: AtomicUsize::new(0),
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
    fn token_persists_until_rotated() {
        let p = std::env::temp_dir().join(format!("brood-tok-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let a = token_from(&p, false).unwrap();
        assert_eq!(a.len(), 32);
        assert_eq!(token_from(&p, false).unwrap(), a);
        let b = token_from(&p, true).unwrap();
        assert_ne!(a, b);
        assert_eq!(token_from(&p, false).unwrap(), b);
        std::fs::write(&p, "junk").unwrap();
        assert_eq!(token_from(&p, false).unwrap().len(), 32);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn cookie_parsing() {
        assert_eq!(
            cookie_value("a=1; brood_token=abc; b=2", COOKIE).as_deref(),
            Some("abc")
        );
        assert_eq!(cookie_value("a=1", COOKIE), None);
    }

    #[test]
    fn token_compare() {
        assert!(token_ok("abc", "abc"));
        assert!(!token_ok("abd", "abc"));
        assert!(!token_ok("", "abc"));
    }

    fn sent(seq: u64, card: &str) -> Sent {
        Sent {
            seq,
            card: card.into(),
            project: "p".into(),
            title: "t".into(),
            body: "b".into(),
            decision: String::new(),
            at: 0,
            pushed: 0,
        }
    }

    #[test]
    fn queue_delivery_parks_items() {
        let mut inner = Inner::default();
        QueueDelivery.deliver(&mut inner, "s1", sent(1, "c"));
        assert_eq!(inner.queue["s1"].len(), 1);
    }

    #[test]
    fn push_does_not_consume_a_card() {
        let open = |_: &Sent| false;
        let mut inner = Inner::default();
        QueueDelivery.deliver(&mut inner, "s1", sent(1, "c1"));
        // Push gets it once and leaves it queued.
        assert_eq!(pull_for_push(&mut inner, "s1", 1000, &open).len(), 1);
        assert_eq!(pull_for_push(&mut inner, "s1", 1001, &open).len(), 0);
        assert_eq!(inner.queue["s1"].len(), 1);
        // Hooks skip it while the push is fresh ...
        assert!(take_for_hook(&mut inner, "s1", 1030, 9, &open).is_empty());
        // ... and deliver it when it is still unclaimed after the grace period.
        let got = take_for_hook(&mut inner, "s1", 1000 + PUSH_GRACE_SECS + 1, 9, &open);
        assert_eq!(got.len(), 1);
        assert!(inner.queue["s1"].is_empty());
    }

    #[test]
    fn claimed_card_is_not_delivered() {
        let mut inner = Inner::default();
        QueueDelivery.deliver(&mut inner, "s1", sent(1, "c1"));
        QueueDelivery.deliver(&mut inner, "s1", sent(2, "c2"));
        pull_for_push(&mut inner, "s1", 1000, &|_| false);
        // Claimed through the API: forgotten everywhere.
        forget_card(&mut inner, "p", "c1");
        assert_eq!(inner.queue["s1"].len(), 1);
        // c2 got claimed (owner set): settled, dropped by the hook.
        let settled = |it: &Sent| it.card == "c2";
        assert!(take_for_hook(&mut inner, "s1", 5000, 9, &settled).is_empty());
        assert!(inner.queue["s1"].is_empty());
    }

    #[test]
    fn os_lock_allows_one_holder() {
        let p = std::env::temp_dir().join(format!("brood-lock-{}", std::process::id()));
        let first = acquire_lock(&p).unwrap();
        assert!(first.is_some());
        assert!(acquire_lock(&p).unwrap().is_none());
        drop(first);
        assert!(acquire_lock(&p).unwrap().is_some());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn origin_check() {
        assert!(origin_ok(None));
        assert!(origin_ok(Some(&"http://127.0.0.1:47777".to_string())));
        assert!(!origin_ok(Some(&"http://evil.example".to_string())));
        assert!(!origin_ok(Some(&"null".to_string())));
    }
}
