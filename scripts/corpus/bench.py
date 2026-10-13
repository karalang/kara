#!/usr/bin/env python3
"""Time the corpus benchmarks on the compiled MIR backend against legacy.

    python3 scripts/corpus/bench.py --karac target/release/karac
    python3 scripts/corpus/bench.py --karac target/release/karac --filter 233- --runs 5
    python3 scripts/corpus/bench.py --karac target/release/karac --apps --json bench.json

Each program is built twice, `karac build` (legacy, `KARAC_AUTO_PAR=0`, as the
corpus's legacy-build) and `karac build --mir`, and each binary is run
`--runs` times. Reported per program and per backend: the build's wall time,
the binary's size (as linked, and stripped), and the fastest run's wall time.
A run is counted only when its stdout is the entry's expected output (the
same file `run.py` holds the mir backends to); otherwise the program is listed
as failing on that backend and left out of the ratios.

The programs are the kata `bench` variants (`--filter` narrows them as in
`run.py`), or with `--apps` the programs under corpus/apps, which make their
own line: the redesign's bars are kata benchmarks within 1.5x of legacy and an
app line of its own. The summary gives, for run time, build time and size,
the geometric mean of mir/legacy over the programs both backends ran, how
many are within 1.5x, and the slowest ratios.

Timing is serial (one build or run at a time) so programs do not compete for
cores; `--jobs` builds in parallel when only sizes and correctness matter.
"""

import argparse
import concurrent.futures as cf
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from corpus_lib import ROOT, entries, read_toml  # noqa: E402

BUILDS = {
    "legacy": (["build"], {"KARAC_AUTO_PAR": "0"}),
    "mir": (["build", "--mir"], {}),
}


def expected(entry: Path) -> bytes | None:
    """The output a run must print: expected.out, else model.out, else legacy.out."""
    for name in ("expected.out", "model.out", "legacy.out"):
        if (entry / name).exists():
            return (entry / name).read_bytes()
    return None


def stripped_size(exe: Path, tmp: Path) -> int | None:
    copy = tmp / f"{exe.name}.stripped"
    shutil.copy(exe, copy)
    try:
        r = subprocess.run(["strip", str(copy)], capture_output=True)
    except FileNotFoundError:
        return None
    return copy.stat().st_size if r.returncode == 0 else None


def measure(entry: Path, backend: str, args, meta: dict) -> dict:
    flags, env_extra = BUILDS[backend]
    env = dict(os.environ)
    env.update(env_extra)
    env.update({"KARAC_HASH_SEED": "1", **{k: str(v) for k, v in meta.get("env", {}).items()}})
    want = expected(entry)
    with tempfile.TemporaryDirectory(prefix="bench-") as tmp:
        tmpd = Path(tmp)
        shutil.copy(entry / "source.kara", tmpd / "source.kara")
        start = time.monotonic()
        try:
            b = subprocess.run([args.karac, *flags, "source.kara"], cwd=tmpd, env=env,
                               capture_output=True, timeout=args.timeout, stdin=subprocess.DEVNULL)
        except subprocess.TimeoutExpired:
            return {"status": "build timeout"}
        build = time.monotonic() - start
        exe = tmpd / "source"
        if b.returncode != 0 or not exe.exists():
            line = (b.stderr or b.stdout).decode(errors="replace").strip().splitlines()
            return {"status": "build failed", "why": line[0][:160] if line else ""}
        out = {"status": "ok", "build_s": build, "size": exe.stat().st_size,
               "stripped": stripped_size(exe, tmpd)}
        best = None
        for _ in range(args.runs):
            start = time.monotonic()
            try:
                r = subprocess.run([str(exe)], cwd=tmpd, env=env, capture_output=True,
                                   timeout=args.timeout, stdin=subprocess.DEVNULL)
            except subprocess.TimeoutExpired:
                return {**out, "status": "run timeout"}
            secs = time.monotonic() - start
            if r.returncode != int(meta.get("exit", 0)) or (want is not None and r.stdout != want):
                return {**out, "status": "wrong output", "why": f"exit {r.returncode}"}
            best = secs if best is None else min(best, secs)
        out["run_s"] = best
        return out


def geomean(xs: list[float]) -> float:
    return math.exp(sum(math.log(x) for x in xs) / len(xs)) if xs else float("nan")


def summarize(label: str, rows: list[dict], key: str, unit: str) -> list[str]:
    pairs = [(r["name"], r["mir"][key] / r["legacy"][key]) for r in rows
             if r["mir"].get("status") == "ok" and r["legacy"].get("status") == "ok"
             and r["mir"].get(key) and r["legacy"].get(key)]
    if not pairs:
        return [f"{label} {key}: no program ran on both backends"]
    ratios = [p for _, p in pairs]
    within = sum(1 for p in ratios if p <= 1.5)
    worst = sorted(pairs, key=lambda p: -p[1])[:5]
    lines = [f"{label} {key} ({unit}): mir/legacy geomean {geomean(ratios):.2f}x over {len(pairs)}, "
             f"{within} within 1.5x ({100 * within / len(pairs):.0f}%)"]
    lines += [f"    {ratio:6.2f}x  {name}" for name, ratio in worst if ratio > 1.5]
    return lines


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--karac", required=True, help="a karac built with --features llvm (absolute or relative)")
    ap.add_argument("--corpus", default=str(ROOT / "corpus"))
    ap.add_argument("--filter", action="append", default=[], help="substring of the entry path")
    ap.add_argument("--apps", action="store_true", help="time corpus/apps instead of the kata benches")
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--timeout", type=float, default=120.0)
    ap.add_argument("--jobs", type=int, default=1)
    ap.add_argument("--json", help="write every measurement here")
    args = ap.parse_args()
    args.karac = str(Path(args.karac).resolve())
    corpus = Path(args.corpus)
    picked = []
    for e in entries(corpus):
        rel = str(e.relative_to(corpus))
        meta = read_toml(e / "meta.toml")
        if meta.get("expect", "stdout") != "stdout":
            continue
        if args.apps != rel.startswith("apps/"):
            continue
        if not args.apps and "bench" not in meta.get("tags", []):
            continue
        if args.filter and not any(f in rel for f in args.filter):
            continue
        picked.append((e, rel, meta))
    if not picked:
        print("bench: no program matched", file=sys.stderr)
        return 2

    def one(item):
        e, rel, meta = item
        return {"name": rel, **{b: measure(e, b, args, meta) for b in BUILDS}}

    with cf.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        rows = []
        for row in pool.map(one, picked):
            rows.append(row)
            cells = []
            for b in BUILDS:
                m = row[b]
                cells.append(f"{b} {m['run_s']:.3f}s build {m['build_s']:.1f}s {m['size'] // 1024}K"
                             if m.get("status") == "ok" else f"{b} {m['status']}")
            print(f"{row['name']}: " + " | ".join(cells), flush=True)

    label = "apps" if args.apps else "bench"
    print()
    for key, unit in (("run_s", "fastest run"), ("build_s", "build wall time"),
                      ("size", "linked bytes"), ("stripped", "stripped bytes")):
        print("\n".join(summarize(label, rows, key, unit)))
    for b in BUILDS:
        bad = [r["name"] for r in rows if r[b].get("status") != "ok"]
        if bad:
            print(f"{label} {b}: {len(bad)} of {len(rows)} did not run correctly")
            for r in rows:
                if r[b].get("status") != "ok":
                    print(f"    {r[b]['status']}: {r['name']} {r[b].get('why', '')}".rstrip())
    if args.json:
        Path(args.json).write_text(json.dumps(rows, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
