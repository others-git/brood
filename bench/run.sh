#!/usr/bin/env bash
# A/B benchmark: Claude Code with brood vs without. See bench/README.md.
# usage: bench/run.sh [--reps N] [--rep-from N] [--solo-reps N] [--tasks a,b] [--conditions with,delegate,solo] [--main-model alias]
# Resumable: a run whose result JSON exists is skipped. Condition order is shuffled per rep and task.
set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPS=1
REP_FROM=1
SOLO_REPS=
TASKS=$(ls "$HERE/tasks" | paste -sd,)
CONDS=with,delegate,solo
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
    --rep-from) REP_FROM=$2; shift 2 ;;
    --solo-reps) SOLO_REPS=$2; shift 2 ;;
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

for c in ${CONDS//,/ }; do case $c in with|delegate|solo) ;; *) fail "bad condition $c" ;; esac; done
for t in ${TASKS//,/ }; do [ -f "$HERE/tasks/$t/prompt.md" ] || fail "no task $t"; done
command -v claude >/dev/null || fail "claude not on PATH"

MTAG=${MODEL//[^a-z0-9]/}
for rep in $(seq "$REP_FROM" "$REPS"); do
 for task in ${TASKS//,/ }; do
  for cond in $(for c in ${CONDS//,/ }; do echo "$c"; done | shuf); do
    [ "$cond" = solo ] && [ -n "$SOLO_REPS" ] && [ "$rep" -gt "$SOLO_REPS" ] && continue
    RES="$OUT/$task-$cond-$MTAG-$rep.json"
    [ -f "$RES" ] && { echo "skip $task/$cond/$MODEL rep $rep (done)"; continue; }
    sid=$(python3 -c 'import uuid;print(uuid.uuid4())')
    dir=$(mktemp -d "$RUNS/$task-$cond-XXXXXX")
    cp -r "$HERE/fixture/." "$dir/" || fail "copy fixture"
    ( cd "$dir" && git init -q -b main && git add -A \
      && git -c user.name=bench -c user.email=bench@example.invalid commit -qm fixture ) >>"$LOG" 2>&1 || fail "git init"
    args=(-p "$(cat "$HERE/tasks/$task/prompt.md")" --model "$MODEL" --session-id "$sid" --allowedTools $TOOLS --output-format json)
    if [ "$cond" = delegate ]; then args+=(--append-system-prompt "$DELEGATE_PROMPT" --settings '{"enabledPlugins":{"brood@brood":false}}')
    elif [ "$cond" = solo ]; then args+=(--settings '{"enabledPlugins":{"brood@brood":false}}')
    elif [ -n "$PLUGIN_DIR" ]; then args+=(--plugin-dir "$PLUGIN_DIR" --settings '{"enabledPlugins":{"brood@brood":false}}'); fi
    echo "== $task/$cond rep $rep session $sid dir $dir" >>"$LOG"
    start=$(date +%s.%N)
    ( cd "$dir" && env -u CLAUDE_CODE_SESSION_ID -u CLAUDE_PID -u CLAUDECODE \
        timeout --kill-after=15 "$TIMEOUT" claude "${args[@]}" \
        < /dev/null ) >"$LOG.json" 2>"$LOG.run"
    rc=$?
    cat "$LOG.run" "$LOG.json" >>"$LOG"
    wall=$(python3 -c "import time;print(time.time()-$start)")
    if grep -qiE "usage limit|weekly limit|hit your .*limit|rate limit" "$LOG.json" \
       || { [ $rc -ne 0 ] && [ "${wall%.*}" -lt 15 ]; }; then
      rm -rf "$dir" "$LOG.run" "$LOG.json"; echo "ABORT: usage limit or instant failure at $task/$cond rep $rep (rc=$rc, ${wall%.*}s); partial results in $OUT" >&2; tail -n 5 "$LOG" >&2; exit 3
    fi
    to=(); [ $rc -eq 124 ] || [ $rc -eq 137 ] && to=(--timed-out)
    ( cd "$dir" && bash "$HERE/tasks/$task/verify.sh" ) >"$LOG.v" 2>&1
    vrc=$?
    cat "$LOG.v" >>"$LOG"
    python3 "$HERE/score.py" run --session "$sid" --run-dir "$dir" --task "$task" --condition "$cond" \
      --rep "$rep" --main-model "$MODEL" --wall "$wall" --claude-rc $rc --verify-rc $vrc --verify-log "$LOG.v" --result-json "$LOG.json" "${to[@]}" \
      --out "$RES" >>"$LOG" 2>&1 || fail "score $task/$cond"
    rm -rf "$dir" "$LOG.run" "$LOG.v" "$LOG.json"
    echo "done $task/$cond/$MODEL rep $rep: verify=$vrc claude=$rc wall=${wall%.*}s"
  done
 done
done
python3 "$HERE/score.py" summary "$OUT" || true
echo "results: $OUT   log: $LOG"
