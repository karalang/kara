#!/usr/bin/env python3
"""Median wall + rusage timing for a command. Stands in for hyperfine (absent here)."""
import os, subprocess, sys, time, statistics, json

def run_one(cmd, env):
    t0 = time.monotonic()
    fa = [(os.POSIX_SPAWN_OPEN, 1, os.devnull, os.O_WRONLY, 0o666),
          (os.POSIX_SPAWN_DUP2, 1, 2)]
    pid = os.posix_spawn(cmd[0], cmd, env, file_actions=fa)
    _, status, ru = os.wait4(pid, 0)
    wall = (time.monotonic() - t0) * 1000.0
    if status != 0:
        raise SystemExit(f"command failed status={status}: {cmd}")
    return wall, ru.ru_utime * 1000.0, ru.ru_stime * 1000.0

def bench(cmd, runs=30, warmup=5, extra_env=None):
    env = dict(os.environ)
    if extra_env:
        env.update(extra_env)
    for _ in range(warmup):
        run_one(cmd, env)
    walls, users, syss = [], [], []
    for _ in range(runs):
        w, u, s = run_one(cmd, env)
        walls.append(w); users.append(u); syss.append(s)
    med = statistics.median(walls)
    return {
        "median_ms": round(med, 3),
        "mean_ms": round(statistics.mean(walls), 3),
        "min_ms": round(min(walls), 3),
        "max_ms": round(max(walls), 3),
        "stddev_ms": round(statistics.pstdev(walls), 3),
        "user_ms": round(statistics.median(users), 2),
        "system_ms": round(statistics.median(syss), 2),
        "cpu_pct": round(100.0 * (statistics.median(users) + statistics.median(syss)) / med, 1),
        "runs": runs,
    }

if __name__ == "__main__":
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", type=int, default=30)
    ap.add_argument("--warmup", type=int, default=5)
    ap.add_argument("--label", default="")
    ap.add_argument("--env", action="append", default=[])
    ap.add_argument("cmd", nargs="+")
    a = ap.parse_args()
    ev = dict(kv.split("=", 1) for kv in a.env)
    r = bench([os.path.abspath(a.cmd[0])] + a.cmd[1:], a.runs, a.warmup, ev)
    r["label"] = a.label or a.cmd[0]
    r["env_overrides"] = ev
    print(json.dumps(r))
