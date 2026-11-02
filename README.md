# brood

brood is a Claude Code plugin with a Rust helper binary. The plugin keeps the main session (the coordinator) free to take new requests. The coordinator sends work to background subagents. The helper binary, `brood hook`, runs on Claude Code hook events. It enforces limits, cleans up processes, and blocks unsafe commands.

## 1. Requirements

- Claude Code with plugin support.
- The `brood` binary on `PATH`. The hooks run `brood hook`.
- Rust (edition 2024), only to build with `cargo install`.

## 2. Installation

1. Download the binary from [Releases](https://github.com/others-git/brood/releases). Or run `cargo install --git https://github.com/others-git/brood`.
2. Put `brood` on your `PATH`.
3. In Claude Code, run `/plugin marketplace add others-git/brood`.
4. Run `/plugin install brood@brood`.
5. Start a new session.

To test without installing, run `claude --plugin-dir ./plugin`.

## 3. Description of function

### 3.1 Coordinator limits

These rules apply to the coordinator only, per user message, when `coordinator` is `true`. Claude Code bookkeeping paths (auto-memory, plans, session scratchpad) are exempt. `#solo` in the message silences all nudges and the refusals below.

**Nudges.** brood adds context to the model and does not block the call. Each nudge fires once per message. `0` disables a threshold.

| Trigger | Threshold | Nudge |
|---|---|---|
| Work tool calls (Bash, Read, Grep, Glob, Edit, Write, MultiEdit, NotebookEdit, WebFetch, WebSearch) | `main_tool_budget` = 3 | Soft: "this request is growing". |
| Distinct files edited | 3 | Soft. |
| Lines changed in one edit | `trivial_edit_lines` = 30 | Soft. |
| Work tool calls | `firm_nudge_after` = 8 | Firm: "delegate the remainder now". |

**Refusals.**

| Rule | Override |
|---|---|
| Foreground build or replay (`cargo build`, `make`, `npm run build`, game runs). Tests and checks (`cargo test`, `pytest`, `go test`, `npm test`, `cargo check`, `clippy`) run inline | `run_in_background`, or `#solo` |
| `general-purpose` agent without `model` | Set `model`, or use a brood agent |

Git-only commands and `free_commands` matches do not count toward the nudges. The counts reset when the coordinator turn ends.

### 3.2 Subagent rules

| Agent | Model | Tools |
|---|---|---|
| `brood:researcher` | Haiku | Read, Grep, Glob, WebFetch, WebSearch |
| `brood:implementer` | Sonnet | All |
| `brood:reviewer` | Opus | Read, Grep, Glob, Bash |

- Every report starts with `STATUS: complete`, `STATUS: partial` or `STATUS: blocked`.
- A `brood:` subagent that ends without a `STATUS:` line is sent back.
- A subagent spawns helpers in the foreground only. The limit is `max_helpers` = 3 per subagent.
- A subagent that ends while its helpers still run is sent back. After `cleanup_retries` = 2 returns, brood releases it. The coordinator then gets a notice with the first 300 characters of the last message.

### 3.3 Process cleanup

- brood refuses `nohup`, `setsid`, `disown`, `daemonize`, `start /b`, `start /min`, `Start-Process` and a lone `&`. Use `run_in_background`.
- A subagent that ends with its own processes or background tasks running is sent back. After `cleanup_retries` returns, brood stops them.
- Session end stops the leftovers of that session.
- Session start stops the leftovers of dead sessions.
- brood finds processes by the `CLAUDE_CODE_SESSION_ID` and `CLAUDE_PID` variables. It does not rewrite your commands.
- brood sends a polite signal first, then forces a kill. Windows has no polite signal, so brood ends the process at once.

### 3.4 Git safety

| Rule | Action | Override |
|---|---|---|
| `git worktree remove` on a worktree with uncommitted or untracked files (build junk like `__pycache__/` ignored) | Refused. brood lists up to 10 files. | `# brood:allow-dirty` in the command |
| Coordinator merges or removes the branch or worktree of a running or just-resumed agent | Refused | Wait for the agent report |
| `git merge`, `pull`, `rebase` or `cherry-pick` fails, or a merge script reports a git failure | Warning: do not delete the branch or worktree | None |

### 3.5 Guards

| Guard | Trigger | Action |
|---|---|---|
| Loop | Same Bash command, same agent, `loop_streak` = 4 times in a row | Refused on the 4th run |
| Script hint | Chain of 3 or more commands, run a second time | Hint to use a script |
| Secret | Network command or WebFetch with a secret-looking string | Refused |
| Credential | Network command that names a credential file | Refused |
| Credential read | Read or shell use of `.ssh`, `.env`, `.aws/credentials`, `.netrc` and similar | Asks you |
| Config write | Edit of Claude Code settings, plugins or brood config | Asks you |
| Injection | Result of a `scan_tools` tool with instruction-like text | Warning: treat as data |

**WARNING:** The guards are tripwires, not a security boundary. The permission system of Claude Code is the real gate.

### 3.6 Kanban board (optional)

Off by default. Set `kanban = true` (or `BROOD_KANBAN=1`) to enable it. brood then keeps cards as markdown files in each project and serves a web board.

- **Cards.** `<project>/kanban/<column>/<id>.md`. Default columns: `backlog`, `ready`, `in-progress`, `review`, `done`. The file has TOML frontmatter between `+++` lines (`id`, `title`, `rev`, `priority`, `tags`, `owner`, `created`, `updated`). The body is the description. IDs look like `20260725-a1b2-fix-login`. Commit the cards. `kanban/.locks/` is gitignored.
- **Board server.** `brood board` runs one server per machine, on `127.0.0.1:<kanban_port>`. The SessionStart hook starts it if it is down. The sessions register on start and unregister on end. The server exits after 10 minutes with no live session. It serves all projects. Projects are identified by a normalised path, so `/mnt/d/x` (WSL) and `D:\x` (Windows) are one project.
- **Open the board.** `http://127.0.0.1:47777/?token=<token>`. The token is the content of `board.token` in the data directory (mode 0600). Every API call needs it. Requests with a `Host` other than `127.0.0.1` or `localhost` are refused.
- **Single writer.** Only the board writes cards, one at a time. Each update carries the card `rev`. A stale `rev` gives 409 and the current card. Edits made by hand or by `git pull` are noticed within about 2 s.
- **Claims.** `kanban_claim` creates `kanban/.locks/<id>.lock` atomically. A lock of a dead session can be taken over. `brood gc` deletes it.
- **MCP tools.** `brood mcp` (registered in `plugin/.mcp.json`) gives the model `kanban_list`, `kanban_create`, `kanban_update` and `kanban_claim`. With kanban off it lists no tools.
- **Send.** The board UI can send a card to a live session.
  - Hook delivery (always on): the session gets the card in the context of its next prompt, or as a `Stop` block at the end of a running turn. An idle session gets the card on its next user message.
  - Push delivery (research preview): `brood mcp` also declares a Claude Code channel, and the card arrives at once, also in an idle session. Start Claude Code with `claude --dangerously-load-development-channels plugin:brood@brood`. Without the flag, Claude Code drops the push silently. Set `kanban_push = false` to use hook delivery only. A card is delivered once: the first path that takes it wins.
- **First launch.** If a git project has no `kanban/` directory, the session start adds a hint to turn existing planning docs into cards. brood asks you before it creates any card. The first card writes `kanban/.migrated`, and the hint stops.

## 4. Commands

| Command | Function |
|---|---|
| `brood hook` | Handle a hook event from stdin. Claude Code runs it. Do not run it by hand. |
| `brood gc` | Stop processes of dead Claude Code sessions. Delete session state older than 7 days and the kanban claim locks of dead sessions. |
| `brood gc --dry-run` | List the processes `gc` would stop. Stop nothing. |
| `brood merge <branch-or-path> [--verify "<cmd>"]` | Merge a worktree agent's branch. Refuse (exit 2) if the worktree has uncommitted non-junk files or its agent is live. Run `--verify` after the merge; write its output to `.git/brood-verify.log`. On conflict or verify failure, keep the worktree and branch (exit 1). On success, remove both. Free of the coordinator budget. |
| `brood board` | Run the kanban board server. Exit 0 if one is already running. |
| `brood board --ensure` | Start a detached board if none answers. The hooks use this. |
| `brood mcp` | Stdio MCP server for the kanban tools. Claude Code runs it. |
| `brood ps` | List live processes tagged with a session: session, PID, command. |

## 5. Configuration

The file is optional. Every key is optional.

| Key | Default | Function |
|---|---|---|
| `coordinator` | `true` | Inject the coordinator prompt and enforce coordinator limits. |
| `loop_streak` | `4` | Identical commands in a row before refusal. `0` = off. |
| `cleanup_retries` | `2` | Times brood sends a subagent back before it cleans up. |
| `max_helpers` | `3` | Helpers per subagent. `0` = no cap. |
| `main_tool_budget` | `3` | Work tool calls per message that trigger the soft nudge. `0` = off. |
| `trivial_edit_lines` | `30` | Edit size (lines) that triggers the soft nudge. `0` = off. |
| `firm_nudge_after` | `8` | Work tool calls per message that trigger the firm nudge. `0` = off. |
| `free_commands` | `[]` | Regexes for shell commands that do not count toward the nudges. |
| `scan_tools` | `^(WebFetch\|WebSearch\|mcp__.*)$` | Regex on tool names whose output brood scans. |
| `extra_secret_patterns` | `[]` | Extra regexes for secrets. |
| `extra_injection_patterns` | `[]` | Extra regexes for instruction-like text. |
| `kanban` | `false` | Enable the kanban board (3.6). |
| `kanban_port` | `47777` | Board port, on 127.0.0.1. |
| `kanban_columns` | `["backlog","ready","in-progress","review","done"]` | Board columns, left to right. Letters, digits, `-` and `_`. |
| `kanban_push` | `true` | Also push sent cards as channel events (needs the launch flag in 3.6). |
| `worktree_junk` | `[]` | Extra `name/` or `*.ext` patterns the worktree-remove guard ignores, on top of `__pycache__/`, `*.pyc`, `node_modules/`, `target/` and similar. Untracked files only. |

```toml
coordinator = true
main_tool_budget = 5
free_commands = ["^ls\\b"]
extra_secret_patterns = ["mykey_[A-Za-z0-9]{32}"]
```

| OS | Path |
|---|---|
| Linux | `~/.config/brood/config.toml` |
| macOS | `~/Library/Application Support/brood/config.toml` |
| Windows | `%APPDATA%\brood\config.toml` |

Environment variables:

| Variable | Function |
|---|---|
| `BROOD_COORDINATOR=0` (or `false`) | Disable coordinator mode. |
| `BROOD_KANBAN=1` (or `0`) | Enable (disable) the kanban board. Overrides the file. |
| `BROOD_KANBAN_PORT` | Board port. Overrides the file. |
| `BROOD_DATA_DIR` | Data directory. |
| `BROOD_DEBUG=1` | Also log raw hook events to the global log. |

To change the coordinator instructions, edit `plugin/context/coordinator.md`. brood reads it from `CLAUDE_PLUGIN_ROOT` at session start. No rebuild is needed.

## 6. Logs and troubleshooting

The data directory is `~/.local/share/brood` (Linux), `~/Library/Application Support/brood` (macOS) or `%LOCALAPPDATA%\brood` (Windows).

| File | Content |
|---|---|
| `sessions/<session_id>/brood.log` | Denials, releases and kills of one session |
| `board.lock`, `board.token`, `board-state.json` | Kanban board pid and port, access token, registered sessions and sent cards |
| `brood.log` | Hook errors, and raw events when `BROOD_DEBUG=1` |

| Problem | Action |
|---|---|
| Hooks do nothing | Make sure `brood --version` works in the shell Claude Code uses. |
| Coordinator refuses a small task | Only a foreground build or replay is refused: run it in the background. Add `#solo` to the message to allow it. Nudges are advice: raise `main_tool_budget` or `firm_nudge_after`, or set them to `0`. |
| Config has no effect | Check the TOML syntax. brood uses all defaults if the file does not parse. |
| Processes remain | Run `brood ps`, then `brood gc`. |

## 7. Limits

**WARNING:** The guards are tripwires. Do not rely on them to stop a malicious command.

**WARNING:** `# brood:allow-dirty` destroys uncommitted files in the worktree. Use it only when you want that.

**CAUTION:** The worktree guard skips quoted paths with spaces. It also skips paths set by `git -C` or `cd`. It does not refuse these commands.

**CAUTION:** Agent-team teammates do not appear in `claude -p` runs. This mode is untested.

**CAUTION:** CI tests Windows and macOS builds only. Native use on those systems is not otherwise tested.

**CAUTION:** Kanban edits that bypass the board (by hand, `git pull`) do not raise `rev`. A board edit made from an older view can overwrite them.

**NOTE:** brood fails open. If the hook fails, the tool call goes on.

**NOTE:** Windows `.exe` processes started from WSL are not in the Linux process table. brood does not clean them.

**NOTE:** If Claude Code crashes, brood sweeps the leftovers at the next session start.

## 8. Release procedure (maintainers)

Requirements: `cargo`, `git`, `gh` and `claude` on `PATH`. The script runs `git add -A`, so commit or stash unrelated changes first.

1. Run `tools/release.sh X.Y.Z [commit-message-file]`.
2. The script stops if tag `vX.Y.Z` exists.
3. It sets the version in `Cargo.toml` and `plugin/.claude-plugin/plugin.json`.
4. It runs `cargo build` and `cargo test`.
5. It commits, tags `vX.Y.Z`, and pushes `main` and the tag.
6. The tag starts the `release` workflow. The script waits for it to pass.
7. It installs the Linux x86_64 binary to `~/.local/bin/brood` and checks the version.
8. It updates the Claude Code marketplace and the plugin.

On failure, the script prints the failed step. The log is `${TMPDIR:-/tmp}/brood-release.log`.
