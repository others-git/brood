#!/usr/bin/env python3
"""Score one benchmark run, or summarise a results directory. Stdlib only.

  score.py run  --session ID --run-dir DIR --task T --condition C --rep N
                --wall S --claude-rc N --verify-rc N [--timed-out] [--out FILE]
  score.py summary RESULTS_DIR
"""
import argparse
import glob
import json
import os
import signal
import subprocess
import sys
import time
from collections import defaultdict

HOME = os.path.expanduser("~")
FIELDS = ("input", "output", "cache_read", "cache_write")


def read_jsonl(path):
    out = []
    with open(path, errors="replace") as f:
        for line in f:
            line = line.strip()
            if line:
                try:
                    out.append(json.loads(line))
                except ValueError:
                    pass
    return out


def assistant_msgs(path):
    """Assistant messages of one transcript, deduplicated by message id.
    Claude Code writes one line per content block; usage repeats, output grows,
    so keep the entry with the largest output_tokens."""
    best, order = {}, []
    for d in read_jsonl(path):
        if d.get("type") != "assistant":
            continue
        m = d.get("message") or {}
        u = m.get("usage")
        if not u or m.get("model") in (None, "<synthetic>"):
            continue
        key = m.get("id") or d.get("uuid")
        if key not in best:
            order.append(key)
        if key not in best or u.get("output_tokens", 0) >= best[key]["usage"].get("output_tokens", 0):
            best[key] = {"model": m["model"], "usage": u}
    return [best[k] for k in order]


def usage_tuple(u):
    return {"input": u.get("input_tokens", 0) or 0,
            "output": u.get("output_tokens", 0) or 0,
            "cache_read": u.get("cache_read_input_tokens", 0) or 0,
            "cache_write": u.get("cache_creation_input_tokens", 0) or 0}


def transcripts(session):
    mains = glob.glob(os.path.join(HOME, ".claude", "projects", "*", session + ".jsonl"))
    if not mains:
        return None, []
    main = mains[0]
    subs = sorted(glob.glob(os.path.join(main[:-6], "subagents", "*.jsonl")))
    return main, subs


def token_stats(session):
    main, subs = transcripts(session)
    if main is None:
        return None
    tokens = defaultdict(lambda: dict.fromkeys(FIELDS, 0))
    roles = defaultdict(lambda: dict.fromkeys(FIELDS, 0))
    peak = 0
    main_msgs = assistant_msgs(main)
    for m in main_msgs:
        t = usage_tuple(m["usage"])
        for k in FIELDS:
            tokens[m["model"]][k] += t[k]
            roles["main"][k] += t[k]
        peak = max(peak, t["input"] + t["cache_read"] + t["cache_write"])
    agents = []
    for p in subs:
        meta = {}
        try:
            meta = json.load(open(p[:-6] + ".meta.json"))
        except (OSError, ValueError):
            pass
        models = defaultdict(int)
        role = meta.get("agentType", "?")
        for m in assistant_msgs(p):
            t = usage_tuple(m["usage"])
            for k in FIELDS:
                tokens[m["model"]][k] += t[k]
                roles[role][k] += t[k]
            models[m["model"]] += 1
        model = max(models, key=models.get) if models else None
        agents.append({"type": meta.get("agentType", "?"), "model": model,
                       "id": os.path.basename(p)[:-6]})
    return {"main_transcript": main, "unfinished_agents": sum(unfinished(p) for p in subs),
            "failed_merges": failed_merges([main] + subs), "brood_merge_calls": count_cmd(main, "brood merge"), "main_messages": len(main_msgs),
            "tokens": {k: dict(v) for k, v in tokens.items()},
            "tokens_by_role": {k: dict(v) for k, v in roles.items()},
            "main_peak_context": peak, "agents": agents}


MERGE_FAIL = ("CONFLICT", "Automatic merge failed", "merge failed", "Merge conflict")


def tool_result_texts(d):
    c = (d.get("message") or {}).get("content")
    if d.get("type") != "user" or not isinstance(c, list):
        return
    for b in c:
        if isinstance(b, dict) and b.get("type") == "tool_result":
            x = b.get("content")
            if isinstance(x, list):
                x = " ".join(i.get("text", "") for i in x if isinstance(i, dict))
            yield x if isinstance(x, str) else ""


