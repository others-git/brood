#!/usr/bin/env bash
set -u
f=$(ls docs/proposal-*.md 2>/dev/null | head -1)
[ -n "$f" ] || { echo "no docs/proposal-*.md"; exit 1; }
words=$(wc -w < "$f")
[ "$words" -ge 150 ] || { echo "too short: $words words"; exit 1; }
[ "$words" -le 1500 ] || { echo "too long: $words words"; exit 1; }
heads=$(grep -c '^#' "$f")
[ "$heads" -ge 3 ] || { echo "fewer than 3 headings"; exit 1; }
grep -qi 'csv' "$f" || { echo "no csv"; exit 1; }
grep -qE 'store(\.py)?' "$f" || { echo "does not mention store"; exit 1; }
grep -qiE 'alternativ|rejected' "$f" || { echo "no alternatives"; exit 1; }
grep -qiE 'dry.?run' "$f" || { echo "no dry-run"; exit 1; }
git diff --quiet "$(git rev-list --max-parents=0 HEAD | tail -1)" -- shelf tests data || { echo "source was modified"; exit 1; }
