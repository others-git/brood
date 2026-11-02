#!/usr/bin/env python3
"""Estimate total_cost_usd for old results that lack it. Stdlib only.
usage: rescore_old.py CALIBRATION_DIR OLD_DIR OUT_DIR
Fits per-model USD rates (input, output, cache read, cache write 5m/1h) by least squares on the
modelUsage costUSD of CALIBRATION_DIR, then prices the transcript tokens of OLD_DIR. The cache
write TTL is not in old results, so the calibration's mean 1h share is used. Output marks cost.estimated."""
import glob, json, os, sys
from collections import defaultdict

def solve(A, y):
    n = len(A[0])
    M = [[sum(r[i] * r[j] for r in A) for j in range(n)] + [sum(r[i] * v for r, v in zip(A, y))] for i in range(n)]
    for i in range(n):
        p = max(range(i, n), key=lambda k: abs(M[k][i])); M[i], M[p] = M[p], M[i]
        for k in range(n):
            if k != i:
                f = M[k][i] / M[i][i]
                M[k] = [a - f * b for a, b in zip(M[k], M[i])]
    return [M[i][n] / M[i][i] for i in range(n)]

cal, old, out = sys.argv[1:4]
rows = defaultdict(list)
share = defaultdict(list)
for p in glob.glob(os.path.join(cal, "*.json")):
    r = json.load(open(p))
    for m, u in ((r.get("cost") or {}).get("modelUsage") or {}).items():
        cw = u["cacheCreationInputTokens"]
        if cw:
            share[m].append(u["cacheCreationEphemeral1hInputTokens"] / cw)
        rows[m].append(([u["inputTokens"], u["outputTokens"], u["cacheReadInputTokens"], cw], u["costUSD"]))
rates = {}
for m, rs in rows.items():
    if len(rs) >= 4:
        rates[m] = solve([x for x, _ in rs], [y for _, y in rs])
        err = [abs(sum(a * b for a, b in zip(rates[m], x)) - y) / y for x, y in rs]
        print(f"{m}: $/Mtok in/out/cr/cw = " + "/".join(f"{v*1e6:.2f}" for v in rates[m]) + f"  max fit err {max(err):.1%} n={len(rs)}")
os.makedirs(out, exist_ok=True)
for p in glob.glob(os.path.join(old, "*.json")):
    r = json.load(open(p))
    tot = 0
    for m, t in r["tokens"].items():
        if m not in rates:
            raise SystemExit(f"no rates for {m}")
        tot += sum(a * b for a, b in zip(rates[m], [t["input"], t["output"], t["cache_read"], t["cache_write"]]))
    r["cost"] = {"total_cost_usd": tot, "estimated": True}
    json.dump(r, open(os.path.join(out, os.path.basename(p)), "w"), indent=1)
