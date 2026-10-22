#!/usr/bin/env python3
"""Hold extracted test programs to their test's own literal, not to legacy.

M3 item 9 (review/m3-plan): programs extracted from the test targets that go
with legacy need an expected output that survives legacy's deletion. For each
entry named by --filter, this runs `karac __mir-run` and compares its stdout
with the holding test's string literals (the manifest's `candidates`, compared
as record.py compares them). When a literal is the v2 stdout, or else the
recorded legacy stdout (legacy.out), with exit 0, that output is written as
expected.out and the entry gets expected_from = "oracle", expect = "stdout",
exit = 0, so the classifier reads it as the spec.

Entries expecting a refusal, entries with no matching literal, and entries
already held to a spec source (pin, spec, mirror, decision, oracle) are left
as they are. One line per entry, then a count per outcome:

  oracle-v2       a literal is the v2 stdout
  oracle-legacy   a literal is legacy's stdout and v2 prints something else
  no-literal      no literal is either stdout (an IR-shape or tolerant test)
  refusal         the entry expects a refusal
  kept            already held to a spec source
"""

import argparse
import collections
import concurrent.futures as cf
import json
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from corpus_lib import ROOT, read_toml, write_toml  # noqa: E402
from record import matches  # noqa: E402

SPEC_SOURCES = ("pin", "spec", "mirror", "decision", "oracle")


def one(entry: Path, rel: str, cands: list, args) -> tuple[str, str]:
    mp = entry / "meta.toml"
    meta = read_toml(mp)
    if meta.get("expected_from") in SPEC_SOURCES:
        return rel, "kept"
    expect = str(meta.get("expect", ""))
    if expect.startswith("error") and expect != "error:codegen":
        # (`error:codegen` is only legacy-build's codegen declining: no rule.)
        return rel, "refusal"
    try:
        r = subprocess.run([args.karac, "__mir-run", "source.kara"], cwd=entry, capture_output=True,
                           timeout=args.timeout, stdin=subprocess.DEVNULL,
                           env={**os.environ, "KARAC_HASH_SEED": "1", **meta.get("env", {})})
        v2 = r.stdout.decode(errors="replace") if r.returncode == 0 else None
    except subprocess.TimeoutExpired:
        v2 = None
    lo = entry / "legacy.out"
    legacy = lo.read_text(errors="replace") if lo.exists() and meta.get("legacy_exit") == 0 else None
    out, how = None, "no-literal"
    if v2 is not None and any(matches(c, v2) for c in cands):
        out, how = v2, "oracle-v2"
    elif legacy is not None and any(matches(c, legacy) for c in cands):
        out, how = legacy, "oracle-legacy"
    if out is not None and not args.dry_run:
        (entry / "expected.out").write_text(out)
        meta.update(expected_from="oracle", expect="stdout", exit=0)
        write_toml(mp, meta)
    return rel, how


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--karac", default=str(ROOT / "target/release/karac"))
    ap.add_argument("--corpus", default=str(ROOT / "corpus"))
    ap.add_argument("--work", default=str(ROOT / "target/corpus-work"))
    ap.add_argument("--filter", action="append", default=[], help="entry path prefix (repeatable)")
    ap.add_argument("--list", help="file of entry paths, one per line")
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--timeout", type=float, default=60.0)
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()
    corpus = Path(args.corpus)
    manifest = json.loads((Path(args.work) / "manifest.json").read_text())
    cands = {m["dir"]: m["candidates"] for m in manifest}
    want = list(args.filter)
    if args.list:
        want += [l.strip() for l in Path(args.list).read_text().splitlines() if l.strip()]
    if not want:
        ap.error("name entries with --filter or --list")
    todo = sorted(rel for rel in cands if any(rel == w or rel.startswith(w.rstrip("/") + "/") for w in want)
                  and (corpus / rel / "meta.toml").exists())
    counts = collections.Counter()
    with cf.ThreadPoolExecutor(args.jobs) as ex:
        for rel, how in ex.map(lambda rel: one(corpus / rel, rel, cands[rel], args), todo):
            counts[how] += 1
            print(f"{how}\t{rel}", flush=True)
    print("test-oracle " + " ".join(f"{k}={v}" for k, v in sorted(counts.items())) + f" total={len(todo)}")
    return 0 if todo else 2


if __name__ == "__main__":
    sys.exit(main())
