#!/usr/bin/env bash
# cwd: the fixture copy
set -u
python3 -m unittest discover -s tests >/dev/null 2>&1 || { echo "suite fails"; exit 1; }
out=$(python3 -m shelf.cli --data data/items.json low-stock --threshold 3 2>&1) || { echo "cli fails: $out"; exit 1; }
python3 - "$out" <<'P' || exit 1
import sys, re
out = sys.argv[1]
lines = out.strip().splitlines()
# expected: C200 (0), B200 (2), A200 (3), in that order; B100 (12) excluded
pos = [out.find(s) for s in ("C200", "B200", "A200")]
assert all(p >= 0 for p in pos) and pos == sorted(pos), f"order/content wrong: {out}"
assert "B100" not in out and "A100" not in out, out
assert re.search(r"LOW\s*3", lines[-1]), lines[-1]
P
python3 - <<'P' || exit 1
from shelf.store import Store
s = Store.load("data/items.json")
assert [i.sku for i in s.low_stock(2)] == ["C200", "B200"]
assert s.low_stock(-1) == []
P
n_before=$(git show "$(git rev-list --max-parents=0 HEAD | tail -1)":tests/test_basic.py | grep -c "def test_")
n_new=$(grep -rh "def test_" tests | wc -l)
[ "$n_new" -ge $((n_before + 3)) ] || { echo "fewer than 3 new tests"; exit 1; }
