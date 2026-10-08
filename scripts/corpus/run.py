#!/usr/bin/env python3
"""Run the program corpus (`corpus/`) on one backend.

    python3 scripts/corpus/run.py --backend legacy            # each entry on the legacy backend it was recorded on
    python3 scripts/corpus/run.py --backend legacy-build --filter drop
    python3 scripts/corpus/run.py --backend legacy-interp --filter fixtures/interpreter/

Backends: legacy (per entry, its `legacy_backend`), legacy-interp,
legacy-build (AOT at the default auto-par setting, which is off), mir-interp
(the MIR builder and interpreter, `karac __mir-run`), and the mir-llvm /
mir-llvm-asan slots, which have no command until MIR->LLVM lands.

--filter matches a tag exactly or a substring of the entry path; repeat it to
widen. Each entry is judged by its meta.toml `expect`:

    stdout        runs; stdout == expected.out and exit == `exit` (an entry not
                  yet classified has no expected.out and is held to legacy.out)
    panic:<N>     runs; exit == N and stdout == expected.out
    error:<code>  the compiler refuses it, naming <code> (`error` alone or
                  `error:codegen` = any refusal)
    skip          not run

On the mir-* backends the expectation is expected.out, then model.out (the
drop model's output, held to `model_exit`; an ORDER-UNSPEC entry compares its
lines as a set), then legacy.out. An entry the model rejects (model_verdict
V2-REJECT) is not run: `karac check` must refuse it. When check accepts it the
verdict is MREJ ("model rejects, check accepts"), neither a pass nor a fail:
each one is a checker gap or a model bug. MREJ is judged even before a mir
backend has a command, since it needs only `karac check`. `error:codegen` says
only that legacy-build's codegen declined the program: on a mir backend it
passes when `karac check` refuses, and is otherwise judged on its output. The
interpreter backends skip kata bench variants (`bench`), which measure the
compiled backend.

Output: one `PASS|FAIL|SKIP|MREJ <entry>` line per program (FAIL lines carry the
reason), then a summary by class and by tag, and the number of programs
actually executed. corpus/apps gets its own last line (`corpus-run apps ...`):
the share of apps programs without a `deferred:` tag that pass, with the M1 bar. Exits 1 on any FAIL, and 2 when nothing ran — a filter that
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
    base = {"name": rel, "class": meta.get("class", "unknown"), "tags": meta.get("tags", []),
            "model": meta.get("model_verdict", "(none)")}
    if expect == "skip" or (meta.get("backends") and backend not in meta["backends"]):
        return {**base, "verdict": "SKIP", "why": expect if expect == "skip" else "backend excluded", "ran": False}
    mir = backend.startswith("mir-")
    if not mir and meta.get("legacy_expect"):
        expect = meta["legacy_expect"]  # legacy refuses what v2 accepts
    if mir and meta.get("v2_reject"):
        expect = "error"  # legacy ran it; v2 refuses it
    if mir and expect == "error:codegen":
        # Only legacy-build's codegen declined it: no language rule. A check
        # refusal still passes; otherwise it is judged on its output.
        if check_refuses(entry, args):
            return {**base, "verdict": "PASS", "why": "", "ran": True}
        expect = "stdout"
    if backend in ("mir-interp", "legacy-interp") and "bench" in meta.get("tags", []):
        # A kata's bench variant measures the compiled backend; no interpreter finishes it.
        return {**base, "verdict": "SKIP", "why": "AOT throughput workload", "ran": False}
    if mir and meta.get("model_verdict") == "V2-REJECT" and not expect.startswith("error"):
        if check_refuses(entry, args):
            return {**base, "verdict": "PASS", "why": "", "ran": True}
        return {**base, "verdict": "MREJ", "why": f"model: {meta.get('model_note', '')}", "ran": True}
    if BACKENDS.get(backend) is None:
        return {**base, "verdict": "SKIP", "why": f"backend {backend} not available yet", "ran": False}
    env = {"KARAC_HASH_SEED": str(args.seed), **meta.get("env", {})}
    orig = not mir and bool(meta.get("fixed_by_classify"))
    res = run_program(entry, backend, args.karac, args.timeout, env,
                      source="source.orig.kara" if orig else "source.kara")
    why = ""
    if res["status"] == "timeout":
        why = f"timeout after {args.timeout:.0f}s"
    elif expect == "error" or expect.startswith("error:"):
        code = expect.split(":", 1)[1] if ":" in expect else ""
        if res["status"] != "refused":
            why = f"expected a refusal ({code}), program ran (exit {res['exit']})"
        elif mir and b"error[mir]: build" in res["stderr"]:
            why = f"expected a refusal ({code}); check accepts and MIR cannot build it yet"
        elif code not in ("codegen", "") and code not in check_codes(entry, args) + [None]:
            why = f"refused, but without {code}"
    else:
        want_exit = int(expect.split(":", 1)[1]) if expect.startswith("panic:") else int(meta.get("exit", 0))
        # The legacy backends are held to their own record: a classified
        # entry's expected.out states v2 behaviour, and a class-b entry's
        # source.kara is the fixed text legacy never ran.
        exp_file = entry / "expected.out" if mir or not (entry / "legacy.out").exists() else entry / "legacy.out"
        as_set = False
        if mir and exp_file.exists() and meta.get("expected_from") == "spec" and "model_exit" in meta:
            # expected.out copied from the model: held to the model's exit.
            want_exit = int(meta["model_exit"])
            as_set = meta.get("model_verdict") == "ORDER-UNSPEC"
        if not exp_file.exists() and mir and (entry / "model.out").exists():
            exp_file = entry / "model.out"
            want_exit = int(meta.get("model_exit", want_exit))
            as_set = meta.get("model_verdict") == "ORDER-UNSPEC"
        if not exp_file.exists():
            exp_file = entry / "legacy.out"  # unclassified: legacy is the baseline
        exp = exp_file.read_bytes() if exp_file.exists() else None
        if res["status"] == "refused":
            why = f"refused (exit {res['exit']}): {first_line(res['stderr'])}"
        elif exp is None:
            why = "no expected.out"
        elif as_set and sorted(res["stdout"].splitlines()) == sorted(exp.splitlines()):
            if res["exit"] != want_exit:
                why = f"exit {res['exit']}, expected {want_exit}"
        elif res["stdout"] != exp:
            why = f"stdout differs ({len(res['stdout'])} vs {len(exp)} bytes)"
        elif res["exit"] != want_exit:
            why = f"exit {res['exit']}, expected {want_exit}"
    return {**base, "verdict": "FAIL" if why else "PASS", "why": why, "ran": True}


def check_refuses(entry: Path, args) -> bool:
    r = subprocess.run([args.karac, "check", "source.kara"], cwd=entry, capture_output=True,
                       timeout=args.timeout, stdin=subprocess.DEVNULL)
    return r.returncode != 0


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
            if r["verdict"] in ("FAIL", "MREJ") or not args.quiet:
                tail = f"  ({r['why']})" if r["why"] and r["verdict"] in ("FAIL", "MREJ") else ""
                print(f"{r['verdict']} {r['name']}{tail}", flush=True)

    def tally(key_fn):
        t: dict[str, dict[str, int]] = {}
        for r in results:
            for k in key_fn(r):
                t.setdefault(k, {"PASS": 0, "FAIL": 0, "SKIP": 0, "MREJ": 0})[r["verdict"]] += 1
        return t

    print()
    for title, t in (("class", tally(lambda r: [r["class"]])), ("tag", tally(lambda r: r["tags"] or ["(none)"])),
                     ("model_verdict", tally(lambda r: [r["model"]]))):
        print(f"by {title}:")
        for k, v in sorted(t.items()):
            mrej = f" model-reject-check-accepts={v['MREJ']}" if v["MREJ"] else ""
            print(f"  {k:28} pass={v['PASS']} fail={v['FAIL']} skip={v['SKIP']}{mrej}")
    ran = sum(r["ran"] for r in results)
    fails = sum(r["verdict"] == "FAIL" for r in results)
    passes = sum(r["verdict"] == "PASS" for r in results)
    mrej = sum(r["verdict"] == "MREJ" for r in results)
    print(f"corpus-run backend={args.backend} matched={len(results)} executed={ran} "
          f"pass={passes} fail={fails} skip={len(results) - passes - fails - mrej}"
          + (f" model-reject-check-accepts={mrej}" if mrej else ""))
    # corpus/apps is reported on its own line with its own bar, never pooled with the katas
    # (PLAN_RECHECK_2026-10-07 §2: at M1, 95% of the apps programs that use no deferred
    # feature must pass on the MIR interpreter).
    apps = [r for r in results if r["name"].startswith("apps/")]
    if apps:
        eligible = [r for r in apps if not any(t.startswith("deferred:") for t in r["tags"])]
        ok = sum(r["verdict"] == "PASS" for r in eligible)
        pct = f"{100 * ok / len(eligible):.0f}%" if eligible else "-"
        print(f"corpus-run apps backend={args.backend} programs={len(apps)} without-deferred={len(eligible)} "
              f"pass={ok} ({pct}; M1 bar on mir-interp: 95%)")
    if ran == 0:
        print("corpus-run: nothing executed", file=sys.stderr)
        return 2
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
