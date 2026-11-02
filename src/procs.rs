//! Finding and killing processes that Claude Code sessions left behind.
//!
//! Claude Code puts `CLAUDE_CODE_SESSION_ID` and `CLAUDE_PID` in the
//! environment of everything it spawns, and that survives detaching
//! (`nohup`, `&`, reparenting to init). brood uses those tags to find a
//! session's processes; no command rewriting is needed.

use std::collections::{HashMap, HashSet};
use std::time::Duration;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, UpdateKind};

pub struct Proc {
    pub pid: u32,
    pub parent: Option<u32>,
    pub cmd: String,
    pub session: String,
    pub claude_pid: Option<u32>,
    pub start: u64,
}

struct Snapshot {
    sys: Option<System>,
    procs: HashMap<u32, Proc>,
}

fn env_value(env: &[std::ffi::OsString], key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    env.iter()
        .filter_map(|e| e.to_str())
        .find_map(|e| e.strip_prefix(prefix.as_str()).map(str::to_owned))
}

fn snapshot() -> Snapshot {
    let Ok(mut sys) = System::new() else {
        return Snapshot {
            sys: None,
            procs: HashMap::new(),
        };
    };
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_environ(UpdateKind::Always),
    );
    let mut procs = HashMap::new();
    for (pid, p) in sys.processes() {
        // Threads show up as processes on Linux; skip them.
        if p.thread_kind()
            .is_some_and(|k| k == sysinfo::ThreadKind::Userland)
        {
            continue;
        }
        let cmd = p
            .cmd()
            .iter()
            .map(|a| a.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        procs.insert(
            pid.as_u32(),
            Proc {
                pid: pid.as_u32(),
                parent: p.parent().map(|pp| pp.as_u32()),
                cmd,
                session: env_value(p.environ(), "CLAUDE_CODE_SESSION_ID").unwrap_or_default(),
                claude_pid: env_value(p.environ(), "CLAUDE_PID").and_then(|v| v.parse().ok()),
                start: p.start_time(),
            },
        );
    }
    Snapshot {
        sys: Some(sys),
        procs,
    }
}

impl Snapshot {
    fn ancestors(&self, pid: u32) -> Vec<u32> {
        let mut out = vec![];
        let mut cur = self.procs.get(&pid).and_then(|p| p.parent);
        while let Some(pp) = cur {
            if out.contains(&pp) || out.len() > 256 {
                break;
            }
            out.push(pp);
            cur = self.procs.get(&pp).and_then(|p| p.parent);
        }
        out
    }

    /// The Bash tool's wrapper shell this process runs under (itself included),
    /// looking no higher than `stop` (the session's Claude process). Without the
    /// stop, a session launched from another session's Bash tool would pick up
    /// the outer session's shell.
    fn tool_shell(&self, pid: u32, stop: Option<u32>) -> Option<&Proc> {
        std::iter::once(pid)
            .chain(self.ancestors(pid))
            .take_while(|p| Some(*p) != stop)
            .filter_map(|p| self.procs.get(&p))
            .find(|p| is_tool_shell(&p.cmd))
    }

    /// The Claude Code process that ran this hook: the nearest ancestor that looks like `claude`.
    fn session_claude(&self) -> Option<u32> {
        let me = std::process::id();
        let chain = self.ancestors(me);
        chain
            .iter()
            .copied()
            .find(|p| self.procs.get(p).is_some_and(|p| looks_like_claude(&p.cmd)))
            .or(chain.first().copied())
    }

    /// brood itself and the chain that launched it (Claude, shells). Never kill these.
    fn protected(&self) -> HashSet<u32> {
        self.protected_with(crate::kanban::board::lock_pid())
    }

    /// `protected`, plus the kanban board's pid: it is not a session leftover.
    fn protected_with(&self, board: Option<u32>) -> HashSet<u32> {
        let me = std::process::id();
        let mut set: HashSet<u32> = self.ancestors(me).into_iter().collect();
        set.insert(me);
        set.extend(board);
        set
    }

