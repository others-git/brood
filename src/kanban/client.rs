//! Talking to the board: a tiny blocking HTTP/1.1 client, and starting the board.
//!
//! The client never sends the token to an unverified server: before each request it
//! sends a random nonce to `/api/health` and checks the answer is HMAC-SHA256(token, nonce).

use super::{rand_hex, valid_sid};
use crate::{config::Config, state};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

pub fn token_path() -> std::path::PathBuf {
    state::root().join("board.token")
}

pub fn read_token() -> Option<String> {
    let t = std::fs::read_to_string(token_path())
        .ok()?
        .trim()
        .to_owned();
    (!t.is_empty()).then_some(t)
}

/// Hex HMAC-SHA256 of `nonce` under `token`: the board's proof that it knows the token.
pub fn proof(token: &str, nonce: &str) -> String {
    let mut m =
        Hmac::<Sha256>::new_from_slice(token.as_bytes()).expect("hmac takes any key length");
    m.update(nonce.as_bytes());
    m.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn raw_request(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
    timeout: Duration,
) -> std::io::Result<(u16, String)> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(500))?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(Duration::from_secs(5)))?;
    let body = body.unwrap_or("");
    let mut req =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    if let Some(t) = token {
        req.push_str(&format!("X-Brood-Token: {t}\r\n"));
    }
    req.push_str(&format!(
        "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    ));
    s.write_all(req.as_bytes())?;
    let mut buf = vec![];
    s.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let (head, payload) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    Ok((status, payload.to_owned()))
}

/// Does the server on `port` prove it holds `token`? Sends no secret.
pub fn verify(port: u16, token: &str) -> bool {
    let nonce = rand_hex(32);
    let Ok((200, body)) = raw_request(
        port,
        "GET",
        &format!("/api/health?nonce={nonce}"),
        None,
        None,
        Duration::from_secs(2),
    ) else {
        return false;
    };
    serde_json::from_str::<Value>(&body).is_ok_and(|v| {
        v["name"] == "brood-board" && v["proof"].as_str() == Some(proof(token, &nonce).as_str())
    })
}

/// A verified board answers on the configured port.
pub fn board_up(cfg: &Config) -> bool {
    read_token().is_some_and(|t| verify(cfg.kanban_port, &t))
}

pub struct Conn {
    pub port: u16,
    pub token: String,
}

impl Conn {
    pub fn from_config(cfg: &Config) -> Option<Conn> {
        read_token().map(|token| Conn {
            port: cfg.kanban_port,
            token,
        })
    }

    pub fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), String> {
        self.request_timeout(method, path, body, Duration::from_secs(5))
    }

    pub fn request_timeout(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<(u16, Value), String> {
        if !verify(self.port, &self.token) {
            return Err(format!(
                "nothing on 127.0.0.1:{} proved to be this machine's brood board (port taken, or board down; see kanban_port)",
                self.port
            ));
        }
        let b = body.map(Value::to_string);
        let (status, text) = raw_request(
            self.port,
            method,
            path,
            Some(&self.token),
            b.as_deref(),
            timeout,
        )
        .map_err(|e| format!("board unreachable: {e}"))?;
        Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
    }
}

/// The command that starts a board. Every `CLAUDE_*` variable is removed:
/// `CLAUDE_CODE_SESSION_ID` and `CLAUDE_PID` are what `procs` uses to tag a process
/// as a session leftover, and the board belongs to no session.
pub fn board_command(exe: &Path, env_keys: impl IntoIterator<Item = String>) -> Command {
    let mut c = Command::new(exe);
    c.arg("board")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for k in env_keys.into_iter().filter(|k| k.starts_with("CLAUDE")) {
        c.env_remove(k);
    }
    c.env_remove("CLAUDE_CODE_SESSION_ID");
    c.env_remove("CLAUDE_PID");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
        c.creation_flags(0x0000_0008 | 0x0000_0200 | 0x0800_0000);
    }
    c
}

