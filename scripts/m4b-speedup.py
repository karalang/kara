#!/usr/bin/env python3
"""M4b speedup harness: each program's `karac build` against a
`KARAC_AUTO_PAR=1` build of the same source, by the same karac.

The programs are the kata loop list (`docs/investigations/m4-kata-auto-par-loops.json`,
sources read from a kara-katas checkout) and the corpus apps tagged
`stmt-par`. Both builds of a program come from one karac binary; the
parallel build also runs with `KARAC_AUTO_PAR_DEBUG=1`, whose stderr names
the loops, statement groups and reductions it made tasks of.

Timing runs the two binaries interleaved (seq, par, seq, par, ...) after one
untimed warm-up each, and reports the median wall time and the median
CPU-per-wall of each. A ratio is seq/par: above 1 the parallel build is
faster. The M4b floor is 0.9: no program slower than 0.9x its own seq build.

    python3 scripts/m4b-speedup.py --karac target/release/karac \\
        --katas ../kara-katas --out /mnt/attach/project-files/review/m4b-speedup

Builds run up to `--jobs` at a time before any timing starts; timing runs one
binary at a time, so nothing else of the harness shares the cores with it.
"""

import argparse
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
import tomllib
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
LOOPS = REPO / "docs" / "investigations" / "m4-kata-auto-par-loops.json"
FLOOR = 0.9
# `auto-par: <what>: a, b` as `build_renamed_source` prints it.
DONE = re.compile(r"^auto-par: (loops as tasks|statements as tasks|reductions): (.*)$")


def git_sha(repo: Path) -> str:
    r = subprocess.run(["git", "-C", str(repo), "rev-parse", "--short=10", "HEAD"],
                       capture_output=True, text=True)
    dirty = subprocess.run(["git", "-C", str(repo), "status", "--porcelain", "--untracked-files=no"],
                           capture_output=True, text=True).stdout.strip()
    return r.stdout.strip() + ("-dirty" if dirty else "")


def programs(args) -> list[dict]:
    out = []
    loops = json.loads(LOOPS.read_text())
    for p in loops["programs"]:
        src = Path(args.katas) / p["program"]
        out.append({"name": f"kata {p['kata']}: {p['program']}", "kind": "kata", "kata": p["kata"],
                    "source": src, "cwd": src.parent, "expected": None,
                    "legacy_speedup": p.get("speedup")})
    for meta in sorted((REPO / "corpus" / "apps").glob("*/*/meta.toml")):
        m = tomllib.loads(meta.read_text())
        if "stmt-par" not in m.get("tags", []):
            continue
        entry = meta.parent
        exp = entry / "expected.out"
        out.append({"name": f"app {entry.relative_to(REPO / 'corpus')}", "kind": "app",
                    "source": entry / "source.kara", "cwd": None,
                    "expected": exp.read_bytes() if exp.exists() else None,
                    "env": {k: str(v) for k, v in m.get("env", {}).items()}})
    if args.filter:
        out = [p for p in out if any(f in p["name"] for f in args.filter)]
    return out


def build(karac: str, src: Path, out: Path, par: bool, timeout: float) -> dict:
    env = dict(os.environ)
    for k in ("KARAC_AUTO_PAR", "KARAC_AUTO_PAR_DEBUG"):
        env.pop(k, None)
    if par:
        env["KARAC_AUTO_PAR"] = "1"
        env["KARAC_AUTO_PAR_DEBUG"] = "1"
    start = time.monotonic()
    try:
        r = subprocess.run([karac, "build", str(src), "-o", str(out)], env=env, capture_output=True,
                           timeout=timeout, stdin=subprocess.DEVNULL)
    except subprocess.TimeoutExpired:
        return {"ok": False, "why": f"build timed out after {timeout:.0f}s", "secs": timeout}
    secs = time.monotonic() - start
    err = r.stderr.decode(errors="replace")
    sites: dict[str, list[str]] = {}
    for line in err.splitlines():
        m = DONE.match(line)
        if m:
            sites[m.group(1)] = [s.strip() for s in m.group(2).split(",")]
    kept = [line for line in err.splitlines() if line.startswith("auto-par:") and not DONE.match(line)]
    if r.returncode != 0 or not out.exists():
        tail = [line for line in err.splitlines() if line.strip()][-3:]
        return {"ok": False, "why": f"build exit {r.returncode}: {' | '.join(tail)}", "secs": secs}
    return {"ok": True, "secs": secs, "sites": sites, "declined": kept}


