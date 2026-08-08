#!/usr/bin/env python3
"""Find `#[cfg(feature = "llvm")]`-gated tests that no CI job runs.

B-2026-08-08-27. The two dark-target gaps found so far (B-2026-07-31-44, 554
tests across 19 targets; B-2026-08-08-26, 35 more in `tests/cli.rs`) were both
found the same way: somebody ran the full suite locally, noticed a failure CI
did not have, and worked backwards. That is luck, twice — and it only works
when the dark tests are RED. A dark target that is green hides until the day it
is not, and then it hides the regression too.

The gap is mechanically closable, which is what this script does:

  * `cargo test --all` (the `test` job) builds WITHOUT `--features llvm`, so
    every llvm-gated test is compiled out of it. Only invocations that pass
    `--features llvm` can cover one.
  * so: parse the `--test <name>` lists out of `.github/workflows/ci.yml`,
    intersect with the targets that actually contain llvm-gated code, and
    report the difference.

POSITIONAL FILTERS ARE THE SUBTLE PART, and the `cli` case is why. A target can
be named in the workflow and still be almost entirely dark, because
`cargo test --features llvm --test cli -- name1 name2` runs only the tests whose
names substring-match a filter. A target covered ONLY by filtered invocations is
reported as PARTIAL rather than covered.

Two modes:

  (default)  Target-level, pure-python, no build. Answers "is there a target
             with llvm-gated code that no CI job runs?" in milliseconds. This is
             the CI gate — it would have caught all three pockets.
  --count    Adds exact per-target dark test COUNTS by diffing
             `cargo test --features llvm --test T -- --list` against the same
             without the feature. Needs a toolchain with LLVM and takes minutes;
             this is the measurement mode, not the gate.

Exit status is 1 when an un-allowlisted dark target exists, 0 otherwise.
"""

from __future__ import annotations

import argparse
import glob
import os
import re
import subprocess
import sys

CI_YML = ".github/workflows/ci.yml"
LLVM_CFG = 'cfg(feature = "llvm")'

# Targets deliberately left out of CI, each with the reason. A checker without
# an explicit allowlist just reports these forever and gets ignored, which is
# the failure mode this script exists to avoid. Adding an entry here is a
# decision that should be visible in review — that is the point.
ALLOWLIST = {
    "selfhost_codegen": "1 test, ~181 s; wants its own runtime-budget decision",
    "parallax_bench": "bench reproduction is opt-in by design",
    "relay_bench": "bench reproduction is opt-in by design",
    "wasm_codegen": "documented Tier 3; covered by the separate `wasm` job",
}


def strip_comments(text: str) -> str:
    """Drop whole-line comments.

    A `#` at the start of a line is a comment in both YAML and in the shell of
    a `run: |` block, and ci.yml's prose discusses `cargo test` invocations at
    length — without this, the rationale comments parse as coverage.
    """
    return "\n".join(l for l in text.splitlines() if not l.lstrip().startswith("#"))


def ci_coverage(ci_path: str) -> dict[str, set[str] | None]:
    """target -> None if run unfiltered, else the union of positional filters."""
    src = strip_comments(open(ci_path).read())
    covered: dict[str, set[str] | None] = {}
    # A command continues onto lines indented past the `run:` key that do not
    # start a new YAML list item.
    for m in re.finditer(r"cargo test((?:[^\n]|\n\s{10,}(?!-))*)", src):
        cmd = " ".join(x.strip() for x in m.group(1).splitlines())
        if "--features llvm" not in cmd:
            continue  # llvm-gated tests are compiled out of this invocation
        toks = cmd.split()
        targets = [toks[i + 1] for i, t in enumerate(toks) if t == "--test" and i + 1 < len(toks)]
        filters = set(toks[toks.index("--") + 1:]) if "--" in toks else set()
        for t in targets:
            if covered.get(t, "missing") is None:
                continue  # already fully covered; a filtered run cannot narrow that
            covered[t] = None if not filters else (covered.get(t) or set()) | filters
    return covered


def targets_with_llvm_code() -> list[str]:
    return sorted(
        os.path.basename(p)[:-3]
        for p in glob.glob("tests/*.rs")
        if LLVM_CFG in open(p, encoding="utf-8").read()
    )


def list_tests(target: str, llvm: bool) -> set[str]:
    cmd = ["cargo", "test"]
    if llvm:
        cmd += ["--features", "llvm"]
    cmd += ["--test", target, "--", "--list"]
    out = subprocess.run(cmd, capture_output=True, text=True)
    if out.returncode != 0:
        raise SystemExit(f"`{' '.join(cmd)}` failed:\n{out.stderr[-2000:]}")
    return {l.rsplit(":", 1)[0] for l in out.stdout.splitlines() if l.endswith(": test")}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--count",
        action="store_true",
        help="also report exact dark test counts (builds each target twice; slow)",
    )
    ap.add_argument("--ci", default=CI_YML)
    args = ap.parse_args()

    if not os.path.exists(args.ci):
        raise SystemExit(f"{args.ci} not found — run from the repo root")

    covered = ci_coverage(args.ci)
    candidates = targets_with_llvm_code()

    dark = [t for t in candidates if t not in covered]
    partial = sorted(t for t in candidates if covered.get(t, "missing") not in (None, "missing"))
    flagged = [t for t in dark if t not in ALLOWLIST]
    excused = [t for t in dark if t in ALLOWLIST]

    print(f"{len(covered)} llvm target(s) named in {args.ci}")
    print(f"{len(candidates)} test target(s) contain {LLVM_CFG}")
    print()

    if args.count:
        for t in flagged + partial:
            n = len(list_tests(t, llvm=True) - list_tests(t, llvm=False))
            print(f"  {t}: {n} llvm-gated test(s)")
        print()

    if partial:
        print("PARTIAL — named in CI but only with positional filters, so most of")
        print("the target may still be dark (this is how tests/cli.rs hid 35 tests):")
        for t in partial:
            print(f"  {t}  filters: {sorted(covered[t])}")
        print()

    if excused:
        print("Dark but allowlisted:")
        for t in excused:
            print(f"  {t} — {ALLOWLIST[t]}")
        print()

    if flagged:
        print("::error::Dark llvm test target(s) — llvm-gated tests that NO CI job runs:")
        for t in flagged:
            print(f"  {t}")
        print()
        print("Fix by adding `--test <target>` to the codegen-e2e job in")
        print(f"{args.ci}, or by adding an entry with a reason to ALLOWLIST in")
        print(f"{__file__}.")
        return 1

    stale = sorted(set(ALLOWLIST) - set(dark))
    if stale:
        print("NOTE: allowlist entries that are no longer dark (now run by CI);")
        print("remove them so the list keeps meaning something:")
        for t in stale:
            print(f"  {t}")

    print("OK — every target with llvm-gated code is run by CI or explicitly allowlisted.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
