#!/usr/bin/env python3
"""Cost analysis of benchmark results. Stdlib only.
usage: analyze.py RESULTS_DIR [--iters 2000]
Primary metric: total_cost_usd from claude -p (includes subagents). Ratio = with / delegate."""
import glob
import json
import math
import os
import random
import statistics as st
import sys
from collections import defaultdict


def load(d):
    runs = []
    for p in sorted(glob.glob(os.path.join(d, "*.json"))):
        r = json.load(open(p))
        c = r.get("cost") or {}
        if c.get("total_cost_usd") is None or r.get("timed_out"):
            continue
        runs.append(r)
    return runs


def ratio_ci(a, b, rng, iters):
    pt = st.mean(a) / st.mean(b)
    xs = sorted(st.mean(rng.choices(a, k=len(a))) / st.mean(rng.choices(b, k=len(b))) for _ in range(iters))
    return pt, xs[int(0.025 * iters)], xs[int(0.975 * iters) - 1]


def geomean_ci(cells, rng, iters):
    """cells: list of (with_costs, delegate_costs)."""
    def gm(rs):
        return math.exp(st.mean(math.log(x) for x in rs))
    pt = gm([st.mean(a) / st.mean(b) for a, b in cells])
    xs = sorted(gm([st.mean(rng.choices(a, k=len(a))) / st.mean(rng.choices(b, k=len(b))) for a, b in cells])
                for _ in range(iters))
    return pt, xs[int(0.025 * iters)], xs[int(0.975 * iters) - 1]


def verdict(lo, hi):
    return "no clear difference" if lo <= 1.0 <= hi else ("brood cheaper" if hi < 1.0 else "brood costlier")


def main():
    d = sys.argv[1]
    iters = int(sys.argv[sys.argv.index("--iters") + 1]) if "--iters" in sys.argv else 2000
    rng = random.Random(1)
    runs = load(d)
    cell = defaultdict(list)
    for r in runs:
        cell[(r["main_model"], r["task"], r["condition"])].append(r)
    models = sorted({k[0] for k in cell})
    tasks = sorted({k[1] for k in cell})
    sd = lambda x: st.stdev(x) if len(x) > 1 else 0.0
    print("## Cost per run (USD, total_cost_usd incl. subagents): mean +- sd [n], pass rate\n")
    print("| main | task | with | delegate | solo | with/delegate (95% CI) | verdict |")
    print("|---|---|---|---|---|---|---|")
    paired = defaultdict(list)
    for m in models:
        for t in tasks:
            row, costs = [], {}
            for c in ("with", "delegate", "solo"):
                rs = cell.get((m, t, c), [])
                costs[c] = [r["cost"]["total_cost_usd"] for r in rs]
                if rs:
                    ok = sum(r["verify_passed"] for r in rs)
                    row.append(f"{st.mean(costs[c]):.3f} +- {sd(costs[c]):.3f} [{len(rs)}] {ok}/{len(rs)}")
                else:
                    row.append("-")
            if len(costs["with"]) >= 2 and len(costs["delegate"]) >= 2:
                pt, lo, hi = ratio_ci(costs["with"], costs["delegate"], rng, iters)
                paired[m].append((costs["with"], costs["delegate"]))
                rc, v = f"{pt:.2f} ({lo:.2f}-{hi:.2f})", verdict(lo, hi)
            else:
                rc, v = "-", "too few runs"
            print(f"| {m} | {t} | " + " | ".join(row) + f" | {rc} | {v} |")
    print("\n## Overall geometric-mean ratio with/delegate\n")
    allc = []
    for m in models + ["all"]:
        cells = allc if m == "all" else paired.get(m, [])
        if m != "all":
            allc += cells
        if cells:
            pt, lo, hi = geomean_ci(cells, rng, iters)
            print(f"- {m}: {pt:.2f} ({lo:.2f}-{hi:.2f}) over {len(cells)} task cells: {verdict(lo, hi)}")
    print("\n## Secondary (means per run)\n")
    print("| main | task | cond | wall s | main peak | agents | left procs | wt/br left | denies | brood merge calls |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for (m, t, c), rs in sorted(cell.items()):
        mean = lambda f: st.mean(f(r) for r in rs)
        print(f"| {m} | {t} | {c} | {mean(lambda r: r['wall_s']):.0f} | {mean(lambda r: r['main_peak_context'])/1000:.1f}k "
              f"| {mean(lambda r: len(r['agents'])):.1f} | {sum(len(r['leftover_processes']) for r in rs)} "
              f"| {sum(r['leftover_worktrees'] for r in rs)}/{sum(r['leftover_branches'] for r in rs)} "
              f"| {sum(r['refusals'] for r in rs)} | {sum(r.get('brood_merge_calls', 0) for r in rs)} |")
    print(f"\nruns analysed: {len(runs)}; total cost ${sum(r['cost']['total_cost_usd'] for r in runs):.2f}")


main()
