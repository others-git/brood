//! brood: the helper binary behind the brood Claude Code plugin.
//!
//! The plugin's hooks call `brood hook`, which reads the hook event JSON on
//! stdin and answers on stdout. `brood gc` sweeps leftovers by hand.

mod config;
mod guard;
mod hook;
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
            }
            println!("{} process(es)", killed.len());
        }
        Cmd::Ps => {
            for p in procs::all_session_procs() {
                println!("{}\t{}\t{}", p.session, p.pid, p.cmd);
            }
        }
    }
}
