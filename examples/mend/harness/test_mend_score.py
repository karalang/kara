#!/usr/bin/env python3
"""
test_mend_score.py — unit tests for the Mend scorer's fix-mechanics predicates.

Run: python3 examples/mend/harness/test_mend_score.py

These cover B-2026-08-02-1: the scorer charged a CORRECT `karac fix` with
"breaking the build" whenever it unmasked errors an earlier-phase failure had
been hiding. `karac` stops at the first failing phase, so repairing a parse
error is precisely what lets typecheck and ownership run and report what they
were never reached to find — progress, scored as regression.

The point of the fix is NOT to make the metric quieter. A real regression must
still be caught, so the negative cases below (a parse fix yielding a DIFFERENT
parse error, a fix that introduces an earlier-phase error) matter at least as
much as the positive ones.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from mend_score import (  # noqa: E402
    _blocking_rank,
    _fix_regressed,
    _fixes_resolved,
    _phase_rank,
)


def d(code: str, phase: str) -> dict:
    return {"code": code, "phase": phase, "message": f"{code} sample"}


FAILURES: list[str] = []


def check(name: str, got, want) -> None:
    if got != want:
        FAILURES.append(f"{name}: got {got!r}, want {want!r}")


# ── phase ordering ────────────────────────────────────────────────────────────

check("parse before typecheck", _phase_rank("parse") < _phase_rank("typecheck"), True)
check(
    "typecheck before ownership",
    _phase_rank("typecheck") < _phase_rank("ownership"),
    True,
)
# An unknown phase sorts LAST so a phase name the harness has not learned yet is
# treated as newly-unmasked rather than as a regression — the conservative
# direction for a metric whose failure mode is over-reporting.
check(
    "unknown phase sorts last",
    _phase_rank("some_future_phase") >= _phase_rank("codegen"),
    True,
)
check("missing phase sorts last", _phase_rank(None) >= _phase_rank("codegen"), True)
check("blocking rank is the earliest", _blocking_rank([d("E1", "ownership"), d("E2", "parse")]), _phase_rank("parse"))


# ── the reported case: progress must NOT be charged as a regression ───────────
# batch_20260801T235833, canonical_request/iter_000 — verbatim.

before = [d("E0001", "parse")]
after = [d("E0210", "typecheck"), d("E0500", "ownership")]
check("unmasked later-phase errors are not a regression", _fix_regressed(before, after), False)
# And the fix must be CREDITED: the parse error really did go away. The old
# `max(0, len(before) - len(after))` scored this 0 (1 before, 2 after), counting
# a fix that did its job against fix_precision_pct.
check("unmasking still credits the resolved diagnostic", _fixes_resolved(before, after), 1)


# ── real regressions must STILL be caught ─────────────────────────────────────

# A parse fix that yields a DIFFERENT parse error is suspect: that phase ran
# both times, so the new code is genuinely new.
check(
    "different error at the same phase is a regression",
    _fix_regressed([d("E0001", "parse")], [d("E0002", "parse")]),
    True,
)
# A fix that introduces an EARLIER-phase error is unambiguously a regression.
check(
    "new earlier-phase error is a regression",
    _fix_regressed([d("E0210", "typecheck")], [d("E0001", "parse")]),
    True,
)
# Mixed: one unmasked later-phase error (fine) plus a new same-phase one (not).
check(
    "a same-phase newcomer is caught even alongside unmasked ones",
    _fix_regressed(
        [d("E0001", "parse")],
        [d("E0002", "parse"), d("E0210", "typecheck")],
    ),
    True,
)


# ── neutral cases ─────────────────────────────────────────────────────────────

check("no change is not a regression", _fix_regressed([d("E1", "parse")], [d("E1", "parse")]), False)
check("clean after is not a regression", _fix_regressed([d("E1", "parse")], []), False)
check("clean after resolves everything", _fixes_resolved([d("E1", "parse"), d("E2", "parse")], []), 2)
check("nothing resolved when unchanged", _fixes_resolved([d("E1", "parse")], [d("E1", "parse")]), 0)
# Duplicates are compared per code, so two of a code going to one counts as one
# resolved rather than being lost in a total-length comparison.
check(
    "per-code multiset difference",
    _fixes_resolved([d("E1", "parse"), d("E1", "parse")], [d("E1", "parse")]),
    1,
)
# An empty before-set cannot resolve anything and cannot regress (no phase ran).
check("empty before resolves nothing", _fixes_resolved([], [d("E1", "parse")]), 0)


if FAILURES:
    print(f"FAILED ({len(FAILURES)}):")
    for f in FAILURES:
        print("  -", f)
    sys.exit(1)
print("all mend_score predicate tests passed")
