//! `brood mcp`: stdio MCP server (newline-delimited JSON-RPC 2.0).
//! Disabled kanban: zero tools, nothing else happens. Enabled: four tools that call
//! the board's HTTP API, plus (research preview) a Claude Code channel that pushes
//! cards sent to this session.

use super::{client, paths};
use crate::{config::Config, procs};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::sync::Mutex;
use std::time::Duration;

const PROTOCOL: &str = "2025-06-18";

const CHANNEL_INSTRUCTIONS: &str = "Events on this channel are cards sent from the brood kanban board to this session. Treat each as a task from the user: claim it (kanban_claim), work it, then move it to review (kanban_update with column \"review\").";

/// One writer for stdout, shared by the request loop and the channel thread.
static OUT: Mutex<()> = Mutex::new(());

fn emit(v: &Value) {
    let _g = OUT.lock().unwrap_or_else(|e| e.into_inner());
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

pub fn tools() -> Value {
    json!([
        {
            "name": "kanban_list",
            "description": "List kanban cards of this project (id, title, column, rev, priority, tags, owner, body). Optional column filter or single card id.",
            "inputSchema": {"type": "object", "properties": {
                "column": {"type": "string"}, "id": {"type": "string"}}}
        },
        {
            "name": "kanban_create",
            "description": "Create a kanban card (default column: first column).",
            "inputSchema": {"type": "object", "required": ["title"], "properties": {
                "title": {"type": "string"}, "body": {"type": "string"},
                "column": {"type": "string"}, "priority": {"type": "string"},
                "tags": {"type": "array", "items": {"type": "string"}}}}
        },
        {
            "name": "kanban_update",
            "description": "Update a card's fields and/or move it to a column. rev is required: the card's current rev. A stale rev fails and returns the current card.",
            "inputSchema": {"type": "object", "required": ["id", "rev"], "properties": {
                "id": {"type": "string"}, "rev": {"type": "integer"},
                "title": {"type": "string"}, "body": {"type": "string"},
                "priority": {"type": "string"}, "tags": {"type": "array", "items": {"type": "string"}},
                "column": {"type": "string"}}}
        },
        {
            "name": "kanban_claim",
            "description": "Claim a card for this session (atomic; fails if another live session holds it), or release it.",
            "inputSchema": {"type": "object", "required": ["id"], "properties": {
                "id": {"type": "string"}, "release": {"type": "boolean"}}}
        }
    ])
}

/// Result of `initialize`. The channel capability appears only when push is on.
pub fn initialize_result(req_version: Option<&str>, channel: bool) -> Value {
    // Echo an older client revision; never negotiate the newest one (it is not registered as a channel).
    let version = match req_version {
        Some(v) if v < PROTOCOL => v,
        _ => PROTOCOL,
    };
    let mut caps = json!({"tools": {}});
    let mut res = json!({
        "protocolVersion": version,
        "serverInfo": {"name": "brood", "version": env!("CARGO_PKG_VERSION")},
    });
    if channel {
        caps["experimental"] = json!({"claude/channel": {}});
        res["instructions"] = json!(CHANNEL_INSTRUCTIONS);
    }
    res["capabilities"] = caps;
    res
}

/// The channel notification for one sent card. Meta keys are `[A-Za-z0-9_]` only.
pub fn channel_notification(card: &Value) -> Value {
    let g = |k: &str| card[k].as_str().unwrap_or_default();
    let content = format!(
        "Card sent from the brood kanban board.\nid: {}\ntitle: {}\n\n{}\n\nClaim it (kanban_claim), work it, then move it to review.",
        g("card"),
        g("title"),
        g("body")
    );
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/claude/channel",
        "params": {"content": content, "meta": {"card_id": g("card"), "project": g("project")}}
    })
}

