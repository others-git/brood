# brood

Claude Code plugin plus a Rust hook binary. The main session (coordinator) stays free: work goes to background subagents, and `brood hook` enforces limits, cleans up processes and blocks unsafe commands.

## Install

1. Get the binary from [Releases](https://github.com/others-git/brood/releases), or `cargo install --git https://github.com/others-git/brood`. Put it on `PATH`.
2. In Claude Code: `/plugin marketplace add others-git/brood`, then `/plugin install brood@brood`.
3. Start a new session.

Try without installing: `claude --plugin-dir ./plugin`.

## Agents

| Agent | Model | Tools |
|---|---|---|
| `brood:researcher` | Haiku | Read, Grep, Glob, WebFetch, WebSearch |
| `brood:implementer` | Sonnet | All |
| `brood:reviewer` | Opus | Read, Grep, Glob, Bash |

- Reports start with `STATUS: complete`, `partial` or `blocked`. A `brood:` agent without one is sent back.
- Helpers are foreground only, max `max_helpers` per agent.
- An agent that ends with helpers or processes still running is sent back `cleanup_retries` times, then released or killed.

## Coordinator limits

Per user message, when `coordinator = true`. Bookkeeping paths (memory, plans, scratchpad) are exempt. Git-only and `free_commands` matches are not counted.

| Trigger | Threshold | Result |
|---|---|---|
| Work tool calls | `main_tool_budget` (3) | Soft nudge |
| Distinct files edited | 3 | Soft nudge |
| Lines in one edit | `trivial_edit_lines` (30) | Soft nudge |
| Work tool calls | `firm_nudge_after` (8) | Firm nudge: delegate now |
| Foreground build or replay (`cargo build`, `make`, `npm run build`, game runs) | always | Refused. Use `run_in_background` |
| `general-purpose` agent without `model` | always | Refused. Set `model` or use a brood agent |

Tests and checks (`cargo test`, `pytest`, `go test`, `npm test`, `cargo check`, `clippy`) run inline. `0` disables a threshold.

**Quick builds.** A foreground compile-only build is allowed when this turn's edits are small (at most `trivial_edit_lines` lines, at most 2 files) and the Bash `timeout` is at most `quick_build_timeout_secs` (120). Commands matching `quick_builds` are always allowed, replays included.

**`#solo`** in a message silences all nudges and refusals for that message.

## Process cleanup

- Refused: `nohup`, `setsid`, `disown`, `daemonize`, `start /b`, `Start-Process`, a lone `&`. Use `run_in_background`.
- Session end stops that session's leftovers. Session start stops dead sessions' leftovers.
- Processes are found by `CLAUDE_CODE_SESSION_ID` / `CLAUDE_PID`. Polite signal first, then kill.

## Git safety

| Rule | Action | Override |
|---|---|---|
| `git worktree remove` with uncommitted or untracked files (build junk ignored) | Refused, lists up to 10 files | `# brood:allow-dirty` (destroys the files) |
| Merging or removing the branch or worktree of a running agent | Refused | Wait for its report |
| `merge`, `pull`, `rebase`, `cherry-pick` fails | Warning: keep branch and worktree | None |

**`brood merge <branch-or-path> [--verify "<cmd>"]`** merges a worktree agent's branch.

| Exit | Meaning |
|---|---|
| 0 | Merged (and verified). Worktree and branch removed. |
| 1 | Conflict or verify failure. Both kept. Verify output: `.git/brood-verify.log`. |
| 2 | Refused: uncommitted files or live agent. |

## Guards

Tripwires, not a security boundary. Claude Code permissions are the real gate.

| Guard | Trigger | Action |
|---|---|---|
| Loop | Same Bash command `loop_streak` (4) times in a row | Refused |
| Script hint | Chain of 3+ commands run twice | Hint to script it |
| Secret | Network command or WebFetch with a secret-looking string | Refused |
| Credential | Network command naming a credential file | Refused |
| Credential read | `.ssh`, `.env`, `.aws/credentials`, `.netrc` | Asks you |
| Config write | Edit of Claude Code settings, plugins or brood config | Asks you |
| Injection | `scan_tools` output with instruction-like text | Warning |

## Kanban (optional)

Off by default. Enable with `kanban = true` or `BROOD_KANBAN=1`.

**Cards** are markdown files: `<project>/kanban/<column>/<id>.md`, TOML frontmatter between `+++`. Default columns: `backlog`, `ready`, `in-progress`, `review`, `done`. Commit them; `kanban/.locks/` is gitignored.

**Board.** One server per machine on `127.0.0.1:<kanban_port>`, started by the SessionStart hook, exits 10 minutes after the last session ends.

| Do | How |
|---|---|
| Open | `brood board --url`, open the printed `http://127.0.0.1:47777/?token=<token>`. A cookie is set; bookmark the bare URL. |
| Token | Created once, kept in `board.token` (0600). `brood board --rotate-token` invalidates old URLs. |
| API | Send header `X-Brood-Token`. Wrong `Host` or `Origin` is refused. |
| Search | Press `/` on the board. |
| Send | Card button sends it to a live session. Unsaved edits are saved first. |
| Claim | The session claims with `kanban_claim` (a lock file). Claimed on create, released on done or backlog. |
| Decision | Cards in `review` have a Decision box for your verdict. |

**Writes.** Only the board writes cards. A stale `rev`/`ver` or an outside edit gives 409 with the current card. WSL and native Windows run separate boards: give them different `kanban_port` values. They share the card files.

**MCP tools** (`brood mcp`): `kanban_list`, `kanban_create`, `kanban_update`, `kanban_claim`, `kanban_comment`. None when kanban is off.

**Delivery of sent cards** (framed as `<kanban-card>` task data):

| Path | Default | How |
|---|---|---|
| Monitor | on | `brood inbox --follow` wakes an idle session with one line per card. Needs plugin monitors (Claude Code 2.1.3x+). |
| Hooks | on | Card arrives with the next prompt or as a `Stop` block. Also redelivers a card unclaimed after 60 s. |
| Push | `kanban_push = false` | Channel event. Start with `claude --dangerously-load-development-channels plugin:brood@brood`. |

### Comments

- Users and sessions add comments to a card, shown as a flat list.
- Sessions use `kanban_comment`. Its `awaiting_user` flag shows an "awaiting you" badge.
- User comments go to the owning session.
- If the owner is offline, they wait and go with the next send.

## Commands

| Command | Function |
|---|---|
| `brood hook` | Hook handler. Claude Code runs it. |
| `brood gc [--dry-run]` | Stop processes of dead sessions, prune state older than 7 days, remove dead claim locks. |
| `brood ps` | List live session-tagged processes. |
| `brood merge <target> [--verify "<cmd>"]` | See Git safety. |
| `brood board [--url \| --rotate-token \| --ensure]` | Run the board, print its URL, rotate the token, or start it detached. |
| `brood mcp` | Kanban MCP server. Claude Code runs it. |

## Configuration

Optional file; all keys optional. A file that does not parse means all defaults.

| OS | Path |
|---|---|
| Linux | `~/.config/brood/config.toml` |
| macOS | `~/Library/Application Support/brood/config.toml` |
| Windows | `%APPDATA%\brood\config.toml` |

| Key | Default | Function |
|---|---|---|
| `coordinator` | `true` | Coordinator prompt and limits |
| `loop_streak` | `4` | Identical commands before refusal (0 = off) |
| `cleanup_retries` | `2` | Send-backs before brood cleans up |
| `max_helpers` | `3` | Helpers per subagent (0 = no cap) |
| `main_tool_budget` | `3` | Calls before soft nudge (0 = off) |
| `trivial_edit_lines` | `30` | Edit lines before soft nudge (0 = off); also the quick-build edit limit |
| `firm_nudge_after` | `8` | Calls before firm nudge (0 = off) |
| `quick_build_timeout_secs` | `120` | Max Bash timeout for a quick build |
| `quick_builds` | `[]` | Regexes for builds always allowed |
| `free_commands` | `[]` | Regexes for commands not counted |
| `scan_tools` | `^(WebFetch\|WebSearch\|mcp__.*)$` | Tool names whose output is scanned |
| `extra_secret_patterns` | `[]` | Extra secret regexes |
| `extra_injection_patterns` | `[]` | Extra injection regexes |
| `worktree_junk` | `[]` | Extra `name/` or `*.ext` ignored by the worktree guard |
| `kanban` | `false` | Enable the board |
| `kanban_port` | `47777` | Board port |
| `kanban_columns` | `["backlog","ready","in-progress","review","done"]` | Columns (letters, digits, `-`, `_`) |
| `kanban_push` | `false` | Also push sent cards as channel events |

Env: `BROOD_COORDINATOR=0`, `BROOD_KANBAN=1\|0`, `BROOD_KANBAN_PORT`, `BROOD_DATA_DIR`, `BROOD_DEBUG=1` (raw hook events in the log).

Coordinator prompt: edit `plugin/context/coordinator.md`; no rebuild needed.

## Logs and troubleshooting

Data dir: `~/.local/share/brood` (Linux), `~/Library/Application Support/brood` (macOS), `%LOCALAPPDATA%\brood` (Windows).

| File | Content |
|---|---|
| `sessions/<id>/brood.log` | Denials, releases, kills per session |
| `brood.log` | Hook errors |
| `board.*` | Board lock, pid, token, state |

| Problem | Fix |
|---|---|
| Hooks do nothing | `brood --version` must work in Claude Code's shell |
| Small task refused | Run the build in background, or add `#solo` |
| Config ignored | Check TOML syntax |
| Stray processes | `brood ps`, then `brood gc` |

## Limits

- brood fails open: a failed hook lets the call through.
- The worktree guard skips quoted paths with spaces and paths set by `git -C` or `cd`.
- Windows `.exe` processes started from WSL are not cleaned.
- Only Windows and macOS builds are tested in CI.

## Release (maintainers)

`tools/release.sh X.Y.Z [commit-message-file]` bumps versions, builds, tests, commits, tags, pushes, waits for the release workflow, then installs the binary and updates the plugin. Needs `cargo`, `git`, `gh`, `claude`. It runs `git add -A`. Log: `${TMPDIR:-/tmp}/brood-release.log`.