def run_once(binary: Path, cwd: Path, env: dict, timeout: float) -> dict:
    with tempfile.TemporaryFile() as so, tempfile.TemporaryFile() as se:
        start = time.monotonic()
        p = subprocess.Popen([str(binary)], cwd=cwd, env=env, stdout=so, stderr=se,
                             stdin=subprocess.DEVNULL)
        deadline = start + timeout
        while True:
            pid, status, ru = os.wait4(p.pid, os.WNOHANG)
            if pid:
                break
            if time.monotonic() > deadline:
                p.kill()
                os.wait4(p.pid, 0)
                return {"timeout": True, "wall": timeout}
            time.sleep(0.002)
        wall = time.monotonic() - start
        so.seek(0)
        se.seek(0)
        return {"timeout": False, "wall": wall, "cpu": ru.ru_utime + ru.ru_stime,
                "exit": os.waitstatus_to_exitcode(status), "stdout": so.read(), "stderr": se.read()}


def measure(prog: dict, seq: Path, par: Path, args) -> dict:
    env = dict(os.environ)
    for k in ("KARAC_AUTO_PAR", "KARAC_AUTO_PAR_DEBUG"):
        env.pop(k, None)
    env["KARAC_HASH_SEED"] = str(args.seed)
    env.update(prog.get("env", {}))
    with tempfile.TemporaryDirectory(prefix="m4b-run-") as tmp:
        cwd = prog["cwd"] or Path(tmp)
        load = os.getloadavg()[0]
        first = {}
        for arm, binary in (("seq", seq), ("par", par)):
            r = run_once(binary, cwd, env, args.timeout)
            if r["timeout"]:
                return {"verdict": "TIMEOUT", "why": f"{arm} warm-up over {args.timeout:.0f}s", "load": load}
            first[arm] = r
        walls = {"seq": [], "par": []}
        cpus = {"seq": [], "par": []}
        arms = (("seq", seq), ("par", par))
        for n in range(args.runs):
            # Alternate which arm goes first, so an order effect cancels.
            for arm, binary in arms if n % 2 == 0 else arms[::-1]:
                r = run_once(binary, cwd, env, args.timeout)
                if r["timeout"]:
                    return {"verdict": "TIMEOUT", "why": f"{arm} over {args.timeout:.0f}s", "load": load}
                walls[arm].append(r["wall"])
                cpus[arm].append(r["cpu"] / r["wall"] if r["wall"] else 0.0)
    s, p = statistics.median(walls["seq"]), statistics.median(walls["par"])
    same = first["seq"]["stdout"] == first["par"]["stdout"] and first["seq"]["exit"] == first["par"]["exit"]
    oracle = None
    if prog["expected"] is not None:
        oracle = first["seq"]["stdout"] == prog["expected"] and first["par"]["stdout"] == prog["expected"]
    return {"verdict": "RAN", "seq_s": round(s, 4), "par_s": round(p, 4),
            "ratio": round(s / p, 3) if p else None,
            "seq_spread": [round(min(walls["seq"]), 4), round(max(walls["seq"]), 4)],
            "par_spread": [round(min(walls["par"]), 4), round(max(walls["par"]), 4)],
            "seq_cpu_per_wall": round(statistics.median(cpus["seq"]), 2),
            "par_cpu_per_wall": round(statistics.median(cpus["par"]), 2),
            "exit": [first["seq"]["exit"], first["par"]["exit"]],
            "output_same": same, "matches_expected": oracle, "load_before": round(load, 2)}