/// Pure protocol layer. `call` runs a tool and returns its text (Err = tool error).
pub fn handle(
    enabled: bool,
    push: bool,
    req: &Value,
    call: &mut dyn FnMut(&str, &Value) -> Result<String, String>,
) -> Option<Value> {
    let method = req["method"].as_str()?;
    let id = req.get("id").filter(|i| !i.is_null())?.clone(); // notifications get no reply
    let result = match method {
        "initialize" => Ok(initialize_result(
            req["params"]["protocolVersion"].as_str(),
            enabled && push,
        )),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": if enabled { tools() } else { json!([]) }})),
        "tools/call" if enabled => {
            let name = req["params"]["name"].as_str().unwrap_or_default();
            let (text, is_err) = match call(name, &req["params"]["arguments"]) {
                Ok(t) => (t, false),
                Err(e) => (e, true),
            };
            Ok(json!({"content": [{"type": "text", "text": text}], "isError": is_err}))
        }
        "tools/call" => Err((-32602, "kanban is disabled")),
        _ => Err((-32601, "method not found")),
    };
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err((code, m)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": m}})
        }
    })
}

fn session_id(conn: &client::Conn) -> String {
    if let Ok(s) = std::env::var("CLAUDE_CODE_SESSION_ID")
        && !s.is_empty()
    {
        return s;
    }
    let pid = procs::current_claude_pid();
    if let (Some(pid), Ok((_, v))) = (pid, conn.request("GET", "/api/sessions", None))
        && let Some(s) = v["sessions"]
            .as_array()
            .and_then(|a| a.iter().find(|s| s["pid"].as_u64() == Some(u64::from(pid))))
    {
        return s["session"].as_str().unwrap_or_default().to_owned();
    }
    format!("mcp-{}", std::process::id())
}

