#!/usr/bin/env bash
# Release brood: tools/release.sh X.Y.Z [commit-message-file]
set -euo pipefail

REPO=others-git/brood
LOG="${TMPDIR:-/tmp}/brood-release.log"
CO_AUTHOR='Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>'

ver="${1:-}"
msgfile="${2:-}"
[[ "$ver" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "usage: $0 X.Y.Z [msgfile]" >&2; exit 2; }
cd "$(dirname "$0")/.."
tag="v$ver"
: >"$LOG"
step=init
trap 'rc=$?; if [ $rc -ne 0 ]; then echo "FAILED at step: $step" >&2; grep -iE "error|fail|panick|denied|fatal" "$LOG" >&2 || tail -n 30 "$LOG" >&2; echo "log: $LOG" >&2; fi' EXIT

run() { "$@" >>"$LOG" 2>&1; }
ok() { echo "ok  $step: $1"; }

step=preflight
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null || git ls-remote --exit-code --tags origin "$tag" >/dev/null 2>&1; then
  echo "tag $tag already exists" | tee -a "$LOG"; exit 1
fi
ok "$tag is new"

step=version
sed -i -E '0,/^version = "[^"]*"/s//version = "'"$ver"'"/' Cargo.toml
sed -i -E '0,/"version": "[^"]*"/s//"version": "'"$ver"'"/' plugin/.claude-plugin/plugin.json
grep -q "^version = \"$ver\"" Cargo.toml && grep -q "\"version\": \"$ver\"" plugin/.claude-plugin/plugin.json
ok "Cargo.toml and plugin.json set to $ver"

step=build
run cargo build -q
ok "cargo build"
step=test
run cargo test -q
ok "cargo test"

step=commit
msg=$(mktemp)
if [ -n "$msgfile" ]; then cat "$msgfile" >"$msg"; else echo "Release $ver" >"$msg"; fi
grep -qF "$CO_AUTHOR" "$msg" || printf '\n%s\n' "$CO_AUTHOR" >>"$msg"
run git add -A
run git commit -F "$msg"
rm -f "$msg"
ok "$(git rev-parse --short HEAD)"

step=tag
run git tag -a "$tag" -m "brood $ver"
ok "$tag"

step=push
run git push origin main "$tag"
ok "pushed main and $tag"

step=ci
id=""
for _ in $(seq 1 13); do
  id=$(gh run list -R "$REPO" --workflow release.yml --limit 20 --json databaseId,headBranch --jq "[.[]|select(.headBranch==\"$tag\")][0].databaseId // empty" 2>>"$LOG" || true)
  [ -n "$id" ] && break
  sleep 5
done
[ -n "$id" ] || { echo "release run for $tag not found" | tee -a "$LOG"; exit 1; }
run gh run watch "$id" --exit-status -R "$REPO"
ok "run $id passed: $(gh run view "$id" -R "$REPO" --json jobs --jq '[.jobs[]|"\(.name)=\(.conclusion)"]|join(", ")')"

step=install
tmp=$(mktemp -d)
run gh release download "$tag" -R "$REPO" -p "brood-$tag-x86_64-unknown-linux-gnu.tar.gz" -D "$tmp"
run tar xzf "$tmp/brood-$tag-x86_64-unknown-linux-gnu.tar.gz" -C "$tmp"
mkdir -p "$HOME/.local/bin"
run install -m 755 "$tmp/brood-$tag-x86_64-unknown-linux-gnu/brood" "$HOME/.local/bin/brood"
rm -rf "$tmp"
got=$("$HOME/.local/bin/brood" --version 2>&1 | tee -a "$LOG")
[[ "$got" == *"$ver"* ]] || { echo "version mismatch: $got"; exit 1; }
ok "$got"

step=plugin
run claude plugin marketplace update brood
run claude plugin update brood@brood
ok "marketplace and plugin updated"

step=done
echo "release: https://github.com/$REPO/releases/tag/$tag"
echo "log: $LOG"