def failed_merges(paths):
    n = 0
    for p in paths:
        for d in read_jsonl(p):
            for t in tool_result_texts(d):
                if any(m in t for m in MERGE_FAIL) and "git" in t.lower() or "Automatic merge failed" in t:
                    n += 1
    return n


def count_cmd(path, needle):
    seen = set()
    for d in read_jsonl(path):
        c = (d.get("message") or {}).get("content")
        if d.get("type") == "assistant" and isinstance(c, list):
            for b in c:
                if b.get("type") == "tool_use" and b.get("name") == "Bash" and needle in (b.get("input") or {}).get("command", ""):
                    seen.add(b.get("id"))
    return len(seen)


def unfinished(path):
    """True if the transcript does not end on a completed assistant turn."""
    last = None
    for d in read_jsonl(path):
        if d.get("type") in ("assistant", "user"):
            last = d
    if last is None:
        return True
    return not (last.get("type") == "assistant" and (last.get("message") or {}).get("stop_reason") == "end_turn")


def refusals(session):
    p = os.path.join(HOME, ".local", "share", "brood", "sessions", session, "brood.log")
    try:
        return sum(1 for l in open(p, errors="replace") if " deny " in l)
    except OSError:
        return 0


def find_leftovers(session, run_dir):
    """Live processes that carry the session id in their environment, or whose cwd is
    inside the run dir. Returns [(pid, cmdline)]."""
    me = {os.getpid(), os.getppid()}
    tag = f"CLAUDE_CODE_SESSION_ID={session}".encode()
    found = []
    for pid_s in os.listdir("/proc"):
        if not pid_s.isdigit() or int(pid_s) in me:
            continue
        base = f"/proc/{pid_s}"
        try:
            env = open(base + "/environ", "rb").read().split(b"\0")
            hit = tag in env
            if not hit:
                cwd = os.readlink(base + "/cwd")
                hit = cwd == run_dir or cwd.startswith(run_dir + "/")
            if hit:
                cmd = open(base + "/cmdline", "rb").read().replace(b"\0", b" ").decode(errors="replace").strip()
                if open(base + "/stat").read().rsplit(")", 1)[1].split()[0] == "Z":
                    continue
                found.append((int(pid_s), cmd[:200]))
        except OSError:
            continue
    return found


def kill_all(pids):
    for sig in (signal.SIGTERM, signal.SIGKILL):
        for pid in pids:
            try:
                os.kill(pid, sig)
            except OSError:
                pass
        time.sleep(1)


def git(run_dir, *a):
    r = subprocess.run(["git", "-C", run_dir, *a], capture_output=True, text=True)
    return r.stdout.splitlines() if r.returncode == 0 else []


def cmd_run(a):
    time.sleep(2)  # let session-end cleanup finish before looking
    left = find_leftovers(a.session, a.run_dir)
    kill_all([p for p, _ in left])
    worktrees = [l for l in git(a.run_dir, "worktree", "list") if l.split()[0] != a.run_dir][:]
    branches = [b.strip(" *+") for b in git(a.run_dir, "branch", "--format=%(refname:short)")]
    branches = [b for b in branches if b != "main"]
    res = {"main_model": a.main_model, "task": a.task, "condition": a.condition, "rep": a.rep, "session": a.session,
           "verify_passed": a.verify_rc == 0, "success": a.verify_rc == 0 and not a.timed_out, "verify_rc": a.verify_rc,
           "claude_rc": a.claude_rc, "timed_out": a.timed_out, "wall_s": round(a.wall, 1),
           "leftover_processes": [{"pid": p, "cmd": c} for p, c in left],
           "leftover_worktrees": len(worktrees), "leftover_branches": len(branches),
           "refusals": refusals(a.session) if a.condition == "with" else 0}
    try:
        res["verify_tail"] = open(a.verify_log, errors="replace").read().splitlines()[-30:] if a.verify_log else []
    except OSError:
        res["verify_tail"] = []
    ts = token_stats(a.session)
    if ts is None:
        res["error"] = "transcript not found"
        ts = {"tokens": {}, "main_peak_context": 0, "agents": [], "main_messages": 0}
    res.update(ts)
    out = json.dumps(res, indent=2)
    if a.out:
        with open(a.out, "w") as f:
            f.write(out + "\n")
    return res


