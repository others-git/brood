# bench

A/B benchmark: Claude Code with brood against Claude Code without brood. Each run is one `claude -p` call on a fresh copy of a small Python project (`fixture/`).

## 1. Requirements

- `claude` and `python3` (stdlib only) on `PATH`; `git`; Linux (leftover scan reads `/proc`).
- brood installed as `brood@brood` for the `with` condition.

## 2. Procedure

1. Run `bench/run.sh --reps 3`. Runs go one at a time.
2. Read the summary table at the end. Per-run JSON is in `bench/results/<timestamp>/`.
3. Re-print a table with `python3 bench/score.py summary bench/results/<timestamp>`.
4. On failure read `bench/results/run.log` (one file, overwritten by every run).

| Option | Default | Function |
|---|---|---|
| `--reps N` | 1 | Repetitions per task and condition |
| `--tasks a,b` | all | Task names from `tasks/` |
| `--conditions with,without` | both | `without` adds `--settings '{"enabledPlugins":{"brood@brood":false}}'` |
| `BENCH_MODEL` | `sonnet` | Main-session model. Cheaper than a real Opus setup, so absolute numbers differ from daily use. |
| `BENCH_TIMEOUT` | 900 | Seconds per run |
| `BENCH_RUNS` | `/tmp/brood-bench-runs` | Where run copies live. Deleted after scoring. |
| `BENCH_STAMP` | now | Results directory name. Reuse it to add cells to one result set. |

Every run uses `--session-id <uuid>`, a fixed `--allowedTools` list, `< /dev/null` and `timeout`. `CLAUDE_CODE_SESSION_ID`, `CLAUDE_PID` and `CLAUDECODE` are unset first.

## 3. Tasks

| Task | Tests | verify.sh checks |
|---|---|---|
| `feature` | Feature over 3 files plus tests | CLI output order and content, new method, suite, new tests |
| `bugfix` | Bug that needs searching | Code normalisation cases, suite, regression test |
| `design` | Proposal in `docs/` | Length, headings, topics, source untouched |
| `server` | Start server, hit endpoint, stop it | `out/summary.json` matches the data |
| `parallel` | Two independent changes | Both behaviours, suite, new tests |

Each `verify.sh` runs in the run copy and exits 0 on success. To add a task, create `tasks/<name>/{prompt.md,verify.sh}`.

## 4. Measured per run

| Field | Source |
|---|---|
| `success` | `verify.sh` exit 0 and no timeout |
| `tokens` | Per model: input, output, cache_read, cache_write. Main transcript plus `subagents/*.jsonl`, deduplicated by message id. No prices. |
| `main_peak_context` | Largest input + cache_read + cache_write on one main assistant message |
| `agents` | Agent type (from `.meta.json`) and model of each subagent |
| `wall_s` | Wall time of `claude` |
| `leftover_processes` | Live processes with the session id in their environment or a cwd in the run dir, 2 s after exit. Killed after recording. |
| `leftover_worktrees`, `leftover_branches` | `git worktree list` and `git branch` in the run copy, beyond main |
| `refusals` | `deny` lines in `~/.local/share/brood/sessions/<session>/brood.log`; 0 for `without` |

## 5. Limits

- **WARNING:** Runs spend real tokens and run `claude` with Bash allowed. Use a machine you trust.
- Port 8765 must be free for `server`.
- Results vary run to run. Use `--reps` 3 or more before drawing conclusions.
- Haiku and Opus appear only when the model delegates to `brood:researcher` or `brood:reviewer`.
