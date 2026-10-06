#!/usr/bin/env python3
"""Run the program corpus (`corpus/`) on one backend.

    python3 scripts/corpus/run.py --backend legacy            # each entry on the legacy backend it was recorded on
    python3 scripts/corpus/run.py --backend legacy-build --filter drop
    python3 scripts/corpus/run.py --backend legacy-interp --filter fixtures/interpreter/

Backends: legacy (per entry, its `legacy_backend`), legacy-interp,
legacy-build (AOT at the default auto-par setting, which is off), and the
mir-interp / mir-llvm / mir-llvm-asan slots, which have no command until the
new pipeline lands.

--filter matches a tag exactly or a substring of the entry path; repeat it to
widen. Each entry is judged by its meta.toml `expect`:

    stdout        runs; stdout == expected.out and exit == `exit` (an entry not
                  yet classified has no expected.out and is held to legacy.out)
    panic:<N>     runs; exit == N and stdout == expected.out
    error:<code>  the compiler refuses it, naming <code> (`error` alone or
                  `error:codegen` = any refusal)
    skip          not run

Output: one `PASS|FAIL|SKIP <entry>` line per program (FAIL lines carry the
reason), then a summary by class and by tag, and the number of programs
actually executed. Exits 1 on any FAIL, and 2 when nothing ran — a filter that
matches nothing must not read as a pass.
"""

import argparse
import concurrent.futures as cf
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from corpus_lib import BACKENDS, ROOT, entries, read_toml, run_program  # noqa: E402


def judge(entry: Path, corpus: Path, args) -> dict:
    rel = str(entry.relative_to(corpus))
    meta = read_toml(entry / "meta.toml")
    expect = meta.get("expect", "stdout")
    backend = meta.get("legacy_backend", "legacy-build") if args.backend == "legacy" else args.backend
    base = {"name": rel, "class": meta.get("class", "unknown"), "tags": meta.get("tags", [])}
    if expect == "skip" or (meta.get("backends") and backend not in meta["backends"]):
        return {**base, "verdict": "SKIP", "why": expect if expect == "skip" else "backend excluded", "ran": False}
    if BACKENDS.get(backend) is None:
        return {**base, "verdict": "SKIP", "why": f"backend {backend} not available yet", "ran": False}
    env = {"KARAC_HASH_SEED": str(args.seed), **meta.get("env", {})}
    res = run_program(entry, backend, args.karac, args.timeout, env)
    why = ""
    if res["status"] == "timeout":
        why = f"timeout after {args.timeout:.0f}s"
    elif expect == "error" or expect.startswith("error:"):
        code = expect.split(":", 1)[1] if ":" in expect else ""
        if res["status"] != "refused":
            why = f"expected a refusal ({code}), program ran (exit {res['exit']})"
        elif code not in ("codegen", "") and code not in check_codes(entry, args) + [None]:
            why = f"refused, but without {code}"
    else:
        want_exit = int(expect.split(":", 1)[1]) if expect.startswith("panic:") else int(meta.get("exit", 0))
        exp_file = entry / "expected.out"
        if not exp_file.exists():
            exp_file = entry / "legacy.out"  # unclassified: legacy is the baseline
        exp = exp_file.read_bytes() if exp_file.exists() else None
        if res["status"] == "refused":
            why = f"refused (exit {res['exit']}): {first_line(res['stderr'])}"
        elif exp is None:
            why = "no expected.out"
        elif res["stdout"] != exp:
            why = f"stdout differs ({len(res['stdout'])} vs {len(exp)} bytes)"
        elif res["exit"] != want_exit:
            why = f"exit {res['exit']}, expected {want_exit}"
    return {**base, "verdict": "FAIL" if why else "PASS", "why": why, "ran": True}


def check_codes(entry: Path, args) -> list:
    import json
    r = subprocess.run([args.karac, "check", "--output=json", "source.kara"], cwd=entry,
                       capture_output=True, timeout=args.timeout, stdin=subprocess.DEVNULL)
    try:
        return [d.get("code") or d.get("phase") for d in json.loads(r.stdout).get("diagnostics", [])]
    except json.JSONDecodeError:
        return []


def first_line(b: bytes) -> str:
    for line in b.decode(errors="replace").splitlines():
        if line.strip():
            return line.strip()[:160]
    return ""


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--backend", required=True, choices=["legacy", *BACKENDS])
    ap.add_argument("--karac", default=str(ROOT / "target/debug/karac"))
    ap.add_argument("--corpus", default=str(ROOT / "corpus"))
    ap.add_argument("--filter", action="append", default=[], help="tag or path substring; repeatable")
    ap.add_argument("--exclude", action="append", default=[],
                    help="tag or path substring to leave out; `--exclude core` gives the M0 legacy check, "
                         "since the core pins hold v2 expectations legacy is meant to miss")
    ap.add_argument("--jobs", type=int, default=os.cpu_count() or 4)
    ap.add_argument("--timeout", type=float, default=60.0)
    ap.add_argument("--seed", type=int, default=1, help="KARAC_HASH_SEED for every run")
    ap.add_argument("--quiet", action="store_true", help="print only FAIL lines and the summary")
    args = ap.parse_args()

    corpus = Path(args.corpus)
    todo = []
    for e in entries(corpus):
        rel = str(e.relative_to(corpus))
        if args.filter or args.exclude:
            tags = set(read_toml(e / "meta.toml").get("tags", []))
            if args.filter and not any(f in tags or f in rel for f in args.filter):
                continue
            if any(f in tags or f in rel for f in args.exclude):
                continue
        todo.append(e)

    results = []
    with cf.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for r in pool.map(lambda e: judge(e, corpus, args), todo):
            results.append(r)
            if r["verdict"] == "FAIL" or not args.quiet:
                tail = f"  ({r['why']})" if r["why"] and r["verdict"] == "FAIL" else ""
                print(f"{r['verdict']} {r['name']}{tail}", flush=True)

    def tally(key_fn):
        t: dict[str, dict[str, int]] = {}
        for r in results:
            for k in key_fn(r):
                t.setdefault(k, {"PASS": 0, "FAIL": 0, "SKIP": 0})[r["verdict"]] += 1
        return t

    print()
    for title, t in (("class", tally(lambda r: [r["class"]])), ("tag", tally(lambda r: r["tags"] or ["(none)"]))):
        print(f"by {title}:")
        for k, v in sorted(t.items()):
            print(f"  {k:28} pass={v['PASS']} fail={v['FAIL']} skip={v['SKIP']}")
    ran = sum(r["ran"] for r in results)
    fails = sum(r["verdict"] == "FAIL" for r in results)
    passes = sum(r["verdict"] == "PASS" for r in results)
    print(f"corpus-run backend={args.backend} matched={len(results)} executed={ran} "
          f"pass={passes} fail={fails} skip={len(results) - passes - fails}")
    if ran == 0:
        print("corpus-run: nothing executed", file=sys.stderr)
        return 2
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
