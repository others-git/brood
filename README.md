# brood

Claude Code plugin plus a Rust hook binary. The main session (coordinator) hands work to background subagents; `brood hook` enforces limits, cleans up processes and blocks unsafe commands.

## Install

1. Get the binary from [Releases](https://github.com/others-git/brood/releases) or `cargo install --git https://github.com/others-git/brood`. Put it on `PATH`.
2. In Claude Code: `/plugin marketplace add others-git/brood`, then `/plugin install brood@brood`.
3. Start a new session. (No install: `claude --plugin-dir ./plugin`.)

## Agents

| Agent | Model | Tools |
|---|---|---|
| `brood:researcher` | Haiku | Read, Grep, Glob, WebFetch, WebSearch |
| `brood:implementer` | Sonnet | All |
| `brood:reviewer` | Opus | Read, Grep, Glob, Bash |

Reports start with `STATUS: complete|partial|blocked`, or the agent is sent back. Helpers: foreground only, max `max_helpers`. An agent ending with processes or helpers running is sent back `cleanup_retries` times, then cleaned up.

## Nudged vs refused

Per user message, coordinator only. Memory, plans and scratchpad paths, git-only commands and `free_commands` do not count. `0` disables a threshold.

| Trigger | Threshold | Result |
|---|---|---|
| Work tool calls | `main_tool_budget` (3) | Soft nudge |
| Distinct files edited | 3 | Soft nudge |
| Lines in one edit | `trivial_edit_lines` (30) | Soft nudge |
| Work tool calls | `firm_nudge_after` (8) | Firm nudge |
| Foreground build or replay (`cargo build`, `make`, `npm run build`) | always | Refused: use `run_in_background` |
| `general-purpose` agent without `model` | always | Refused: set `model` |

Tests and checks (`cargo test`, `pytest`, `go test`, `npm test`, `cargo check`, `clippy`) run inline.

- **Quick builds:** a foreground compile-only build is allowed after a small edit this turn (at most `trivial_edit_lines` lines, 2 files) with Bash `timeout` at most `quick_build_timeout_secs`. `quick_builds` regexes are allowed anytime, replays included.
- **`#solo`** in a message silences all nudges and refusals.

## Cleanup

- Refused: `nohup`, `setsid`, `disown`, `daemonize`, `start /b`, `Start-Process`, a lone `&`. Use `run_in_background`.
- Session end kills that session's leftovers; session start kills dead sessions' leftovers.

## Git safety

| Rule | Action | Override |
|---|---|---|
| `git worktree remove` with uncommitted or untracked files | Refused | `# brood:allow-dirty` (destroys them) |
| Merge or remove the branch or worktree of a running agent | Refused | Wait for its report |
| `merge`, `pull`, `rebase`, `cherry-pick` fails | Warning: keep branch and worktree | None |

`brood merge <branch-or-path> [--verify "<cmd>"]` merges a worktree agent's branch.

| Exit | Meaning |
|---|---|
| 0 | Merged, verified. Worktree and branch removed. |
| 1 | Conflict or verify failure. Kept. Log: `.git/brood-verify.log`. |
| 2 | Refused: uncommitted files or live agent. |

## Guards

Tripwires, not a security boundary.

| Guard | Trigger | Action |
|---|---|---|
| Loop | Same Bash command `loop_streak` (4) times in a row | Refused |
| Secret / credential | Network command with a secret or credential file | Refused |
| Credential read | `.ssh`, `.env`, `.aws/credentials`, `.netrc` | Asks you |
| Config write | Edit of Claude Code or brood config | Asks you |
| Injection | `scan_tools` output with instruction-like text | Warning |

## Kanban (optional)

Set `kanban = true` or `BROOD_KANBAN=1`. Cards are files `<project>/kanban/<column>/<id>.md` (TOML frontmatter). Commit them.

| Do | How |
|---|---|
| Open board | `brood board --url`, open the URL (`http://127.0.0.1:47777/?token=<token>`). Then bookmark the bare URL. |
| Token | In `board.token` (0600). `brood board --rotate-token` replaces it. API clients send `X-Brood-Token`. |
| Search | Press `/`. |
| Send | Sends a card to a live session. Saves your edits first. |
| Claim | Sessions claim with `kanban_claim`. Cards are claimed on create, released on done or backlog. |
| Decision | Cards in `review` have a Decision box. |

- One board per machine, started by the SessionStart hook, exits 10 minutes after the last session.
- Stale `rev`/`ver` or outside edits give 409. WSL and Windows run separate boards: use different `kanban_port` values.
- MCP tools: `kanban_list`, `kanban_create`, `kanban_update`, `kanban_claim`, `kanban_comment`.
- Sent cards arrive via the plugin monitor (wakes idle sessions), then the hooks (next prompt or `Stop`). Opt-in push: `kanban_push = true` plus `claude --dangerously-load-development-channels plugin:brood@brood`.

### Comments

- Users and sessions comment on a card, as a flat list.
- Sessions use `kanban_comment`; its `awaiting_user` flag shows an "awaiting you" badge.
- User comments are delivered to the owning session.
- If the owner is offline, they wait and go with the next send.

## Commands

| Command | Function |
|---|---|
| `brood hook` | Hook handler (Claude Code runs it) |
| `brood gc [--dry-run]` | Stop dead sessions' processes, prune old state and dead locks |
| `brood ps` | List session-tagged processes |
| `brood merge` | See Git safety |
| `brood board [--url \| --rotate-token \| --ensure]` | Run the board, print URL, new token, or start detached |
| `brood mcp` | Kanban MCP server (Claude Code runs it) |

## Configuration

`~/.config/brood/config.toml` (macOS `~/Library/Application Support/brood/`, Windows `%APPDATA%\brood\`). All keys optional.

| Key | Default | Function |
|---|---|---|
| `coordinator` | `true` | Coordinator prompt and limits |
| `loop_streak` | `4` | Repeats before refusal |
| `cleanup_retries` | `2` | Send-backs before cleanup |
| `max_helpers` | `3` | Helpers per subagent |
| `main_tool_budget` | `3` | Calls before soft nudge |
| `trivial_edit_lines` | `30` | Edit lines before nudge; quick-build edit limit |
| `firm_nudge_after` | `8` | Calls before firm nudge |
| `quick_build_timeout_secs` | `120` | Max Bash timeout for a quick build |
| `quick_builds` | `[]` | Regexes always allowed as builds |
| `free_commands` | `[]` | Regexes for uncounted commands |
| `scan_tools` | `^(WebFetch\|WebSearch\|mcp__.*)$` | Tools whose output is scanned |
| `extra_secret_patterns` | `[]` | Extra secret regexes |
| `extra_injection_patterns` | `[]` | Extra injection regexes |
| `worktree_junk` | `[]` | Extra `name/` or `*.ext` ignored by worktree guard |
| `kanban` | `false` | Enable the board |
| `kanban_port` | `47777` | Board port |
| `kanban_columns` | `["backlog","ready","in-progress","review","done"]` | Board columns |
| `kanban_push` | `false` | Push sent cards as channel events |

Env: `BROOD_COORDINATOR=0`, `BROOD_KANBAN=1|0`, `BROOD_KANBAN_PORT`, `BROOD_DATA_DIR`, `BROOD_DEBUG=1`.

## Notes

- Fails open: a broken hook lets the call through.
- Logs: `sessions/<id>/brood.log` and `brood.log` in the data dir (`~/.local/share/brood` on Linux).
- Stray processes: `brood ps`, then `brood gc`.
- Windows `.exe` processes started from WSL are not cleaned.
- Release: `tools/release.sh X.Y.Z` (maintainers; needs `cargo`, `git`, `gh`, `claude`).
