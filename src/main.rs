//! brood: the helper binary behind the brood Claude Code plugin.
//!
//! The plugin's hooks call `brood hook`, which reads the hook event JSON on
//! stdin and answers on stdout. `brood gc` sweeps leftovers by hand.

mod config;
mod guard;
mod hook;
mod kanban;
mod merge;
mod procs;
mod state;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "brood", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Handle a Claude Code hook event (JSON on stdin). Called by the plugin.
    Hook,
    /// Kill processes left behind by dead Claude Code sessions and prune old state.
    Gc {
        /// Only list what would be killed.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show live processes that belong to Claude Code sessions.
    Ps,
    /// Run the kanban board server (one per machine), or with --ensure start it if it is down.
    Board {
        /// Start a detached board if none answers, then exit.
        #[arg(long)]
        ensure: bool,
        /// Internal: the short-lived first stage that starts a board outside the caller's process tree.
        #[arg(long, hide = true)]
        detach: bool,
        /// Generate a new access token (the old URL and cookies stop working).
        #[arg(long)]
        rotate_token: bool,
        /// Print the board URL with its token.
        #[arg(long)]
        url: bool,
    },
    /// Stdio MCP server for the kanban tools (started by the plugin).
    Mcp,
    /// Merge a worktree agent's branch, optionally verify, then remove its worktree and branch.
    Merge {
        /// Branch name or worktree path.
        target: String,
        /// Shell command to run after the merge; cleanup happens only if it passes.
        #[arg(long)]
        verify: Option<String>,
        /// Session transcript path, for the live-agent check (default: env BROOD_TRANSCRIPT).
        #[arg(long, hide = true)]
        transcript: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        // Hooks fail open: a broken helper must never wedge the session.
        Cmd::Hook => {
            if let Err(e) = hook::run() {
                state::log_global(&format!("hook error: {e:#}"));
            }
        }
        Cmd::Gc { dry_run } => {
            let killed = procs::gc(dry_run);
            let verb = if dry_run { "would kill" } else { "killed" };
            for p in &killed {
                println!("{verb} {} {}", p.pid, p.cmd);
            }
            if !dry_run {
                state::prune_old_sessions();
                // A running board owns the claim locks (it knows the live sessions).
                let n = if kanban::board::board_running() {
                    0
                } else {
                    kanban::board::sweep_dead_locks()
                };
                if n > 0 {
                    println!("{n} stale kanban lock(s) removed");
                }
            }
            println!("{} process(es)", killed.len());
        }
        Cmd::Board {
            ensure,
            detach,
            rotate_token,
            url,
        } => {
            std::process::exit(kanban::board::run(
                &config::load(),
                ensure,
                detach,
                rotate_token,
                url,
            ));
        }
        Cmd::Mcp => std::process::exit(kanban::mcp::run(&config::load())),
        Cmd::Merge {
            target,
            verify,
            transcript,
        } => {
            let t = transcript.or_else(|| std::env::var("BROOD_TRANSCRIPT").ok());
            std::process::exit(merge::run(&target, verify.as_deref(), t.as_deref()));
        }
        Cmd::Ps => {
            for p in procs::all_session_procs() {
                println!("{}\t{}\t{}", p.session, p.pid, p.cmd);
            }
        }
    }
}
