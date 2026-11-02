#!/usr/bin/env bash
set -u
python3 -m unittest discover -s tests >/dev/null 2>&1 || { echo "suite fails"; exit 1; }
python3 - <<'P' || exit 1
import json, threading, urllib.request
from http.server import HTTPServer
from shelf.models import Item
from shelf.store import Store
from shelf import server
i = Item("Z", "z", 100, 1)
assert i.reorder_level == 5
s = Store.load("data/items.json")
assert [x.sku for x in s.needs_reorder()] == ["A200", "C200"], [x.sku for x in s.needs_reorder()]
old = json.load(open("data/items.json"))
for d in old: d.pop("reorder_level", None)
json.dump(old, open("/tmp/_old_items.json", "w"))
assert len(Store.load("/tmp/_old_items.json").all()) == 6
h = HTTPServer(("127.0.0.1", 0), server.make_handler(s))
threading.Thread(target=h.serve_forever, daemon=True).start()
body = json.load(urllib.request.urlopen(f"http://127.0.0.1:{h.server_port}/reorder"))
assert sorted(d["sku"] for d in body) == ["A200", "C200"], body
assert all("qty" in d and "reorder_level" in d for d in body), body
h.shutdown()
P
out=$(python3 -m shelf.cli --data data/items.json reorder 2>&1) || { echo "cli fails: $out"; exit 1; }
echo "$out" | grep -q A200 && echo "$out" | grep -q C200 || { echo "cli output wrong: $out"; exit 1; }
echo "$out" | tail -1 | grep -qE 'REORDER\s*2' || { echo "no REORDER 2 line"; exit 1; }
echo "$out" | grep -q B100 && { echo "B100 should not be listed"; exit 1; }
grep -qi reorder docs/architecture.md || { echo "docs not updated"; exit 1; }
n_before=$(git show "$(git rev-list --max-parents=0 HEAD | tail -1)":tests/test_basic.py | grep -c "def test_")
n_new=$(grep -rh "def test_" tests | wc -l)
[ "$n_new" -ge $((n_before + 4)) ] || { echo "fewer than 4 new tests"; exit 1; }
