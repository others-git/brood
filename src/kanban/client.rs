//! Talking to the board: a tiny blocking HTTP/1.1 client, and starting the board.

use crate::{config::Config, state};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

pub fn token_path() -> std::path::PathBuf {
    state::root().join("board.token")
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

pub fn health(port: u16) -> bool {
    raw_request(
        port,
        "GET",
        "/api/health",
        None,
        None,
        Duration::from_secs(2),
    )
    .is_ok_and(|(s, b)| s == 200 && b.contains("brood-board"))
}

pub struct Conn {
    pub port: u16,
    pub token: String,
}

impl Conn {
    pub fn from_config(cfg: &Config) -> Option<Conn> {
        let token = std::fs::read_to_string(token_path())
            .ok()?
            .trim()
            .to_owned();
        (!token.is_empty()).then_some(Conn {
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

/// The command that starts a detached board. Every `CLAUDE_*` variable is removed:
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
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0000_0008 | 0x0800_0000); // DETACHED_PROCESS | CREATE_NO_WINDOW
    }
    c
}

/// Make sure a board answers on the configured port, starting one if needed.
pub fn ensure_board(cfg: &Config) -> bool {
    if health(cfg.kanban_port) {
        return true;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    if board_command(&exe, std::env::vars().map(|(k, _)| k))
        .spawn()
        .is_err()
    {
        return false;
    }
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(100));
        if health(cfg.kanban_port) {
            return true;
        }
    }
    false
}

/// Register this session with the board (hook, SessionStart).
pub fn register_session(cfg: &Config, session: &str, cwd: &str, root: &str, pid: u32) {
    if let Some(c) = Conn::from_config(cfg) {
        let _ = c.request(
            "POST",
            "/api/sessions/register",
            Some(&serde_json::json!({"session": session, "cwd": cwd, "path": root, "pid": pid})),
        );
    }
}

pub fn unregister_session(cfg: &Config, session: &str) {
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
    let Some(c) = Conn::from_config(cfg) else {
        return vec![];
    };
    match c.request(
        "POST",
        &format!("/api/sessions/{session}/take"),
        Some(&serde_json::json!({"max": max})),
    ) {
        Ok((200, v)) => v["cards"].as_array().cloned().unwrap_or_default(),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

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
}