/// Spawn a board that is not a child of this process or of the session.
/// Unix: a short-lived intermediate (`board --detach`) calls setsid, starts the
/// real board and exits at once, so the board is reparented to init; we reap the
/// intermediate here, so nothing is left as a zombie.
fn spawn_board() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let mut cmd = board_command(&exe, std::env::vars().map(|(k, _)| k));
    if cfg!(unix) {
        cmd.arg("--detach");
    }
    match cmd.spawn() {
        Ok(mut child) => {
            if cfg!(unix) {
                let _ = child.wait();
            }
            true
        }
        Err(_) => false,
    }
}

/// Make sure a verified board answers on the configured port, starting one if needed.
pub fn ensure_board(cfg: &Config) -> bool {
    if board_up(cfg) {
        return true;
    }
    if !spawn_board() {
        return false;
    }
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(100));
        if board_up(cfg) {
            return true;
        }
    }
    false
}

/// One call to the board; if it is down (it may have just idled out), start it and retry once.
fn call_ensuring(
    cfg: &Config,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Option<(u16, Value)> {
    if let Some(r) = Conn::from_config(cfg).and_then(|c| c.request(method, path, body).ok()) {
        return Some(r);
    }
    if !ensure_board(cfg) {
        return None;
    }
    Conn::from_config(cfg)?.request(method, path, body).ok()
}

/// Register this session with the board (hook, SessionStart).
pub fn register_session(cfg: &Config, session: &str, cwd: &str, root: &str, pid: u32) {
    if !valid_sid(session) {
        return;
    }
    let _ = call_ensuring(
        cfg,
        "POST",
        "/api/sessions/register",
        Some(&serde_json::json!({"session": session, "cwd": cwd, "path": root, "pid": pid})),
    );
}

pub fn unregister_session(cfg: &Config, session: &str) {
    if !valid_sid(session) {
        return;
    }
    if let Some(c) = Conn::from_config(cfg) {
        let _ = c.request(
            "POST",
            "/api/sessions/unregister",
            Some(&serde_json::json!({"session": session})),
        );
    }
}

/// Take up to `max` cards sent to this session (hook delivery).
pub fn take_sent(cfg: &Config, session: &str, max: usize) -> Vec<Value> {
    if !valid_sid(session) {
        return vec![];
    }
    match call_ensuring(
        cfg,
        "POST",
        &format!("/api/sessions/{session}/take"),
        Some(&serde_json::json!({"max": max})),
    ) {
        Some((200, v)) => v["cards"].as_array().cloned().unwrap_or_default(),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[test]
    fn board_env_drops_claude_vars() {
        let keys = [
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_PID",
            "CLAUDE_PROJECT_DIR",
            "CLAUDECODE",
            "HOME",
            "PATH",
        ]
        .map(String::from);
        let cmd = board_command(Path::new("brood"), keys);
        let removed: Vec<&OsStr> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k)
            .collect();
        for k in [
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_PID",
            "CLAUDE_PROJECT_DIR",
            "CLAUDECODE",
        ] {
            assert!(removed.contains(&OsStr::new(k)), "{k} not removed");
        }
        assert!(!removed.contains(&OsStr::new("HOME")));
        assert!(!removed.contains(&OsStr::new("PATH")));
        // Explicit even when the caller's env list is empty.
        let cmd = board_command(Path::new("brood"), Vec::<String>::new());
        assert!(
            cmd.get_envs()
                .any(|(k, v)| k == "CLAUDE_PID" && v.is_none())
        );
    }

    #[test]
    fn hmac_known_answer() {
        // RFC 4231 test case 2.
        assert_eq!(
            proof("Jefe", "what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    /// A server that answers health with a bogus proof and records everything it receives.
    #[test]
    fn impostor_never_sees_the_token() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(String::new()));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for mut s in l.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                seen2
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
                let body = r#"{"ok":true,"name":"brood-board","proof":"deadbeef"}"#;
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        let conn = Conn {
            port,
            token: "SECRET-TOKEN-VALUE".into(),
        };
        assert!(!verify(port, &conn.token));
        assert!(conn.request("GET", "/api/projects", None).is_err());
        let got = seen.lock().unwrap().clone();
        assert!(got.contains("/api/health?nonce="));
        assert!(!got.contains("SECRET-TOKEN-VALUE"), "token leaked: {got}");
    }
}
