#!/usr/bin/env bash
set -u
python3 -m unittest discover -s tests >/dev/null 2>&1 || { echo "suite fails"; exit 1; }
python3 - <<'P' || exit 1
from shelf.utils import format_money
assert format_money(1234567) == "$12,345.67"
assert format_money(99) == "$0.99"
assert format_money(-100000) == "-$1,000.00"
from shelf.store import Store
s = Store.load("data/items.json")
s.remove("A100")
try:
    s.get("A100"); raise SystemExit("not removed")
except KeyError:
    pass
try:
    s.remove("ZZZ"); raise SystemExit("no KeyError")
except KeyError:
    pass
P
cp data/items.json /tmp/_items.$$ 2>/dev/null
python3 -m shelf.cli --data data/items.json remove B200 >/dev/null 2>&1 || { echo "cli remove fails"; mv /tmp/_items.$$ data/items.json; exit 1; }
grep -q B200 data/items.json && { echo "B200 still in file"; exit 1; }
grep -q A100 data/items.json || { echo "other items lost"; exit 1; }
n_before=$(git show "$(git rev-list --max-parents=0 HEAD | tail -1)":tests/test_basic.py | grep -c "def test_")
n_new=$(grep -rh "def test_" tests | wc -l)
[ "$n_new" -ge $((n_before + 2)) ] || { echo "fewer than 2 new tests"; exit 1; }