    fn claude_alive(&self, p: &Proc) -> bool {
        // A reused PID would have started after the process that recorded it.
        p.claude_pid
            .and_then(|c| self.procs.get(&c))
            .is_some_and(|c| c.start <= p.start)
    }
}

/// The Claude Code process above this one, if any.
pub fn current_claude_pid() -> Option<u32> {
    snapshot().session_claude()
}

/// Which of these pids are running now.
pub fn alive_pids(pids: &[u32]) -> HashSet<u32> {
    let Ok(mut sys) = System::new() else {
        return pids.iter().copied().collect(); // cannot tell: assume alive
    };
    let list: Vec<Pid> = pids.iter().map(|p| Pid::from_u32(*p)).collect();
    sys.refresh_processes(ProcessesToUpdate::Some(&list), true);
    pids.iter()
        .copied()
        .filter(|p| sys.process(Pid::from_u32(*p)).is_some())
        .collect()
}

/// The Bash tool runs commands as `bash -c "source .../shell-snapshots/snapshot-... && eval '<cmd>' && pwd -P ..."`.
pub fn is_tool_shell(cmd: &str) -> bool {
    cmd.contains("shell-snapshots/snapshot-") || cmd.contains("shell-snapshots\\snapshot-")
}

fn looks_like_claude(cmd: &str) -> bool {
    let exe = cmd.split_whitespace().next().unwrap_or_default();
    let name = exe
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    name == "claude" || name == "claude.exe"
}

/// The original command inside a tool shell's `eval '...'`, unescaped.
pub fn eval_command(shell_cmd: &str) -> Option<String> {
    let start = shell_cmd.find("&& eval '")? + "&& eval '".len();
    let rest = &shell_cmd[start..];
    let end = rest.rfind("' && pwd -P")?;
    Some(rest[..end].replace(r#"'"'"'"#, "'"))
}

/// Processes a still-running session started through its tools: anything
/// under one of its Bash tool shells, plus anything carrying its tag that is
/// no longer under its Claude process (detached). MCP servers and other
/// direct children of Claude are left alone.
fn leftovers<'a>(snap: &'a Snapshot, session: &str) -> Vec<&'a Proc> {
    let protected = snap.protected();
    let claude = snap.session_claude();
    snap.procs
        .values()
        .filter(|p| p.session == session && !protected.contains(&p.pid))
        .filter(|p| match claude {
            Some(c) if snap.ancestors(p.pid).contains(&c) => {
                snap.tool_shell(p.pid, Some(c)).is_some()
            }
            Some(_) => true,
            None => snap.tool_shell(p.pid, None).is_some(),
        })
        .collect()
}

/// Live processes started by Bash commands of one agent (exact command match).
pub fn agent_leftovers(session: &str, commands: &[String]) -> Vec<(u32, String)> {
    if commands.is_empty() {
        return vec![];
    }
    let snap = snapshot();
    let claude = snap.session_claude();
    leftovers(&snap, session)
        .into_iter()
        .filter(|p| {
            snap.tool_shell(p.pid, claude)
                .and_then(|sh| eval_command(&sh.cmd))
                .is_some_and(|c| commands.contains(&c))
        })
        .map(|p| (p.pid, p.cmd.clone()))
        .collect()
}

pub struct Killed {
    pub pid: u32,
    pub cmd: String,
}

pub fn kill_session(session: &str) -> Vec<Killed> {
    let snap = snapshot();
    let pids: Vec<u32> = leftovers(&snap, session).iter().map(|p| p.pid).collect();
    kill(snap, &pids)
}

pub fn kill_pids(pids: &[u32]) -> Vec<Killed> {
    kill(snapshot(), pids)
}

/// Kill processes whose Claude Code session is gone.
pub fn gc(dry_run: bool) -> Vec<Killed> {
    let snap = snapshot();
    let protected = snap.protected();
    let orphans: Vec<u32> = snap
        .procs
        .values()
        .filter(|p| !p.session.is_empty() && p.claude_pid.is_some())
        .filter(|p| !protected.contains(&p.pid) && !snap.claude_alive(p))
        .map(|p| p.pid)
        .collect();
    if dry_run {
        return orphans
            .iter()
            .filter_map(|pid| snap.procs.get(pid))
            .map(|p| Killed {
                pid: p.pid,
                cmd: p.cmd.clone(),
            })
            .collect();
    }
    kill(snap, &orphans)
}

