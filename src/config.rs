//! User config at `<config dir>/brood/config.toml`. Every field is optional.

use serde::Deserialize;

#[derive(Deserialize)]
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
    /// Work tool calls (read, search, edit, shell) the main session may make per
    /// user message in coordinator mode before it must delegate. 0 = no limit.
    pub main_tool_budget: u32,
    /// Largest edit (in lines) the main session may make itself in coordinator mode.
    pub trivial_edit_lines: u32,
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            coordinator: true,
            loop_streak: 4,
            cleanup_retries: 2,
            max_helpers: 3,
            main_tool_budget: 3,
            trivial_edit_lines: 10,
            free_commands: vec![],
            scan_tools: r"^(WebFetch|WebSearch|mcp__.*)$".into(),
            extra_secret_patterns: vec![],
            extra_injection_patterns: vec![],
            worktree_junk: vec![],
        }
    }
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
    cfg
}
