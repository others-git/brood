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

/// Boards in tests bind port 0, so the OS picks a free port and concurrent test runs
/// can never share one. This reads the bound port back from the board's pid file.
fn board_port(data: &std::path::Path) -> Option<u16> {
    std::fs::read_to_string(data.join("board.pid"))
        .ok()?
        .lines()
        .nth(1)?
        .trim()
        .parse()
        .ok()
        .filter(|p| *p != 0)
}

/// Wait until the board in `data` is bound and healthy; returns its port.
fn wait_board(data: &std::path::Path, child: &mut Child) -> u16 {
    for _ in 0..100 {
        if let Some(st) = child.try_wait().unwrap() {
            panic!("board exited early: {st}");
        }
        if let Some(p) = board_port(data)
            && wait_health(p)
        {
            return p;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("board did not start");
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

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_brood"));
    cmd.arg("board")
        .env("BROOD_DATA_DIR", &data)
        .env("BROOD_KANBAN_PORT", "0")
        .env("XDG_CONFIG_HOME", base.join("cfg"))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut board = Board(cmd.spawn().unwrap());
    let port = wait_board(&data, &mut board.0);
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
        .env("BROOD_KANBAN_PORT", "0")
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
    let ver1 = card["ver"].as_str().unwrap().to_owned();
    assert_eq!(card["rev"], 1);
    assert_eq!(card["column"], "backlog");

    let (st, body) = http(
        port,
        "POST",
        &format!("/api/projects/{pid}/cards/{id}/move"),
        t,
        h,
        &format!(r#"{{"rev":1,"ver":"{ver1}","column":"in-progress"}}"#),
    );
    assert_eq!(st, 200, "{body}");
    let ver2 = serde_json::from_str::<serde_json::Value>(&body).unwrap()["ver"]
        .as_str()
        .unwrap()
        .to_owned();
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
        &format!(r#"{{"rev":1,"ver":"{ver1}","title":"stale"}}"#),
    );
    assert_eq!(st, 409, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["card"]["rev"], 2);
    assert_eq!(v["card"]["column"], "in-progress");

    // A hand edit that leaves rev alone: the next write with the old ver is a 409 and the edit survives.
    let file = project.join("kanban/in-progress").join(format!("{id}.md"));
    let text = std::fs::read_to_string(&file)
        .unwrap()
        .replace("hi", "edited by hand");
    std::fs::write(&file, text).unwrap();
    let (st, body) = http(
        port,
        "PATCH",
        &format!("/api/projects/{pid}/cards/{id}"),
        t,
        h,
        &format!(r#"{{"rev":2,"ver":"{ver2}","title":"clobber"}}"#),
    );
    assert_eq!(st, 409, "{body}");
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("edited by hand")
    );
    assert_eq!(
        http(
            port,
            "PATCH",
            &format!("/api/projects/{pid}/cards/{id}"),
            t,
            h,
            r#"{"rev":2,"title":"no ver"}"#
        )
        .0,
        400
    );
    // The token in the URL is for the page and the event stream only.
    assert_eq!(
        http(port, "GET", &format!("/api/projects?token={t}"), "", h, "").0,
        401
    );

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

    // Send an unclaimed card, then take (hook path). A claimed card is not delivered.
    let (_, c2) = http(
        port,
        "POST",
        &format!("/api/projects/{pid}/cards"),
        t,
        h,
        r#"{"title":"second"}"#,
    );
    let id2 = serde_json::from_str::<serde_json::Value>(&c2).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
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
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["cards"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let (st, _) = http(
        port,
        "POST",
        &format!("/api/projects/{pid}/cards/{id2}/send"),
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

    // Create-with-claim: working columns claim for the creating session, backlog does not,
    // and moving into done releases the claim.
    let mk = |extra: &str| {
        let (st, b) = http(
            port,
            "POST",
            &format!("/api/projects/{pid}/cards"),
            t,
            h,
            &format!(r#"{{"title":"mine","session":"sess-a"{extra}}}"#),
        );
        assert_eq!(st, 201, "{b}");
        serde_json::from_str::<serde_json::Value>(&b).unwrap()
    };
    let working = mk(r#","column":"in-progress""#);
    assert_eq!(working["owner"], "sess-a");
    assert_eq!(mk("")["owner"], "");
    assert_eq!(mk(r#","column":"in-progress","claim":false"#)["owner"], "");
    assert_eq!(mk(r#","column":"ready","claim":true"#)["owner"], "sess-a");
    let wid = working["id"].as_str().unwrap();
    // The lock is real: another session cannot claim it.
    let (st, _) = http(
        port,
        "POST",
        &format!("/api/projects/{pid}/cards/{wid}/claim"),
        t,
        h,
        r#"{"session":"sess-b"}"#,
    );
    assert_eq!(st, 409);
    let (st, b) = http(
        port,
        "POST",
        &format!("/api/projects/{pid}/cards/{wid}/move"),
        t,
        h,
        &format!(
            r#"{{"rev":{},"ver":"{}","column":"done"}}"#,
            working["rev"],
            working["ver"].as_str().unwrap()
        ),
    );
    assert_eq!(st, 200, "{b}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&b).unwrap()["owner"],
        ""
    );
    let (st, _) = http(
        port,
        "POST",
        &format!("/api/projects/{pid}/cards/{wid}/claim"),
        t,
        h,
        r#"{"session":"sess-b"}"#,
    );
    assert_eq!(st, 200);

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

#[test]
fn two_starters_make_one_board() {
    let base = std::env::temp_dir().join(format!("brood-it2-{}", std::process::id()));
    let data = base.join("data");
    let start = || {
        Command::new(env!("CARGO_BIN_EXE_brood"))
            .arg("board")
            .env("BROOD_DATA_DIR", &data)
            .env("BROOD_KANBAN_PORT", "0")
            .env("XDG_CONFIG_HOME", base.join("cfg"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut kids: Vec<Board> = (0..3).map(|_| Board(start())).collect();
    std::thread::sleep(Duration::from_millis(2500));
    let mut running = 0;
    for k in &mut kids {
        match k.0.try_wait().unwrap() {
            None => running += 1,
            Some(st) => assert!(st.success(), "loser must exit 0"),
        }
    }
    assert_eq!(running, 1, "exactly one board may run");
    let port = board_port(&data).expect("survivor records its port");
    // The survivor answers a challenge only the token holder can answer.
    let (st, body) = http(port, "GET", "/api/health?nonce=abcd", "", "127.0.0.1", "");
    assert_eq!(st, 200);
    assert!(body.contains("\"proof\""), "{body}");
}

/// One health probe that treats any I/O trouble as "not up yet".
fn wait_health(port: u16) -> bool {
    let probe = || -> std::io::Result<bool> {
        let mut s = TcpStream::connect_timeout(
            &SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_secs(1),
        )?;
        s.set_read_timeout(Some(Duration::from_secs(2)))?;
        s.write_all(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")?;
        let mut out = String::new();
        s.read_to_string(&mut out)?;
        Ok(out.starts_with("HTTP/1.1 200"))
    };
    probe().unwrap_or(false)
}

fn raw(port: u16, req: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn token_persists_and_cookie_logs_in() {
    let base = std::env::temp_dir().join(format!("brood-it3-{}", std::process::id()));
    let data = base.join("data");
    let brood = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_brood"));
        c.args(args)
            .env("BROOD_DATA_DIR", &data)
            .env("BROOD_KANBAN_PORT", "0")
            .env("XDG_CONFIG_HOME", base.join("cfg"));
        c
    };
    let start = || {
        let mut b = Board(
            brood(&["board"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let port = wait_board(&data, &mut b.0);
        (b, port)
    };
    let token_file = data.join("board.token");

    let (b1, port) = start();
    let t1 = std::fs::read_to_string(&token_file).unwrap();
    let t1 = t1.trim().to_owned();
    // First visit with ?token= sets the cookie and redirects to the bare URL.
    let r = raw(
        port,
        &format!("GET /?token={t1} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"),
    );
    assert!(r.starts_with("HTTP/1.1 302"), "{r}");
    assert!(
        r.contains(&format!(
            "Set-Cookie: brood_token={t1}; HttpOnly; SameSite=Strict; Path=/"
        )),
        "{r}"
    );
    assert!(r.contains("Location: /\r\n"));
    // The cookie alone opens the page and the API; a wrong one does not.
    let with = |c: &str, path: &str| {
        raw(
            port,
            &format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: brood_token={c}\r\nConnection: close\r\n\r\n"
            ),
        )
    };
    assert!(with(&t1, "/").starts_with("HTTP/1.1 200"));
    assert!(with(&t1, "/api/projects").starts_with("HTTP/1.1 200"));
    assert!(with("nope", "/api/projects").starts_with("HTTP/1.1 401"));
    // A foreign Origin is refused even with the cookie.
    let r = raw(
        port,
        &format!(
            "GET /api/projects HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://evil.example\r\nCookie: brood_token={t1}\r\nConnection: close\r\n\r\n"
        ),
    );
    assert!(r.starts_with("HTTP/1.1 403"), "{r}");
    assert_eq!(
        http(port, "POST", "/api/shutdown", &t1, "127.0.0.1", "").0,
        200
    );
    drop(b1);
    std::thread::sleep(Duration::from_millis(300));

    // A restart keeps the token.
    let (b2, port2) = start();
    assert_eq!(std::fs::read_to_string(&token_file).unwrap().trim(), t1);
    // --url names the port the running board actually bound.
    let out = brood(&["board", "--url"]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("http://127.0.0.1:{port2}/?token={t1}")
    );
    drop(b2);

    // Rotation makes a new one.
    assert!(
        brood(&["board", "--rotate-token"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_ne!(std::fs::read_to_string(&token_file).unwrap().trim(), t1);
}
