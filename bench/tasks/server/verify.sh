#!/usr/bin/env bash
set -u
python3 - <<'P' || exit 1
import json
d = json.load(open("out/summary.json"))
from shelf.store import Store
s = Store.load("data/items.json")
assert d == {"count": len(s.all()), "total_value_cents": s.total_value_cents()}, d
P
