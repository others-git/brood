//! User config at `<config dir>/brood/config.toml`. Every field is optional.

use serde::Deserialize;

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    /// Inject the coordinator prompt at session start. Env `BROOD_COORDINATOR=0` also disables it.
    pub coordinator: bool,
    /// Deny a Bash command once the same agent has run it this many times in a row.
    pub loop_streak: u32,
    /// How many times SubagentStop may send an agent back to clean up before brood kills its processes itself.
    pub cleanup_retries: u32,
    /// Most helpers one subagent may spawn. The main session has no cap.
    pub max_helpers: u32,
    /// Work tool calls (read, search, edit, shell) per user message at which the
    /// coordinator gets a soft nudge to delegate. 0 = no nudge.
    pub main_tool_budget: u32,
    /// An edit larger than this many lines triggers the soft nudge. 0 = never.
    pub trivial_edit_lines: u32,
    /// Work tool calls per user message at which the coordinator gets a firm nudge. 0 = none.
    pub firm_nudge_after: u32,
    /// Regexes for shell commands that don't count toward the work budget
    /// (matched against each simple command). Commands made only of `git` are always free.
    pub free_commands: Vec<String>,
    /// Tools whose output is scanned for prompt injection (regex on tool name).
    pub scan_tools: String,
    /// Extra regexes for secrets that must never leave the machine.
    pub extra_secret_patterns: Vec<String>,
    /// Extra regexes for instruction-like text in fetched content.
    pub extra_injection_patterns: Vec<String>,
    /// Extra junk patterns (`name/` or `*.ext`) the worktree-remove guard ignores for untracked files.
    pub worktree_junk: Vec<String>,
    /// Enable the kanban board (board server, MCP tools, hook delivery). Env `BROOD_KANBAN=1/0` overrides.
    pub kanban: bool,
    /// Port of the per-machine board server (127.0.0.1 only). Env `BROOD_KANBAN_PORT` overrides.
    pub kanban_port: u16,
    /// With kanban on, `brood mcp` also pushes sent cards as Claude Code channel events (research preview).
    pub kanban_push: bool,
    /// Board columns, left to right. Each is a directory under `<project>/kanban/`.
    pub kanban_columns: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            coordinator: true,
            loop_streak: 4,
            cleanup_retries: 2,
            max_helpers: 3,
            main_tool_budget: 3,
            trivial_edit_lines: 30,
            firm_nudge_after: 8,
            free_commands: vec![],
            scan_tools: r"^(WebFetch|WebSearch|mcp__.*)$".into(),
            extra_secret_patterns: vec![],
            extra_injection_patterns: vec![],
            worktree_junk: vec![],
            kanban: false,
            kanban_port: 47777,
            kanban_push: true,
            kanban_columns: default_columns(),
        }
    }
}

pub fn default_columns() -> Vec<String> {
    ["backlog", "ready", "in-progress", "review", "done"]
        .map(String::from)
        .to_vec()
}

pub fn load() -> Config {
    let mut cfg: Config = dirs::config_dir()
        .map(|d| d.join("brood").join("config.toml"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default();
    if std::env::var("BROOD_COORDINATOR").is_ok_and(|v| v == "0" || v == "false") {
        cfg.coordinator = false;
    }
    match std::env::var("BROOD_KANBAN").as_deref() {
        Ok("1" | "true") => cfg.kanban = true,
        Ok("0" | "false") => cfg.kanban = false,
        _ => {}
    }
    if let Some(p) = std::env::var("BROOD_KANBAN_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        cfg.kanban_port = p;
    }
    // Column names are directory names: keep them plain.
    cfg.kanban_columns.retain(|c| {
        !c.is_empty()
            && c.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    });
    cfg.kanban_columns.dedup();
    if cfg.kanban_columns.is_empty() {
        cfg.kanban_columns = default_columns();
    }
    cfg
}
