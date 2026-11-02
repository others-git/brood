#!/usr/bin/env bash
set -u
python3 -m unittest discover -s tests >/dev/null 2>&1 || { echo "suite fails"; exit 1; }
if grep -rnw 'Item' shelf tests docs README.md --include='*' 2>/dev/null | grep -v __pycache__ | grep .; then
  echo "old name Item still present"; exit 1
fi
python3 - <<'P' || exit 1
import shelf.models as m
assert hasattr(m, "Product") and not hasattr(m, "Item")
p = m.Product("S", "n", 150, 2)
assert p.value_cents() == 300 and p.category == "general"
from shelf.store import Store
s = Store.load("data/items.json")
assert s.total_value_cents() == 58895
P
out=$(python3 -m shelf.cli --data data/items.json invoice A100:2 --code SAVE10) || exit 1
echo "$out" | grep -q 'DUE' || { echo "cli broken"; exit 1; }
n_before=$(git show "$(git rev-list --max-parents=0 HEAD | tail -1)":tests/test_basic.py | grep -c "def test_")
[ "$(grep -rh 'def test_' tests | wc -l)" -ge "$n_before" ] || { echo "tests removed"; exit 1; }
git diff --quiet "$(git rev-list --max-parents=0 HEAD | tail -1)" -- data || { echo "data changed"; exit 1; }
