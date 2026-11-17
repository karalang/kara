#!/usr/bin/env python3
"""Rewrite deprecated prefix collection literals (`Vec[a, b]`, `Array[..]`,
`VecDeque[..]`, ...; design.md § Collection literals, B-2026-11-16-255) with
the machine-applicable fix `karac check` attaches to each warning, and touch
nothing else.

`karac fix` applies EVERY machine-applicable fix in a file (`+=`, ranges,
literal suffixes, ...), which rewrites what a test fixture deliberately pins.
This applies only the W0245 prefix-literal edits, repeating while a pass finds
more (an inner literal gets its typed fix once the outer one is rewritten).
It prints each changed file with its count and, at the end, the sites left
with no fix (B-2026-11-17-112).

    python3 scripts/migrate-prefix-literals.py <dir-or-file>... [--karac PATH]
"""
import argparse
import json
import pathlib
import subprocess
from concurrent.futures import ThreadPoolExecutor


def prefix_warnings(karac, p):
    r = subprocess.run(
        [karac, "check", "--output=json", p.name],
        cwd=p.parent,
        capture_output=True,
        timeout=300,
    )
    try:
        d = json.loads(r.stdout.decode(errors="replace") or "{}")
    except ValueError:
        return []
    return [
        g
        for g in d.get("diagnostics", [])
        if g.get("code") == "W0245" and "prefix literal" in g.get("message", "")
    ]


def migrate(karac, p):
    n = 0
    for _ in range(4):
        fixes = [g["replacement"] for g in prefix_warnings(karac, p) if "replacement" in g]
        if not fixes:
            break
        b = p.read_bytes()
        last = None
        for f in sorted(fixes, key=lambda f: -f["offset"]):
            o, length = f["offset"], f["length"]
            if last is not None and o + length > last:
                continue  # overlaps one already applied: the next pass
            b = b[:o] + f["text"].encode() + b[o + length :]
            last = o
            n += 1
        p.write_bytes(b)
    return p, n


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("paths", nargs="+")
    ap.add_argument("--karac", default="karac")
    ap.add_argument("--jobs", type=int, default=8)
    a = ap.parse_args()
    if "/" in a.karac:
        a.karac = str(pathlib.Path(a.karac).resolve())
    files = []
    for path in map(pathlib.Path, a.paths):
        files += [path] if path.is_file() else sorted(path.rglob("*.kara"))
    # `source.orig.kara` is a corpus entry's source as first written, kept
    # verbatim beside the runnable one.
    files = [f.resolve() for f in files if not f.name.endswith(".orig.kara")]
    total = 0
    with ThreadPoolExecutor(a.jobs) as ex:
        for p, n in ex.map(lambda f: migrate(a.karac, f), files):
            if n:
                total += n
                print(f"{n:4} {p}")
    print(f"{total} prefix literal(s) rewritten")
    with ThreadPoolExecutor(a.jobs) as ex:
        for p, ws in zip(files, ex.map(lambda f: prefix_warnings(a.karac, f), files)):
            for g in ws:
                if "replacement" not in g:
                    print(f"no fix: {p}:{g.get('line')}")


if __name__ == "__main__":
    main()