def markdown(report: dict) -> str:
    rows = report["programs"]
    lines = [f"# M4b speedup: seq `karac build` vs `KARAC_AUTO_PAR=1`", "",
             f"- karac `{report['karac_sha']}`, kara-katas `{report['katas_sha']}`, "
             f"{report['nproc']} cores, {report['runs']} interleaved runs (medians), "
             f"`KARAC_HASH_SEED={report['seed']}`, started {report['started']}.",
             f"- Ratio is seq/par: above 1 the parallel build is faster. Floor {FLOOR}.", ""]
    ran = [r for r in rows if r.get("ratio") is not None]
    with_sites = [r for r in ran if r["par_build"].get("sites")]
    under = [r for r in ran if r["ratio"] < FLOOR]
    aa = [r["ratio"] for r in ran if r.get("identical_binary")]
    bad = [r for r in rows if r.get("output_same") is False or r.get("matches_expected") is False]
    if ran:
        gm = statistics.geometric_mean([r["ratio"] for r in ran])
        lines.append(f"- {len(ran)} of {len(rows)} programs timed; geomean ratio {gm:.3f}; "
                     f"{len(with_sites)} got at least one task site; {len(under)} under {FLOOR}; "
                     f"{len(bad)} with differing output.")
    if aa:
        lines.append(f"- Noise: {len(aa)} programs built to identical binaries (no task site); their "
                     f"ratios run {min(aa):.3f}..{max(aa):.3f}, geomean "
                     f"{statistics.geometric_mean(aa):.3f}. A ratio inside that band is not a result.")
    lines += ["", "| program | sites | seq s | par s | ratio | par cpu/wall | output |",
              "|---|---|---:|---:|---:|---:|---|"]
    for r in rows:
        pb = r.get("par_build", {})
        sites = "; ".join(f"{k.split()[0]} {len(v)}" for k, v in pb.get("sites", {}).items()) or "none"
        if r.get("identical_binary"):
            sites += " (same binary)"
        if r.get("ratio") is None:
            lines.append(f"| {r['name']} | {sites} | | | {r.get('verdict')} | | {r.get('why', '')} |")
            continue
        out = "same" if r["output_same"] else "DIFFERS"
        if r.get("matches_expected") is False:
            out += ", not expected.out"
        flag = " **under floor**" if r["ratio"] < FLOOR else ""
        lines.append(f"| {r['name']} | {sites} | {r['seq_s']} | {r['par_s']} | {r['ratio']}{flag} | "
                     f"{r['par_cpu_per_wall']} | {out} |")
    return "\n".join(lines) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--karac", default=str(REPO / "target" / "release" / "karac"))
    ap.add_argument("--katas", default=str(REPO.parent / "kara-katas"))
    ap.add_argument("--out", required=True, help="directory for m4b-speedup-<sha>.{json,md}")
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--jobs", type=int, default=2, help="builds at a time (timing is serial)")
    ap.add_argument("--timeout", type=float, default=120.0, help="seconds per run")
    ap.add_argument("--build-timeout", type=float, default=600.0)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--filter", action="append", help="substring of a program's name (repeatable)")
    args = ap.parse_args()

    progs = programs(args)
    if not progs:
        print("no program matched", file=sys.stderr)
        return 2
    report = {"karac": args.karac, "karac_sha": git_sha(REPO), "katas_sha": git_sha(Path(args.katas)),
              "nproc": os.cpu_count(), "runs": args.runs, "seed": args.seed, "floor": FLOOR,
              "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "programs": []}
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    stem = out / f"m4b-speedup-{report['karac_sha']}"
    work = Path(tempfile.mkdtemp(prefix="m4b-bins-"))
    try:
        def both(i_p):
            i, p = i_p
            return (build(args.karac, p["source"], work / f"{i}.seq", False, args.build_timeout),
                    build(args.karac, p["source"], work / f"{i}.par", True, args.build_timeout))

        with ThreadPoolExecutor(args.jobs) as pool:
            builds = list(pool.map(both, enumerate(progs)))
        for i, (p, (sb, pb)) in enumerate(zip(progs, builds)):
            row = {"name": p["name"], "kind": p["kind"], "seq_build": sb, "par_build": pb}
            if sb["ok"] and pb["ok"]:
                # No task site: the two binaries should be the same bytes, and
                # their ratio then measures the box's noise, not auto-par.
                row["identical_binary"] = (work / f"{i}.seq").read_bytes() == (work / f"{i}.par").read_bytes()
            if "kata" in p:
                row["kata"] = p["kata"]
                row["legacy_speedup"] = p["legacy_speedup"]
            if not (sb["ok"] and pb["ok"]):
                row.update({"verdict": "NOBUILD", "why": sb.get("why") or pb.get("why")})
            else:
                row.update(measure(p, work / f"{i}.seq", work / f"{i}.par", args))
            report["programs"].append(row)
            r = row.get("ratio")
            print(f"[{i + 1}/{len(progs)}] {p['name']}: "
                  f"{r if r is not None else row['verdict']}"
                  f"{'' if row.get('output_same', True) else ' OUTPUT DIFFERS'}", flush=True)
            stem.with_suffix(".json").write_text(json.dumps(report, indent=1) + "\n")
    finally:
        shutil.rmtree(work, ignore_errors=True)
    report["finished"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    stem.with_suffix(".json").write_text(json.dumps(report, indent=1) + "\n")
    stem.with_suffix(".md").write_text(markdown(report))
    print(stem.with_suffix(".md"))
    under = [r for r in report["programs"] if r.get("ratio") is not None and r["ratio"] < FLOOR]
    return 1 if under else 0


if __name__ == "__main__":
    sys.exit(main())