pub struct SessionProc {
    pub session: String,
    pub pid: u32,
    pub cmd: String,
}

pub fn all_session_procs() -> Vec<SessionProc> {
    let snap = snapshot();
    let protected = snap.protected();
    let mut out: Vec<SessionProc> = snap
        .procs
        .values()
        .filter(|p| !p.session.is_empty() && !protected.contains(&p.pid))
        .map(|p| SessionProc {
            session: p.session.clone(),
            pid: p.pid,
            cmd: p.cmd.clone(),
        })
        .collect();
    out.sort_by(|a, b| a.session.cmp(&b.session).then(a.pid.cmp(&b.pid)));
    out
}

/// Terminate the given processes and their descendants, children first.
/// Sends a polite signal, waits briefly, then force-kills survivors.
fn kill(snap: Snapshot, roots: &[u32]) -> Vec<Killed> {
    let protected = snap.protected();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in snap.procs.values() {
        if let Some(pp) = p.parent {
            children.entry(pp).or_default().push(p.pid);
        }
    }
    // Depth-first so children are ordered before their parents.
    let mut order = vec![];
    let mut seen = HashSet::new();
    fn visit(pid: u32, ch: &HashMap<u32, Vec<u32>>, seen: &mut HashSet<u32>, out: &mut Vec<u32>) {
        if !seen.insert(pid) {
            return;
        }
        for c in ch.get(&pid).into_iter().flatten() {
            visit(*c, ch, seen, out);
        }
        out.push(pid);
    }
    for r in roots {
        visit(*r, &children, &mut seen, &mut order);
    }
    order.retain(|p| !protected.contains(p));

    let mut killed = vec![];
    for pid in &order {
        if let Some(p) = snap
            .sys
            .as_ref()
            .and_then(|s| s.process(Pid::from_u32(*pid)))
        {
            if p.kill_with(Signal::Term).is_none() {
                p.kill(); // Platform without SIGTERM (Windows): terminate outright.
            }
            if let Some(info) = snap.procs.get(pid) {
                killed.push(Killed {
                    pid: *pid,
                    cmd: info.cmd.clone(),
                });
            }
        }
    }
    if !order.is_empty() {
        std::thread::sleep(Duration::from_millis(400));
        let Ok(mut sys) = System::new() else {
            return killed;
        };
        sys.refresh_processes(ProcessesToUpdate::All, true);
        for pid in &order {
            if let Some(p) = sys.process(Pid::from_u32(*pid)) {
                p.kill();
            }
        }
    }
    killed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_tool_shell() {
        let cmd = "/bin/bash -c source /home/u/.claude/shell-snapshots/snapshot-bash-1-x.sh 2>/dev/null || true && eval 'sleep 100'";
        assert!(is_tool_shell(cmd));
        assert!(!is_tool_shell("node /usr/lib/mcp-server.js"));
    }

    #[test]
    fn eval_command_unescapes() {
        let shell = r#"/bin/bash -c source /h/.claude/shell-snapshots/snapshot-bash-1.sh 2>/dev/null || true && eval 'tr '"'"'\0'"'"' x && echo done' && pwd -P >| /tmp/claude-1-cwd"#;
        assert_eq!(
            eval_command(shell).as_deref(),
            Some(r"tr '\0' x && echo done")
        );
        assert_eq!(eval_command("node server.js"), None);
    }

    #[test]
    fn gc_protects_board_pid() {
        let snap = Snapshot {
            sys: None,
            procs: HashMap::new(),
        };
        assert!(snap.protected_with(Some(424242)).contains(&424242));
        assert!(!snap.protected_with(None).contains(&424242));
    }

    #[test]
    fn claude_detection() {
        assert!(looks_like_claude("claude -p hi"));
        assert!(looks_like_claude("/home/u/.local/bin/claude"));
        assert!(looks_like_claude(r"C:\Users\u\claude.exe --resume"));
        assert!(!looks_like_claude("node claude-helper.js"));
    }
}
