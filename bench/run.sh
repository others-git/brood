#!/usr/bin/env bash
# A/B benchmark: Claude Code with brood vs without. See bench/README.md.
# usage: bench/run.sh [--reps N] [--tasks a,b] [--conditions with,without] [--main-model alias]
set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPS=1
TASKS=$(ls "$HERE/tasks" | paste -sd,)
CONDS=with,without
RUNS=${BENCH_RUNS:-/tmp/brood-bench-runs}
TIMEOUT=${BENCH_TIMEOUT:-900}
MODEL=${BENCH_MODEL:-sonnet}
PLUGIN_DIR=${BENCH_PLUGIN_DIR:-}
# delegate: no plugin, only the plain pre-brood intent as an instruction
DELEGATE_PROMPT="Use subagents for tasks. Keep the main session free for queueing new tasks to subagents. Use research agents for lookups. Prefer cheaper models (haiku for research, sonnet for implementation) when spawning agents."
TOOLS="Bash Read Edit Write MultiEdit Glob Grep Agent Task SendMessage TaskOutput TaskStop"
while [ $# -gt 0 ]; do
  case $1 in
    --reps) REPS=$2; shift 2 ;;
    --tasks) TASKS=$2; shift 2 ;;
    --conditions) CONDS=$2; shift 2 ;;
    --main-model) MODEL=$2; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
STAMP=${BENCH_STAMP:-$(date +%Y%m%d-%H%M%S)}
OUT="$HERE/results/$STAMP"
LOG="$HERE/results/run.log"          # overwritten every run
mkdir -p "$OUT" "$RUNS"
: > "$LOG"
fail() { echo "FAILED: $*" >&2; tail -n 40 "$LOG" >&2; echo "full log: $LOG" >&2; exit 1; }

for c in ${CONDS//,/ }; do case $c in with|without|delegate) ;; *) fail "bad condition $c" ;; esac; done
for t in ${TASKS//,/ }; do [ -f "$HERE/tasks/$t/prompt.md" ] || fail "no task $t"; done
command -v claude >/dev/null || fail "claude not on PATH"

for rep in $(seq 1 "$REPS"); do
 for task in ${TASKS//,/ }; do
  for cond in ${CONDS//,/ }; do
    sid=$(python3 -c 'import uuid;print(uuid.uuid4())')
    dir=$(mktemp -d "$RUNS/$task-$cond-XXXXXX")
    cp -r "$HERE/fixture/." "$dir/" || fail "copy fixture"
    ( cd "$dir" && git init -q -b main && git add -A \
      && git -c user.name=bench -c user.email=bench@example.invalid commit -qm fixture ) >>"$LOG" 2>&1 || fail "git init"
    args=(-p "$(cat "$HERE/tasks/$task/prompt.md")" --model "$MODEL" --session-id "$sid" --allowedTools $TOOLS)
    if [ "$cond" = delegate ]; then args+=(--append-system-prompt "$DELEGATE_PROMPT" --settings '{"enabledPlugins":{"brood@brood":false}}')
    elif [ "$cond" = without ]; then args+=(--settings '{"enabledPlugins":{"brood@brood":false}}')
    elif [ -n "$PLUGIN_DIR" ]; then args+=(--plugin-dir "$PLUGIN_DIR" --settings '{"enabledPlugins":{"brood@brood":false}}'); fi
    echo "== $task/$cond rep $rep session $sid dir $dir" >>"$LOG"
    start=$(date +%s.%N)
    ( cd "$dir" && env -u CLAUDE_CODE_SESSION_ID -u CLAUDE_PID -u CLAUDECODE \
        timeout --kill-after=15 "$TIMEOUT" claude "${args[@]}" \
        < /dev/null ) >"$LOG.run" 2>&1
    rc=$?
    cat "$LOG.run" >>"$LOG"
    wall=$(python3 -c "import time;print(time.time()-$start)")
    if grep -qiE "usage limit|weekly limit|hit your .*limit|rate limit" "$LOG.run" \
       || { [ $rc -ne 0 ] && [ "${wall%.*}" -lt 15 ]; }; then
      rm -rf "$dir" "$LOG.run"; echo "ABORT: usage limit or instant failure at $task/$cond rep $rep (rc=$rc, ${wall%.*}s); partial results in $OUT" >&2; tail -n 5 "$LOG" >&2; exit 3
    fi
    to=(); [ $rc -eq 124 ] || [ $rc -eq 137 ] && to=(--timed-out)
    ( cd "$dir" && bash "$HERE/tasks/$task/verify.sh" ) >"$LOG.v" 2>&1
    vrc=$?
    cat "$LOG.v" >>"$LOG"
    python3 "$HERE/score.py" run --session "$sid" --run-dir "$dir" --task "$task" --condition "$cond" \
      --rep "$rep" --main-model "$MODEL" --wall "$wall" --claude-rc $rc --verify-rc $vrc --verify-log "$LOG.v" "${to[@]}" \
      --out "$OUT/$task-$cond-$rep.json" >>"$LOG" 2>&1 || fail "score $task/$cond"
    rm -rf "$dir" "$LOG.run" "$LOG.v"
    echo "done $task/$cond rep $rep: verify=$vrc claude=$rc wall=${wall%.*}s"
  done
 done
done
python3 "$HERE/score.py" summary "$OUT"
echo "results: $OUT   log: $LOG"
