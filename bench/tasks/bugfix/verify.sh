#!/usr/bin/env bash
set -u
python3 -m unittest discover -s tests >/dev/null 2>&1 || { echo "suite fails"; exit 1; }
python3 - <<'P' || exit 1
from shelf.models import Item
from shelf.pricing import invoice_total
lines = [(Item("X", "x", 1000, 1), 1)]
want = invoice_total(lines, "SAVE10")
assert want == 972, want
for c in ("save10", " SAVE10", "Save10 ", "\tsave10\n"):
    assert invoice_total(lines, c) == want, repr(c)
assert invoice_total(lines, "NOPE") == 1080
assert invoice_total(lines, None) == 1080
assert invoice_total(lines, "half") == 540
P
n_before=$(git show "$(git rev-list --max-parents=0 HEAD | tail -1)":tests/test_basic.py | grep -c "def test_")
n_new=$(grep -rh "def test_" tests | wc -l)
[ "$n_new" -ge $((n_before + 1)) ] || { echo "no regression test"; exit 1; }
