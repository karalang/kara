#!/usr/bin/env python3
"""
mend_score.py — aggregate Mend run transcripts into a measured score.

Reads the per-run transcripts that `mend.py` writes under
`examples/mend/runs/<timestamp>/` and turns them into the numbers the
AI-first pitch actually needs:

  * how the loop converged, split by *who did the work* —
      clean-on-arrival   (the LLM got it right first try — ergonomics,
                           NOT the fix loop)
      fixed-by-karac     (`karac fix` produced the clean build — the wedge)
      fixed-by-llm        (the LLM rewrote after reading a prose diagnostic —
                           diagnostic quality, still no human)
      non-converged      (hit the iteration cap)
  * fix mechanics — how many diagnostics offered a machine-applicable
    `replacement`, how many were applied, and how many actually resolved
    the error (vs. introduced a new one)
  * the gap ledger — every error code the LLM hit, ranked by frequency,
    flagged by whether that code has EVER carried a machine-applicable
    fix. Codes with no fix coverage are the ranked backlog for making the
    compiler more agent-fixable.

This reads the loop's output only; it never runs `karac` or an LLM and
never touches `mend.py`. Run it after any batch of `mend.py` runs:

    python3 examples/mend/harness/mend_score.py           # score runs/, print report
    python3 examples/mend/harness/mend_score.py --json     # machine summary on stdout
    python3 examples/mend/harness/mend_score.py path/to/runs

By default it writes `results.jsonl` (one record per run) and
`summary.json` next to the run directories. Both are inside `runs/`,
which is fully gitignored, so scoring never dirties the tree.

Disambiguation note: `mend.py` writes `outcome.txt` as literally
"clean-on-arrival" whenever a fresh LLM response checks clean — which at
iteration 0 means the LLM nailed it, but at a later iteration means the
LLM *fixed it from the diagnostics it was fed*. This scorer separates
those by the iteration index at which convergence happened, so the
headline numbers don't credit the fix loop for first-try successes (or
vice versa).
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
DEFAULT_RUNS_DIR = REPO_ROOT / "examples" / "mend" / "runs"


# ── low-level transcript readers ───────────────────────────────────


def _load_json(path: Path) -> dict | None:
    """Parse a JSON file; return None if absent or unparseable."""
    if not path.exists():
        return None
    try:
        return json.loads(path.read_text())
    except (json.JSONDecodeError, OSError):
        return None


def _diagnostics(envelope: dict | None) -> list[dict]:
    if not envelope:
        return []
    return envelope.get("diagnostics") or []


def _has_replacement(diag: dict) -> bool:
    return "replacement" in diag


def _count_applied_fixes(fix_log: str) -> int:
    """Parse `applied N fix(es) to …` out of a karac fix log."""
    for line in fix_log.splitlines():
        if "applied" in line and "fix" in line:
            for tok in line.split():
                if tok.isdigit():
                    return int(tok)
    return 0


# ── per-run scoring ────────────────────────────────────────────────


def _iter_dirs(run_dir: Path) -> list[Path]:
    return sorted(
        d for d in run_dir.iterdir() if d.is_dir() and d.name.startswith("iter_")
    )


def score_run(run_dir: Path) -> dict | None:
    """Reduce one runs/<timestamp>/ transcript to a flat record."""
    iters = _iter_dirs(run_dir)
    if not iters:
        return None

    codes_seen: dict[str, dict] = {}  # code -> {phase, sample, had_replacement}
    fixes_offered = 0
    fixes_applied = 0
    fixes_resolved = 0
    fix_introduced_new_error = False

    converging_iter: int | None = None
    outcome_value: str | None = None

    for idx, iter_dir in enumerate(iters):
        before = _diagnostics(_load_json(iter_dir / "diagnostics.json"))
        after_env = _load_json(iter_dir / "diagnostics.after_fix.json")
        after = _diagnostics(after_env) if after_env is not None else None
        fix_log_path = iter_dir / "fix.log"
        outcome_path = iter_dir / "outcome.txt"

        # taxonomy: every code the LLM's output triggered (pre-fix)
        for d in before:
            code = d.get("code", "<none>")
            rec = codes_seen.setdefault(
                code,
                {
                    "phase": d.get("phase", "?"),
                    "sample": d.get("message", ""),
                    "had_replacement": False,
                },
            )
            if _has_replacement(d):
                rec["had_replacement"] = True

        fixes_offered += sum(1 for d in before if _has_replacement(d))

        # fix mechanics: only meaningful when karac fix actually ran
        if fix_log_path.exists():
            fixes_applied += _count_applied_fixes(fix_log_path.read_text())
        if after is not None:
            before_codes = [d.get("code") for d in before]
            after_codes = [d.get("code") for d in after]
            fixes_resolved += max(0, len(before_codes) - len(after_codes))
            if any(c not in before_codes for c in after_codes):
                fix_introduced_new_error = True

        if outcome_path.exists() and converging_iter is None:
            converging_iter = idx
            outcome_value = outcome_path.read_text().strip()

    converged = converging_iter is not None or (run_dir / "final.kara").exists()
    outcome_class = _classify(converging_iter, outcome_value, converged)

    return {
        "run_id": run_dir.name,
        "n_iters": len(iters),
        "converged": converged,
        "converged_at_iter": converging_iter,
        "outcome_class": outcome_class,
        "error_codes_seen": sorted(codes_seen.keys()),
        "fixes_offered": fixes_offered,
        "fixes_applied": fixes_applied,
        "fixes_resolved": fixes_resolved,
        "fix_introduced_new_error": fix_introduced_new_error,
        # kept out of the top-level record but folded into the gap ledger:
        "_codes": codes_seen,
    }


def _classify(
    converging_iter: int | None, outcome_value: str | None, converged: bool
) -> str:
    if converging_iter is None:
        return "converged-untagged" if converged else "non-converged"
    if outcome_value == "clean-after-karac-fix":
        return "fixed-by-karac"
    if outcome_value == "clean-on-arrival":
        return "clean-on-arrival" if converging_iter == 0 else "fixed-by-llm"
    return outcome_value or "unknown"


# ── aggregation ────────────────────────────────────────────────────


ORDERED_CLASSES = [
    "clean-on-arrival",
    "fixed-by-karac",
    "fixed-by-llm",
    "converged-untagged",
    "non-converged",
]


def aggregate(records: list[dict]) -> dict:
    n = len(records)
    by_class: dict[str, int] = {}
    for r in records:
        by_class[r["outcome_class"]] = by_class.get(r["outcome_class"], 0) + 1

    converged = sum(1 for r in records if r["converged"])
    by_karac = by_class.get("fixed-by-karac", 0)
    by_llm = by_class.get("fixed-by-llm", 0)
    first_try = by_class.get("clean-on-arrival", 0)

    fixes_offered = sum(r["fixes_offered"] for r in records)
    fixes_applied = sum(r["fixes_applied"] for r in records)
    fixes_resolved = sum(r["fixes_resolved"] for r in records)

    iters_to_converge = [
        r["converged_at_iter"] + 1
        for r in records
        if r["converged"] and r["converged_at_iter"] is not None
    ]

    # gap ledger: fold per-run code tables into one, tracking whether any
    # occurrence of a code ever carried a machine-applicable replacement.
    ledger: dict[str, dict] = {}
    for r in records:
        for code, meta in r["_codes"].items():
            entry = ledger.setdefault(
                code,
                {
                    "code": code,
                    "phase": meta["phase"],
                    "runs_hit": 0,
                    "ever_machine_fixable": False,
                    "sample": meta["sample"],
                },
            )
            entry["runs_hit"] += 1
            entry["ever_machine_fixable"] = (
                entry["ever_machine_fixable"] or meta["had_replacement"]
            )

    gap_ledger = sorted(
        ledger.values(),
        key=lambda e: (e["ever_machine_fixable"], -e["runs_hit"], e["code"]),
    )

    def pct(x: int) -> float:
        return round(100.0 * x / n, 1) if n else 0.0

    return {
        "n_runs": n,
        "converged": converged,
        "non_converged": n - converged,
        "outcome_distribution": {
            cls: {"count": by_class.get(cls, 0), "pct": pct(by_class.get(cls, 0))}
            for cls in ORDERED_CLASSES
            if by_class.get(cls, 0)
        },
        "headline": {
            # the wedge: the compiler's fix machinery closed the loop
            "machine_fix_rate_pct": pct(by_karac),
            # the compiler kept the agent unblocked with no human, either
            # by an applicable fix or a diagnostic good enough to act on
            "agent_resolved_rate_pct": pct(by_karac + by_llm),
            # pure language ergonomics — LLM got it right unaided
            "clean_on_arrival_rate_pct": pct(first_try),
            "non_converged_rate_pct": pct(n - converged),
        },
        "fix_mechanics": {
            "diagnostics_offering_fix": fixes_offered,
            "fixes_applied": fixes_applied,
            "fixes_resolved": fixes_resolved,
            "fix_precision_pct": (
                round(100.0 * fixes_resolved / fixes_applied, 1)
                if fixes_applied
                else None
            ),
            "runs_where_fix_introduced_new_error": sum(
                1 for r in records if r["fix_introduced_new_error"]
            ),
        },
        "iterations_to_converge": {
            "mean": round(statistics.mean(iters_to_converge), 2)
            if iters_to_converge
            else None,
            "median": statistics.median(iters_to_converge)
            if iters_to_converge
            else None,
            "max": max(iters_to_converge) if iters_to_converge else None,
        },
        "gap_ledger": gap_ledger,
    }


# ── reporting ──────────────────────────────────────────────────────


def _bar(pct: float, width: int = 24) -> str:
    filled = int(round(pct / 100 * width))
    return "█" * filled + "·" * (width - filled)


def render_report(summary: dict) -> str:
    n = summary["n_runs"]
    lines: list[str] = []
    lines.append(f"Mend score — {n} run(s)\n" + "=" * 48)

    if n == 0:
        lines.append("no runs found.")
        return "\n".join(lines)

    lines.append("\nOutcome (who closed the loop):")
    for cls, d in summary["outcome_distribution"].items():
        lines.append(f"  {cls:<20} {d['count']:>3}  {d['pct']:>5.1f}%  {_bar(d['pct'])}")

    h = summary["headline"]
    lines.append("\nHeadline numbers:")
    lines.append(f"  machine-fix rate      {h['machine_fix_rate_pct']:>5.1f}%   (karac fix closed the loop — the wedge)")
    lines.append(f"  agent-resolved rate   {h['agent_resolved_rate_pct']:>5.1f}%   (no human: fix OR actionable diagnostic)")
    lines.append(f"  clean-on-arrival      {h['clean_on_arrival_rate_pct']:>5.1f}%   (LLM unaided — ergonomics, not the loop)")
    lines.append(f"  non-converged         {h['non_converged_rate_pct']:>5.1f}%   (hit the iteration cap)")

    fm = summary["fix_mechanics"]
    lines.append("\nFix mechanics:")
    lines.append(f"  diagnostics offering a fix : {fm['diagnostics_offering_fix']}")
    lines.append(f"  fixes applied              : {fm['fixes_applied']}")
    lines.append(f"  fixes resolved             : {fm['fixes_resolved']}")
    prec = fm["fix_precision_pct"]
    lines.append(f"  fix precision              : {'n/a' if prec is None else str(prec) + '%'}  (resolved / applied)")
    lines.append(f"  fixes that broke the build : {fm['runs_where_fix_introduced_new_error']} run(s)")

    it = summary["iterations_to_converge"]
    lines.append("\nIterations to converge:")
    lines.append(f"  mean {it['mean']}   median {it['median']}   max {it['max']}")

    lines.append("\nGap ledger — error codes the LLM hit, worst-covered first:")
    lines.append(f"  {'code':<8} {'phase':<10} {'runs':>4}  {'fixable':<7}  sample")
    for e in summary["gap_ledger"]:
        fixable = "yes" if e["ever_machine_fixable"] else "NO"
        sample = e["sample"][:52].replace("\n", " ")
        lines.append(
            f"  {e['code']:<8} {e['phase']:<10} {e['runs_hit']:>4}  {fixable:<7}  {sample}"
        )
    uncovered = [e for e in summary["gap_ledger"] if not e["ever_machine_fixable"]]
    if uncovered:
        lines.append(
            f"\n  → {len(uncovered)} error code(s) have NO machine-applicable fix. "
            "Each is a\n    ranked backlog item: give the diagnostic a `replacement`, or "
            "make it\n    precise enough for an agent to fix from prose."
        )
    return "\n".join(lines)


# ── CLI ────────────────────────────────────────────────────────────


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description="Aggregate Mend run transcripts.")
    p.add_argument(
        "runs_dir",
        nargs="?",
        default=str(DEFAULT_RUNS_DIR),
        help=f"Directory of run transcripts (default: {DEFAULT_RUNS_DIR}).",
    )
    p.add_argument(
        "--json",
        action="store_true",
        help="Print the machine summary to stdout instead of the report.",
    )
    p.add_argument(
        "--no-write",
        action="store_true",
        help="Do not write results.jsonl / summary.json to disk.",
    )
    args = p.parse_args(argv)

    runs_dir = Path(args.runs_dir)
    if not runs_dir.is_dir():
        print(f"[mend-score] no such runs dir: {runs_dir}", file=sys.stderr)
        return 2

    run_dirs = sorted(
        d
        for d in runs_dir.iterdir()
        if d.is_dir() and not d.name.startswith(".")
    )
    records = [r for d in run_dirs if (r := score_run(d)) is not None]
    summary = aggregate(records)

    if not args.no_write:
        results_path = runs_dir / "results.jsonl"
        summary_path = runs_dir / "summary.json"
        with results_path.open("w") as f:
            for r in records:
                flat = {k: v for k, v in r.items() if not k.startswith("_")}
                f.write(json.dumps(flat) + "\n")
        summary_path.write_text(json.dumps(summary, indent=2))
        if not args.json:
            print(f"[mend-score] wrote {results_path} and {summary_path}\n")

    if args.json:
        print(json.dumps(summary, indent=2))
    else:
        print(render_report(summary))
    return 0


if __name__ == "__main__":
    sys.exit(main())
