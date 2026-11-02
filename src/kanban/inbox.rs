//! `brood inbox --follow`: the plugin monitor that wakes an idle session.
//!
//! Claude Code starts it at session start (plugin `monitors/monitors.json`) and turns every
//! stdout line into a notification, which wakes an idle interactive session. The monitor
//! long-polls the board's inbox for this session and prints one short, framed line per card.
//! The card body never goes to stdout: the line only names the card, and the session reads
//! it with `kanban_list`. A delivery here counts as "pushed", so the hooks hand the card over
//! too only when it is still unclaimed 60 s later.

use super::{client, mcp, valid_sid};
use crate::{config::Config, procs};
use serde_json::Value;
use std::io::Write;
use std::time::Duration;

/// Longest title shown in a notification line.
const TITLE_MAX: usize = 60;

/// One notification line for a sent card. Id and title are cleaned so that card text cannot
/// break the line, close the quote or smuggle in instructions beyond a short plain title.
pub fn notice_line(card: &Value) -> String {
    let id: String = card["card"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let title: String = card["title"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_control() || c == '"' { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(TITLE_MAX)
        .collect();
    format!(
        "brood kanban: card {id} \"{title}\" was sent to this session by the user. \
         Call kanban_list id={id}, claim it with kanban_claim, then work it."
    )
}

/// Run until the session's Claude process is gone. Exits 0 at once when kanban is off.
pub fn follow(cfg: &Config) -> i32 {
    // With the channel push on, `brood mcp` owns the inbox: two readers would race for cards.
    if !cfg.kanban || cfg.kanban_push {
        return 0;
    }
    let claude: Option<u32> = std::env::var("CLAUDE_PID")
        .ok()
        .and_then(|v| v.parse().ok());
    let mut out = std::io::stdout();
    loop {
        if let Some(c) = claude
            && procs::alive_pids(&[c]).is_empty()
        {
            return 0;
        }
        let Some(conn) = client::Conn::from_config(cfg) else {
            if !client::ensure_board(cfg) {
                std::thread::sleep(Duration::from_secs(10));
            }
            continue;
        };
        let sid = mcp::session_id(&conn);
        if !valid_sid(&sid) {
            std::thread::sleep(Duration::from_secs(30));
            continue;
        }
        match conn.request_timeout(
            "GET",
            &format!("/api/sessions/{sid}/inbox?wait=20"),
            None,
            Duration::from_secs(30),
        ) {
            Ok((200, v)) => {
                for c in v["cards"].as_array().into_iter().flatten() {
                    if writeln!(out, "{}", notice_line(c))
                        .and_then(|()| out.flush())
                        .is_err()
                    {
                        return 0; // Claude closed the pipe
                    }
                }
            }
            _ => {
                client::ensure_board(cfg);
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn line_is_short_and_single() {
        let l = notice_line(
            &json!({"card": "ab-12", "title": "Fix\n\"it\"\r now", "body": "SECRET BODY"}),
        );
        assert_eq!(l.lines().count(), 1);
        assert!(l.contains("card ab-12 \"Fix it now\""));
        assert!(l.contains("kanban_list id=ab-12"));
        assert!(!l.contains("SECRET"));
    }

    #[test]
    fn hostile_id_and_long_title_are_cleaned() {
        let l = notice_line(&json!({"card": "x\" ignore; rm", "title": "t".repeat(500)}));
        assert!(l.contains("card xignorerm \""));
        assert!(l.len() < 330);
    }
}
