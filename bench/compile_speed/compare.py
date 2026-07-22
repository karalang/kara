#!/usr/bin/env python3
"""Gate + report logic for the compile-speed bench.

Reads hyperfine-schema JSON (latest.json from bench.sh; baseline.json committed
to the repo) where each result is named `<compiler>:<workload>` with compilers
`karac` / `rustc`.

The gated quantity is the karac/rustc RATIO per workload, not the absolute
karac time: both compilers shift together across runner generations, so the
ratio is far more stable than either absolute (see README.md § Baseline).
Absolute deltas are reported as context.

Modes:
  --ratios-only                 print per-workload ratios (bench.sh footer)
  --baseline B [--markdown F]   gate: fail (exit 1) if any workload's ratio
                                exceeds baseline ratio by more than
                                --threshold (default 30%); write a markdown
                                report for the PR comment / step summary
  --write-baseline B            rewrite B from latest when any ratio moved
                                more than --drift (default 5%) from it, or
                                when B is empty; exit 0 either way (the CI
                                step commits only if the file changed)
"""

import argparse
import json
import sys
from pathlib import Path


def load(path):
    data = json.loads(Path(path).read_text())
    out = {}
    for r in data.get("results", []):
        name = r.get("command", "")
        if ":" not in name:
            continue
        compiler, workload = name.split(":", 1)
        out.setdefault(workload, {})[compiler] = r
    return out


def ratios(table):
    out = {}
    for workload, by_compiler in sorted(table.items()):
        if "karac" in by_compiler and "rustc" in by_compiler:
            k, r = by_compiler["karac"]["mean"], by_compiler["rustc"]["mean"]
            out[workload] = (k, r, k / r)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--latest", required=True)
    ap.add_argument("--baseline")
    ap.add_argument("--ratios-only", action="store_true")
    ap.add_argument("--write-baseline")
    ap.add_argument("--markdown")
    ap.add_argument("--threshold", type=float, default=0.30,
                    help="gate: max allowed ratio regression vs baseline")
    ap.add_argument("--drift", type=float, default=0.05,
                    help="write-baseline: min ratio movement that rewrites")
    args = ap.parse_args()

    latest = ratios(load(args.latest))

    if args.ratios_only:
        for w, (k, r, ratio) in latest.items():
            print(f"  {w}: karac {k:.3f}s / rustc {r:.3f}s = {ratio:.2f}x")
        return 0

    if args.write_baseline:
        path = Path(args.write_baseline)
        base = ratios(load(path)) if path.exists() else {}
        stale = [w for w in latest
                 if w not in base
                 or abs(latest[w][2] - base[w][2]) / base[w][2] > args.drift]
        if stale:
            path.write_text(Path(args.latest).read_text())
            print(f"baseline rewritten (moved: {', '.join(stale)})")
        else:
            print(f"baseline unchanged (all ratios within {args.drift:.0%})")
        return 0

    # Gate mode.
    base = ratios(load(args.baseline)) if args.baseline else {}
    rows, failures = [], []
    for w, (k, r, ratio) in latest.items():
        if w in base:
            b = base[w][2]
            delta = (ratio - b) / b
            verdict = "PASS" if delta <= args.threshold else "FAIL"
            if verdict == "FAIL":
                failures.append(w)
            rows.append((w, k, r, ratio, f"{b:.2f}x", f"{delta:+.1%}", verdict))
        else:
            rows.append((w, k, r, ratio, "—", "—", "no baseline"))

    header = f"| workload | karac | rustc -O | ratio | baseline | Δ ratio | verdict |"
    sep = "|---|---|---|---|---|---|---|"
    lines = [
        f"### Compile-speed gate ({args.threshold:.0%} threshold on karac/rustc ratio)",
        "", header, sep,
    ]
    for w, k, r, ratio, b, d, v in rows:
        lines.append(f"| {w} | {k:.3f}s | {r:.3f}s | {ratio:.2f}x | {b} | {d} | {v} |")
    if not base:
        lines.append("")
        lines.append("_No baseline yet — gate passes vacuously; the main-merge "
                     "workflow writes the first baseline._")
    md = "\n".join(lines) + "\n"

    if args.markdown:
        Path(args.markdown).write_text(md)
    print(md)

    if failures:
        print(f"GATE FAIL: {', '.join(failures)} regressed more than "
              f"{args.threshold:.0%} over baseline", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