def mean(xs):
    return sum(xs) / len(xs) if xs else 0


def k(n):
    return f"{n/1000:.1f}k"


def cmd_summary(d):
    runs = [json.load(open(p)) for p in sorted(glob.glob(os.path.join(d, "*.json")))]
    if not runs:
        print("no results in", d)
        return 1
    groups = defaultdict(list)
    for r in runs:
        groups[(r["task"], r["condition"])].append(r)
    mm = sorted({r.get("main_model", "") for r in runs})
    print("main model:", ", ".join(mm))
    print(f"{'task':10} {'cond':7} {'ok':>5} {'wall_s':>7} {'peak':>7} {'agents':>6} {'left':>5} {'wt/br':>6} {'unfin':>5} {'mrgF':>4} {'deny':>4}  tokens per model (in/out/cr/cw, means)")
    for (task, cond), rs in sorted(groups.items()):
        ok = sum(r["success"] for r in rs)
        models = sorted({m for r in rs for m in r["tokens"]})
        parts = []
        for m in models:
            t = [mean([r["tokens"].get(m, {}).get(f, 0) for r in rs]) for f in FIELDS]
            parts.append(f"{m.replace('claude-', '')}: " + "/".join(k(x) for x in t))
        print(f"{task:10} {cond:7} {ok}/{len(rs):<3} {mean([r['wall_s'] for r in rs]):7.0f} "
              f"{k(mean([r['main_peak_context'] for r in rs])):>7} "
              f"{mean([len(r['agents']) for r in rs]):6.1f} "
              f"{sum(len(r['leftover_processes']) for r in rs):5d} "
              f"{sum(r['leftover_worktrees'] for r in rs)}/{sum(r['leftover_branches'] for r in rs):<4} "
              f"{sum(r.get('unfinished_agents', 0) for r in rs):5d} {sum(r.get('failed_merges', 0) for r in rs):4d} "
              f"{sum(r['refusals'] for r in rs):4d}  " + "; ".join(parts))
    tot = defaultdict(lambda: dict.fromkeys(FIELDS, 0))
    for r in runs:
        for m, t in r["tokens"].items():
            for f in FIELDS:
                tot[m][f] += t[f]
    print("\ntokens by role, means per run (in/out/cache_read/cache_write):")
    rg = defaultdict(list)
    for r in runs:
        rg[(r["condition"], r["task"])].append(r)
    for cond in sorted({r["condition"] for r in runs}):
        rs = [r for r in runs if r["condition"] == cond]
        roles = sorted({x for r in rs for x in r.get("tokens_by_role", {})})
        for x in roles:
            t = [sum(r.get("tokens_by_role", {}).get(x, {}).get(f, 0) for r in rs) / len(rs) for f in FIELDS]
            print(f"  {cond:7} {x:20} " + "/".join(k(v) for v in t))
    print("\ntotal tokens, all runs (in/out/cache_read/cache_write):")
    for m, t in sorted(tot.items()):
        print(f"  {m}: " + " / ".join(str(t[f]) for f in FIELDS))
    return 0


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="mode", required=True)
    r = sub.add_parser("run")
    for n in ("session", "run-dir", "task", "condition"):
        r.add_argument("--" + n, required=True)
    r.add_argument("--rep", type=int, default=1)
    r.add_argument("--main-model", default="")
    r.add_argument("--wall", type=float, default=0)
    r.add_argument("--claude-rc", type=int, default=0)
    r.add_argument("--verify-rc", type=int, default=1)
    r.add_argument("--timed-out", action="store_true")
    r.add_argument("--out")
    r.add_argument("--verify-log")
    s = sub.add_parser("summary")
    s.add_argument("dir")
    a = ap.parse_args()
    if a.mode == "run":
        a.run_dir = os.path.realpath(a.run_dir)
        cmd_run(a)
        return 0
    return cmd_summary(a.dir)


if __name__ == "__main__":
    sys.exit(main())
