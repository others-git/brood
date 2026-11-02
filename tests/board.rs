//! Starts a real `brood board` on a random port with a temp data dir and drives the API.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn http(port: u16, method: &str, path: &str, token: &str, host: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(2),
    )
    .unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nX-Brood-Token: {token}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let (head, payload) = out.split_once("\r\n\r\n").unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, payload.to_owned())
}

struct Board(Child);
impl Drop for Board {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn board_lifecycle_over_http() {
    let base = std::env::temp_dir().join(format!("brood-it-{}", std::process::id()));
    let data = base.join("data");
    let project = base.join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_brood"));
    cmd.arg("board")
        .env("BROOD_DATA_DIR", &data)
        .env("BROOD_KANBAN_PORT", port.to_string())
        .env("XDG_CONFIG_HOME", base.join("cfg"))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let board = Board(cmd.spawn().unwrap());

    let mut up = false;
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok()
            && http(port, "GET", "/api/health", "", "127.0.0.1", "").0 == 200
        {
            up = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(up, "board did not start");
    let token = std::fs::read_to_string(data.join("board.token")).unwrap();
    let t = token.trim();
    let h = "127.0.0.1";

    // Auth and Host checks.
    assert_eq!(http(port, "GET", "/api/projects", "wrong", h, "").0, 401);
    assert_eq!(
        http(port, "GET", "/api/projects", t, "evil.example", "").0,
        403
    );

    // A second board exits 0 while the first is live.
    let second = Command::new(env!("CARGO_BIN_EXE_brood"))
        .arg("board")
        .env("BROOD_DATA_DIR", &data)
        .env("BROOD_KANBAN_PORT", port.to_string())
        .env("XDG_CONFIG_HOME", base.join("cfg"))
        .status()
        .unwrap();
    assert!(second.success());

    let path = project.to_string_lossy().replace('\\', "\\\\");
    let (st, body) = http(
        port,
        "POST",
        "/api/projects/open",
        t,
        h,
        &format!(r#"{{"path":"{path}"}}"#),
    );
    assert_eq!(st, 200, "{body}");
    let pid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (st, body) = http(
        port,
        "POST",
        &format!("/api/projects/{pid}/cards"),
        t,
        h,
        r#"{"title":"First card","body":"hi"}"#,
    );
    assert_eq!(st, 201, "{body}");
    let card: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = card["id"].as_str().unwrap().to_owned();
    assert_eq!(card["rev"], 1);
    assert_eq!(card["column"], "backlog");

    let (st, body) = http(
        port,
        "POST",
        &format!("/api/projects/{pid}/cards/{id}/move"),
        t,
        h,
        r#"{"rev":1,"column":"in-progress"}"#,
    );
    assert_eq!(st, 200, "{body}");
    assert!(
        project
            .join("kanban/in-progress")
            .join(format!("{id}.md"))
            .exists()
    );

    // Stale rev: 409 with the current card.
    let (st, body) = http(
        port,
        "PATCH",
        &format!("/api/projects/{pid}/cards/{id}"),
        t,
        h,
        r#"{"rev":1,"title":"stale"}"#,
    );
    assert_eq!(st, 409, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["card"]["rev"], 2);
    assert_eq!(v["card"]["column"], "in-progress");

    // Claim: first session wins, a second live one is refused.
    for s in ["sess-a", "sess-b"] {
        let reg = format!(
            r#"{{"session":"{s}","path":"{path}","pid":{},"cwd":"{path}"}}"#,
            std::process::id()
        );
        assert_eq!(
            http(port, "POST", "/api/sessions/register", t, h, &reg).0,
            200
        );
    }
    let claim = |s: &str| {
        http(
            port,
            "POST",
            &format!("/api/projects/{pid}/cards/{id}/claim"),
            t,
            h,
            &format!(r#"{{"session":"{s}"}}"#),
        )
        .0
    };
    assert_eq!(claim("sess-a"), 200);
    assert_eq!(claim("sess-b"), 409);

    // Send, then take (hook path).
    let (st, _) = http(
        port,
        "POST",
        &format!("/api/projects/{pid}/cards/{id}/send"),
        t,
        h,
        r#"{"session":"sess-b"}"#,
    );
    assert_eq!(st, 200);
    let (_, body) = http(
        port,
        "POST",
        "/api/sessions/sess-b/take",
        t,
        h,
        r#"{"max":5}"#,
    );
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["cards"].as_array().unwrap().len(), 1);
    let (_, body) = http(
        port,
        "POST",
        "/api/sessions/sess-b/take",
        t,
        h,
        r#"{"max":5}"#,
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["cards"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    // Shut down over the API.
    assert_eq!(http(port, "POST", "/api/shutdown", t, h, "").0, 200);
    let mut board = board;
    for _ in 0..50 {
        if board.0.try_wait().unwrap().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("board did not exit after shutdown");
}
