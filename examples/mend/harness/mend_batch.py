#!/usr/bin/env python3
"""
mend_batch.py — run the Mend loop over the whole example corpus and score it.

One command → every example's transcript collected into a single batch
directory + the aggregate Mend score (machine-fix rate, gap ledger). This
is the driver for the corpus-wide measurement: point it at a corpus of task
prompts, run them all, read the number.

    # dry-run every example that ships canned responses (deterministic, no API)
    python3 examples/mend/harness/mend_batch.py --dry-run

    # live mode — a real LLM per task; needs an authenticated `claude` CLI.
    # This is the honest *uncurated* measurement: fresh model instances that
    # never saw the compiler's diagnostics, writing Kāra from the task prompt
    # alone. The machine-fix rate is only meaningful over a corpus authored
    # blind to the failures — dry-run (canned) numbers are demonstrations, not
    # measurements.
    python3 examples/mend/harness/mend_batch.py

    # a specific subset
    python3 examples/mend/harness/mend_batch.py two_source_totals welcome_emails

Reuses `mend.run_loop` (the exact single-example loop) and `mend_score`
(the exact scorer) — no logic is duplicated, so batch numbers match what a
per-example run + `mend_score.py` would report.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import sys
from pathlib import Path

HARNESS_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(HARNESS_DIR))

from mend import run_loop  # noqa: E402  (path set above)
from mend_score import aggregate, render_report, score_run  # noqa: E402

EXAMPLES_ROOT = HARNESS_DIR.parent / "examples"
RUNS_ROOT = HARNESS_DIR.parent / "runs"


def discover(dry_run: bool) -> list[Path]:
    """Every example dir with a task.md (and, for dry-run, canned responses)."""
    out: list[Path] = []
    for d in sorted(EXAMPLES_ROOT.iterdir()):
        if not d.is_dir() or not (d / "task.md").exists():
            continue
        if dry_run and not (d / "canned_responses.json").exists():
            continue
        out.append(d)
    return out


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description="Run + score the whole Mend corpus.")
    p.add_argument("examples", nargs="*", help="Example names (default: all).")
    p.add_argument(
        "--dry-run",
        action="store_true",
        help="Canned responses, no API. Cannot run live-only tasks "
        "(those without canned_responses.json).",
    )
    p.add_argument("--max-iterations", type=int, default=5)
    args = p.parse_args(argv)

    if args.examples:
        selected = [EXAMPLES_ROOT / n for n in args.examples]
        missing = [d.name for d in selected if not (d / "task.md").exists()]
        if missing:
            print(f"[mend-batch] no task.md for: {missing}", file=sys.stderr)
            return 2
        if args.dry_run:
            skipped = [d.name for d in selected if not (d / "canned_responses.json").exists()]
            if skipped:
                print(
                    f"[mend-batch] --dry-run skips live-only example(s): {skipped}",
                    file=sys.stderr,
                )
            selected = [d for d in selected if (d / "canned_responses.json").exists()]
    else:
        selected = discover(args.dry_run)

    if not selected:
        why = (
            "dry-run needs canned_responses.json"
            if args.dry_run
            else "no example has a task.md"
        )
        print(f"[mend-batch] nothing runnable ({why})", file=sys.stderr)
        return 2

    stamp = _dt.datetime.now().strftime("%Y%m%dT%H%M%S")
    batch_dir = RUNS_ROOT / f"batch_{stamp}"
    print(f"[mend-batch] {len(selected)} example(s) → {batch_dir}")
    print(f"[mend-batch] mode: {'dry-run (canned)' if args.dry_run else 'live'}\n")

    for ex in selected:
        print(f"── {ex.name} ──")
        try:
            run_loop(
                ex,
                batch_dir / ex.name,
                dry_run=args.dry_run,
                max_iterations=args.max_iterations,
            )
        except Exception as e:  # one bad example must not sink the batch
            print(f"[mend-batch] {ex.name} errored: {e}", file=sys.stderr)
        print()

    records = [
        r
        for d in sorted(batch_dir.iterdir())
        if d.is_dir() and (r := score_run(d)) is not None
    ]
    summary = aggregate(records)
    print(render_report(summary))
    return 0


if __name__ == "__main__":
    sys.exit(main())
