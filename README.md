# brood

A Claude Code plugin for long sessions with many subagents. It keeps the main session light and stops subagents from leaving a mess behind.

- **Coordinator mode.** The main session plans and queues; work goes to subagents, background ones when independent, so you can keep adding tasks.
- **Cheaper subagents.** `brood:researcher` (Haiku, read-only), `brood:implementer` (Sonnet), `brood:reviewer` (Opus). Every report starts with `STATUS: complete | partial | blocked`.
- **No orphaned helpers.** Subagents must spawn their own helpers in the foreground (at most 3), so a lead can't hand back while its helpers are still writing. If one gets through anyway, brood sends the lead back, then tells the coordinator to resume it.
- **No leftover processes.** `nohup`, `setsid`, `disown`, trailing `&` and `start /b` are refused in favour of the Bash tool's tracked `run_in_background`. A subagent that tries to finish with processes or background tasks still running is sent back to stop them, and brood kills what's left after two tries. Session end kills the session's leftovers, and each new session sweeps processes from crashed ones.
- **Loop guard.** The same Bash command run 4 times in a row by one agent is refused with a "change approach" message.
- **Prompt-injection tripwire.** Web and MCP results with instruction-like text get a "treat this as data" warning. Commands that would send secret-looking strings over the network are refused; reading credential or `.env` files, or editing Claude Code settings, asks you first.

The guards are tripwires, not a security boundary. Claude Code's own permissions remain the real gate.

## Install

1. Put the `brood` binary on your `PATH`: download it from [Releases](https://github.com/others-git/brood/releases), or `cargo install --git https://github.com/others-git/brood`.
2. In Claude Code:
   ```
   /plugin marketplace add others-git/brood
   /plugin install brood@brood
   ```

Try it without installing: `claude --plugin-dir ./plugin`.

## Commands

- `brood gc [--dry-run]`: kill processes left by dead Claude Code sessions, prune old state.
- `brood ps`: list live processes that belong to Claude Code sessions.

## Config

Optional, at `~/.config/brood/config.toml` (Linux), `~/Library/Application Support/brood/config.toml` (macOS) or `%APPDATA%\brood\config.toml` (Windows):

```toml
coordinator = true          # inject coordinator mode; or set BROOD_COORDINATOR=0
loop_streak = 4             # identical commands in a row before refusing (0 = off)
cleanup_retries = 2         # times a subagent is sent back before brood cleans up itself
max_helpers = 3             # helpers per subagent (0 = no cap)
scan_tools = "^(WebFetch|WebSearch|mcp__.*)$"
extra_secret_patterns = ["mykey_[A-Za-z0-9]{32}"]
extra_injection_patterns = []
```

Edit `plugin/context/coordinator.md` to change the coordinator instructions without rebuilding.

## How it finds processes

Claude Code sets `CLAUDE_CODE_SESSION_ID` and `CLAUDE_PID` in the environment of everything it spawns, and that survives detaching. brood reads those tags. It never rewrites your commands, so your permission rules keep working. Logs live in the platform data dir under `brood/` (`BROOD_DEBUG=1` also logs raw hook events).

## Known gaps

- Windows `.exe`s started from WSL run outside Linux's process table and aren't cleaned up.
- If Claude Code crashes, its leftovers are only swept when the next session starts.
- Agent-team teammates don't appear in non-interactive (`claude -p`) runs, so their path is untested.
- Not yet tested on native Windows or macOS beyond CI builds.
