#!/usr/bin/env python3
"""Gate 3 (launch readiness) — the rolling first-run-breaking rate.

Gate 3 asks "is the bug curve flat enough to launch?". Its original criterion
was *"N consecutive katas with no new compiler bug"*, and N was never picked —
so for two months the gate could neither block nor clear. Two things were wrong
with it beyond the missing number:

  1. **It was indexed on katas.** Discovery long ago stopped being kata-driven:
     probes, overnight dogfooding, self-host ports, the ASAN sweep and CI now
     surface most rows. A kata-indexed counter gets reset by finds it is not
     even measuring, so it can never reach N.
  2. **It counted every class equally.** An ergonomics gap, a perf row and a
     silent miscompile are not the same evidence about launch readiness. Most
     of the volume is the long tail; the gate drowned in it.

This replaces both: a rolling count of `high`-severity rows in the classes that
BREAK A FIRST RUN, from ANY source.

    FIRST-RUN-BREAKING = miscompile · double-free · use-after-free
                       · crash · soundness · run-vs-build

`leak` is deliberately NOT in that set — a leak does not stop a first program
from printing the right answer — nor are perf/diagnostics/missing-feature/
codegen-gap/false-positive. Those still get filed and still get fixed; they
just do not gate a launch.

THE ANTI-GAMING CLAUSE. Discovery rate is proportional to how hard you look, so
a criterion on found-bugs alone clears the moment you stop looking. This
requires the search to have CONTINUED at comparable intensity: the window must
also contain at least `--min-total` new rows of any class. A quiet fortnight
because nobody ran a probe is not a flattened curve, and this refuses to score
it as one.

THE ONE JUDGMENT CALL, stated rather than hidden: the class field cannot tell
you whether a bug reproduces on a DEFAULT `-O2 karac build` or only under
`-O0`/ASAN/the JIT. Only the former truly breaks a user's first run. The script
therefore prints every row it counted, so that call is auditable instead of
buried in an aggregate.

Usage:
    scripts/gate3-rate.py                 # current reading
    scripts/gate3-rate.py --window 28
    scripts/gate3-rate.py --history       # week-by-week, to see the trend
"""

import argparse
import datetime
import json
import pathlib
import sys
from collections import Counter

FIRST_RUN_BREAKING = {
    "miscompile",
    "double-free",
    "use-after-free",
    "crash",
    "soundness",
    "run-vs-build",
}

LEDGER = pathlib.Path(__file__).resolve().parent.parent / "docs" / "bug-ledger.jsonl"


def load(path):
    rows = []
    for line in path.read_text().splitlines():
        if line.strip():
            rows.append(json.loads(line))
    return rows


def counts(rows):
    return [r for r in rows if r["severity"] == "high" and r["class"] in FIRST_RUN_BREAKING]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--window", type=int, default=14, help="rolling window in days (default 14)")
    ap.add_argument("--max-breaking", type=int, default=5, help="pass threshold (default 5)")
    ap.add_argument(
        "--min-total",
        type=int,
        default=40,
        help="minimum new rows of ANY class in the window — proof the search continued (default 40)",
    )
    ap.add_argument("--as-of", help="ISO date; defaults to the newest row in the ledger")
    ap.add_argument("--history", action="store_true", help="print the week-by-week trend and exit")
    ap.add_argument("--ledger", default=str(LEDGER))
    args = ap.parse_args()

    rows = load(pathlib.Path(args.ledger))
    date = {r["id"]: datetime.date.fromisoformat(r["date"]) for r in rows}
    breaking = counts(rows)

    if args.history:
        allw = Counter(date[r["id"]].isocalendar()[:2] for r in rows)
        brkw = Counter(date[r["id"]].isocalendar()[:2] for r in breaking)
        print("week        all   breaking   share")
        for k in sorted(allw):
            b = brkw.get(k, 0)
            print(f"{k[0]}-W{k[1]:<3}  {allw[k]:>5}   {b:>8}   {b / allw[k]:>5.0%}")
        print(
            "\nThe SHARE column is the trend that matters. Total discovery tracks how"
            "\nhard anyone looked that week; the share is the part that is about the"
            "\ncompiler rather than about the effort."
        )
        return 0

    as_of = datetime.date.fromisoformat(args.as_of) if args.as_of else max(date.values())
    window = [r for r in rows if 0 <= (as_of - date[r["id"]]).days < args.window]
    hit = counts(window)

    print(f"Gate 3 — rolling {args.window}-day first-run-breaking rate, as of {as_of}")
    print(f"  high-severity first-run-breaking : {len(hit):>4}   (pass: <= {args.max_breaking})")
    print(f"  new rows of any class            : {len(window):>4}   (pass: >= {args.min_total})")
    if window:
        print(f"  share                            : {len(hit) / len(window):>4.0%}")

    searched = len(window) >= args.min_total
    quiet = len(hit) <= args.max_breaking
    if quiet and searched:
        verdict = "MET"
    elif quiet and not searched:
        verdict = "NOT EVALUABLE — too few new rows; the search stopped, the curve did not flatten"
    else:
        verdict = f"NOT MET — {len(hit)} first-run-breaking rows, {len(hit) - args.max_breaking} over"
    print(f"\n  verdict: {verdict}")

    if hit:
        print(f"\nRows counted ({len(hit)}) — check each against 'does this break a DEFAULT -O2 build?':")
        for r in sorted(hit, key=lambda r: date[r["id"]]):
            print(f"  {r['date']}  {r['id']:<17} {r['class']:<15} {r.get('title', '')[:72]}")

    return 0 if verdict == "MET" else 1


if __name__ == "__main__":
    sys.exit(main())
