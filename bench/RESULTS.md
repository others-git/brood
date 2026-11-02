# Results

Does brood make a subagent workflow cheaper than plain Claude told to delegate? On these small tasks: no clear difference overall. The answer depends on the task, and N=3 is too low to be sure.

## 1. Method

- Opus main (`--model opus`), `claude -p`, same `--allowedTools`, fresh fixture copy per run, `< /dev/null`.
- `with`: repo plugin via `--plugin-dir` (installed `brood` binary 0.0.7, with `brood merge`), installed plugin disabled. `delegate`: no plugin, `--append-system-prompt` "Use subagents for tasks... haiku for research, sonnet for implementation".
- Cost: `total_cost_usd` from the `claude -p` result. It includes subagents: its `modelUsage` token counts match main plus subagent transcripts exactly.
- Condition order was shuffled per rep and task in the runs that used `run.sh` with `final`. Earlier runs were not shuffled.
- Ratio = mean cost with / mean cost delegate. CI: bootstrap, 2000 resamples per cell, `analyze.py`. Overall: geometric mean of the task ratios.
- 9 of the 18 runs come from earlier batches (`results/merge1`) whose result had no cost. Their cost is estimated from transcript tokens at rates fitted to the measured runs (fit error 0.0%; `rescore_old.py`). Two `merge1` parallel/with runs failed verify only on a script bug (baseline test count taken from `HEAD`), and are counted as passed.

## 2. Table

Cost per run in USD, mean +- sd, 3 runs per cell. All 18 runs passed verify.

| Task | with | delegate | with/delegate (95% CI) | Verdict |
|---|---|---|---|---|
| bugfix | 0.254 +- 0.020 | 0.214 +- 0.027 | 1.19 (1.04-1.37) | brood costlier |
| parallel | 0.447 +- 0.076 | 0.369 +- 0.016 | 1.21 (1.06-1.43) | brood costlier |
| bigfeature | 0.273 +- 0.010 | 0.329 +- 0.009 | 0.83 (0.80-0.86) | brood cheaper |
| geometric mean | | | 1.06 (0.99-1.14) | no clear difference |

Secondary, mean per run: wall time with/delegate was bugfix 41/30 s, parallel 49/36 s, bigfeature 39/55 s. Main peak context was about 31-37k in every cell. No leftover processes, worktrees or branches in either condition. brood denied 1 call (parallel/with). `brood merge` was used in the parallel/with runs.

## 3. Verdict

- brood was costlier on the two small or parallel tasks and cheaper on the 5-module feature. The overall ratio of 1.06 has a CI that includes 1.0.
- Do not read this as "brood helps" or "brood hurts". It shows brood's fixed overhead on small work and a gain when the main session would otherwise do a lot itself.

## 4. Limits

- **N=3 per cell is low.** Bootstrap CIs from 3 samples are too narrow and understate the real uncertainty. Treat them as a rough guide.
- 9 of 18 costs are estimates, from earlier plugin runs with the same settings.
- Tasks are small, synthetic and quick (30-60 s). Long sessions, where context cost grows, were not tested.
- The cleanup and loop guards, and the process, worktree and branch cleanup that brood is built for, were not triggered by these tasks. The `delegate` baseline did not leak either. This benchmark says nothing about them.
- Sonnet main and the solo reference were dropped to save budget. Solo and feature/refactor results in `results/final/` are single runs and are not analysed here.

## 5. After nudges (brood 0.0.8)

The coordinator limits are now nudges, not denies. Opus main, `with` only, 3 runs per task (`results/nudge1`, measured cost), compared with the delegate runs in section 2. N=3 is low.

| Task | with (nudges) | delegate | with/delegate (95% CI) | Turns with / delegate | Agents | Denies | Nudges | Verify |
|---|---|---|---|---|---|---|---|---|
| bugfix | 0.239 +- 0.009 | 0.214 +- 0.027 | 1.12 (1.00-1.26) | 6.3 / 3.7 | 0.0 / 0.3 | 0 | 9 | 3/3 |
| bigfeature | 0.325 +- 0.043 | 0.329 +- 0.009 | 0.99 (0.85-1.11) | 5.7 / 6.0 | 1.0 / 1.0 | 0 | 0 | 3/3 |

Wall time with / delegate: bugfix 24 / 30 s, bigfeature 50 / 55 s.

- Against the pre-nudge `with` cells (section 2), bugfix cost fell from 0.254 to 0.239, and wall time from 41 to 24 s. In all 3 runs the coordinator did the fix inline after nudges and spawned no agent.
- bigfeature rose from 0.273 to 0.325, to the level of delegate. The old coordinator did it in 2 main turns; the new one used about 6.
- The gain over delegate on bigfeature is gone. bugfix is still at or slightly above delegate.
- Nudges fired only on bugfix (3 per run). Nothing is shown about parallel; it was not rerun.

## 6. After tests-inline and delegate-at-once

Changes since section 5: tests may run inline, and the coordinator delegates obvious feature work at once, without exploring first. Opus main, `with` only, 3 runs per task (`results/nudge2`, measured cost). N=3 is low.

| Task | nudge2 cost | nudge1 cost | delegate cost | nudge2/delegate (95% CI) | Turns nudge2 / nudge1 / delegate | Agents | Denies | Nudges | Verify |
|---|---|---|---|---|---|---|---|---|---|
| bugfix | 0.219 +- 0.014 | 0.239 | 0.214 | 1.03 (0.91-1.17), no clear difference | 5.0 / 6.3 / 3.7 | 0 | 0 | 6 | 3/3 |
| bigfeature | 0.287 +- 0.012 | 0.325 | 0.329 | 0.87 (0.83-0.91), brood cheaper | 3.3 / 5.7 / 6.0 | 1 | 0 | 0 | 3/3 |

Wall time nudge2 / nudge1 / delegate: bugfix 20 / 24 / 30 s, bigfeature 49 / 50 / 55 s. No leftover processes, worktrees or branches.

- bigfeature is cheaper than delegate again (about 13%), with fewer main turns than nudge1. bugfix is level with delegate.
- The CIs are narrow because 3 samples understate the spread. Only the bigfeature gap looks clear, and it is modest.
- parallel was not rerun after the nudge changes.