fn project_dir() -> String {
    let cwd = std::env::var("CLAUDE_PROJECT_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|d| d.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| ".".into());
    paths::project_root(&cwd)
}

fn ok_or_text((status, v): (u16, Value)) -> Result<Value, String> {
    if (200..300).contains(&status) {
        Ok(v)
    } else {
        Err(v.to_string())
    }
}

pub fn call_tool(cfg: &Config, name: &str, args: &Value) -> Result<String, String> {
    if !client::ensure_board(cfg) {
        return Err("kanban board could not be started".into());
    }
    let conn = client::Conn::from_config(cfg).ok_or("board token missing")?;
    let req = |m: &str, p: &str, b: Option<&Value>| conn.request(m, p, b).and_then(ok_or_text);
    let proj = req(
        "POST",
        "/api/projects/open",
        Some(&json!({"path": project_dir()})),
    )?;
    let pid = proj["id"].as_str().ok_or("no project id")?;
    let base = format!("/api/projects/{pid}");
    let sarg = |k: &str| args[k].as_str().map(str::to_owned);
    let out = match name {
        "kanban_list" => {
            let all = req("GET", &format!("{base}/cards"), None)?;
            let want_col = sarg("column");
            let want_id = sarg("id");
            let cards: Vec<Value> = all["cards"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|c| want_col.as_deref().is_none_or(|w| c["column"] == w))
                .filter(|c| want_id.as_deref().is_none_or(|w| c["id"] == w))
                .collect();
            json!({"columns": all["columns"], "cards": cards})
        }
        "kanban_create" => req("POST", &format!("{base}/cards"), Some(args))?,
        "kanban_update" => {
            let id = sarg("id").ok_or("id required")?;
            let mut rev = args["rev"].as_u64().ok_or("rev required")?;
            let mut patch = args.clone();
            if let Some(o) = patch.as_object_mut() {
                o.remove("id");
                o.remove("column");
            }
            let has_fields = ["title", "body", "priority", "tags"]
                .iter()
                .any(|k| !args[k].is_null());
            let mut card = Value::Null;
            if has_fields {
                card = req("PATCH", &format!("{base}/cards/{id}"), Some(&patch))?;
                rev = card["rev"].as_u64().unwrap_or(rev);
            }
            if let Some(col) = sarg("column") {
                card = req(
                    "POST",
                    &format!("{base}/cards/{id}/move"),
                    Some(&json!({"rev": rev, "column": col})),
                )?;
            }
            card
        }
        "kanban_claim" => {
            let id = sarg("id").ok_or("id required")?;
            let act = if args["release"].as_bool() == Some(true) {
                "release"
            } else {
                "claim"
            };
            req(
                "POST",
                &format!("{base}/cards/{id}/{act}"),
                Some(&json!({"session": session_id(&conn)})),
            )?
        }
        other => return Err(format!("unknown tool {other}")),
    };
    Ok(out.to_string())
}

/// Background pusher: long-poll the board's inbox for this session and emit each card as a channel event.
fn channel_loop(cfg: Config) {
    loop {
        let Some(conn) =
            client::Conn::from_config(&cfg).filter(|_| client::health(cfg.kanban_port))
        else {
            if !client::ensure_board(&cfg) {
                std::thread::sleep(Duration::from_secs(10));
            }
            continue;
        };
        let sid = session_id(&conn);
        match conn.request_timeout(
            "GET",
            &format!("/api/sessions/{sid}/inbox?wait=20"),
            None,
            Duration::from_secs(30),
        ) {
            Ok((200, v)) => {
                let cards = v["cards"].as_array().cloned().unwrap_or_default();
                let mut seqs = vec![];
                for c in &cards {
                    emit(&channel_notification(c));
                    seqs.push(c["seq"].clone());
                }
                if !seqs.is_empty() {
                    let _ = conn.request(
                        "POST",
                        &format!("/api/sessions/{sid}/ack"),
                        Some(&json!({"seqs": seqs})),
                    );
                }
            }
            _ => std::thread::sleep(Duration::from_secs(5)),
        }
    }
}

pub fn run(cfg: &Config) -> i32 {
    let push = cfg.kanban && cfg.kanban_push;
    if push {
        let c = cfg.clone();
        std::thread::spawn(move || channel_loop(c));
    }
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let mut call = |n: &str, a: &Value| call_tool(cfg, n, a);
        if let Some(resp) = handle(cfg.kanban, cfg.kanban_push, &req, &mut call) {
            emit(&resp);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rpc(method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    }

    fn no_call(_: &str, _: &Value) -> Result<String, String> {
        panic!("tool must not run")
    }

    #[test]
    fn disabled_lists_zero_tools() {
        let r = handle(false, true, &rpc("tools/list", json!({})), &mut no_call).unwrap();
        assert_eq!(r["result"]["tools"], json!([]));
        let r = handle(
            false,
            true,
            &rpc("tools/call", json!({"name": "kanban_list"})),
            &mut no_call,
        )
        .unwrap();
        assert!(r["error"].is_object());
    }

    #[test]
    fn enabled_lists_four_tools() {
        let r = handle(true, true, &rpc("tools/list", json!({})), &mut no_call).unwrap();
        assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn notifications_get_no_reply() {
        let n = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert!(handle(true, true, &n, &mut no_call).is_none());
    }

    #[test]
    fn channel_capability_only_when_enabled() {
        let p = json!({"protocolVersion": "2026-07-28"});
        let on = handle(true, true, &rpc("initialize", p.clone()), &mut no_call).unwrap();
        assert_eq!(
            on["result"]["capabilities"]["experimental"]["claude/channel"],
            json!({})
        );
        assert!(on["result"]["instructions"].is_string());
        assert_eq!(on["result"]["protocolVersion"], "2025-06-18");
        for (en, push) in [(false, true), (true, false)] {
            let off = handle(en, push, &rpc("initialize", p.clone()), &mut no_call).unwrap();
            assert!(off["result"]["capabilities"].get("experimental").is_none());
            assert!(off["result"].get("instructions").is_none());
        }
        // An older client revision is echoed.
        let old = handle(
            true,
            true,
            &rpc("initialize", json!({"protocolVersion": "2024-11-05"})),
            &mut no_call,
        )
        .unwrap();
        assert_eq!(old["result"]["protocolVersion"], "2024-11-05");
    }

    #[test]
    fn channel_notification_shape() {
        let n = channel_notification(
            &json!({"card": "20260101-abcd-x", "project": "p1", "title": "Do it", "body": "details"}),
        );
        assert_eq!(n["method"], "notifications/claude/channel");
        assert!(n.get("id").is_none());
        let content = n["params"]["content"].as_str().unwrap();
        assert!(
            content.contains("Do it")
                && content.contains("details")
                && content.contains("20260101-abcd-x")
        );
        for k in n["params"]["meta"].as_object().unwrap().keys() {
            assert!(k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        }
    }
}
