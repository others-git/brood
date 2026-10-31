//! Per-session state under `<data dir>/brood/sessions/<session_id>/`.
//!
//! Hooks for parallel subagents run concurrently, so state is split per agent
//! (one small file each) and the command ledger is append-only.

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

const KEEP_SESSIONS_FOR: Duration = Duration::from_secs(7 * 24 * 3600);

pub fn root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("brood")
}

pub fn session_dir(session: &str) -> PathBuf {
    // Session ids are UUIDs; strip anything else so the id can't escape the dir.
    let safe: String = session
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    root().join("sessions").join(safe)
}

fn agent_file(session: &str, agent: &str) -> PathBuf {
    let safe: String = agent
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    session_dir(session)
        .join("agents")
        .join(format!("{safe}.json"))
}

/// What brood remembers about one agent (or `main`) within a session.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentState {
    pub last_command: String,
    pub streak: u32,
    pub cleanup_blocks: u32,
    pub helpers_spawned: u32,
    /// Main session only: work tool calls since the user's last message.
    pub turn_calls: u32,
    /// Main session only: the user said `#solo`, so this turn may work inline.
    pub solo: bool,
    /// Main session only: files edited since the user's last message.
    pub turn_files: Vec<String>,
}

pub fn load_agent(session: &str, agent: &str) -> AgentState {
    fs::read_to_string(agent_file(session, agent))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_agent(session: &str, agent: &str, st: &AgentState) {
    let path = agent_file(session, agent);
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(st) {
        let _ = fs::write(path, json);
    }
}

/// One Bash command an agent ran, used to attribute processes back to agents.
#[derive(Serialize, Deserialize)]
pub struct LedgerEntry {
    pub agent: String,
    pub command: String,
}

pub fn record_command(session: &str, agent: &str, command: &str) {
    let entry = LedgerEntry {
        agent: agent.into(),
        command: command.into(),
    };
    if let Ok(line) = serde_json::to_string(&entry) {
        append(session_dir(session).join("ledger.jsonl"), &line);
    }
}

pub fn commands_of(session: &str, agent: &str) -> Vec<String> {
    let Ok(text) = fs::read_to_string(session_dir(session).join("ledger.jsonl")) else {
        return vec![];
    };
    text.lines()
        .filter_map(|l| l.split_once(' ').map(|(_, json)| json))
        .filter_map(|l| serde_json::from_str::<LedgerEntry>(l).ok())
        .filter(|e| e.agent == agent)
        .map(|e| e.command)
        .collect()
}

/// Notes for the coordinator, delivered on its next prompt and then cleared.
pub fn push_notice(session: &str, msg: &str) {
    append(session_dir(session).join("notices.txt"), msg);
}

pub fn take_notices(session: &str) -> Vec<String> {
    let path = session_dir(session).join("notices.txt");
    let Ok(text) = fs::read_to_string(&path) else {
        return vec![];
    };
    let _ = fs::remove_file(path);
    // Drop the timestamp `append` adds.
    text.lines()
        .map(|l| l.split_once(' ').map_or(l, |(_, m)| m).to_owned())
        .collect()
}

pub fn log(session: &str, msg: &str) {
    append(session_dir(session).join("brood.log"), msg);
}

pub fn log_global(msg: &str) {
    append(root().join("brood.log"), msg);
}

fn append(path: PathBuf, line: &str) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let ts = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "{ts} {line}");
    }
}

/// Delete state for sessions untouched for a week.
pub fn prune_old_sessions() {
    let Ok(entries) = fs::read_dir(root().join("sessions")) else {
        return;
    };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let stale = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > KEEP_SESSIONS_FOR);
        if stale {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}
