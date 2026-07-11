#!/usr/bin/env python3
# ruff: noqa: E501
"""bench_gate.py — CI regression gate for the ws_idle_holder flagship demo.

Phase 6.3 Slice 7 (docs/implementation_checklist/phase-6-runtime.md). Consumes
one JSON report from the bench harness (`examples/ws_idle_holder/bench`, run in
idle-hold mode with churn) and gates a CI run against a committed baseline.

Two independent things are checked, in this order:

  1. CORRECTNESS (hard, never overridable) — the server actually held the load:
     every requested connection established, zero connect failures, zero churn
     reconnect failures, and the harness's own `ok` flag is true. A broken build
     that drops connections is not a "5%-regression" question; an override must
     not let it merge.

  2. REGRESSION vs a committed baseline (overridable with justification) — the
     tracked steady-state metrics did not get more than `tolerance_pct` worse:
       - connect.p50_ms / p95_ms / p99_ms / p999_ms   (connection-establishment
         cost — tracked SEPARATELY from memory per the roadmap)
       - memory.per_conn_bytes                        (idle density — the
         scale- & machine-invariant headline metric; the tight gate)
       - churn.cliff_ratio                            (P99 latency cliff under
         churn — also has an absolute ceiling)

Higher is worse for every tracked metric. A metric whose baseline is `null` is
RECORD-ONLY (printed, not gated) — the escape hatch for values that still need
calibrating against a given runner's noise floor. A metric with a `ceiling`
also hard-fails when the observed value exceeds it, independent of the baseline.

Latency on a shared CI runner is inherently noisy, so its default tolerances are
wide and the per-conn-memory tolerance is tight — density is what actually
regresses deterministically. Tolerances live in the baseline JSON and can be
tightened per-metric with no code change once a runner is characterized.

Override: set env `BENCH_GATE_OVERRIDE` to a non-empty justification string to
downgrade REGRESSION failures to warnings (exit 0). Correctness failures are
never downgraded. In CI the token is lifted from the head commit message
(`[bench-override: <why>]`) — see .github/workflows/ci.yml.

Usage:
  bench_gate.py --report r.json --baseline b.json     # gate (default)
  bench_gate.py --report r.json --baseline b.json --update-baseline
  bench_gate.py --selftest
"""

from __future__ import annotations

import argparse
import json
import os
import sys

# Dotted path -> report field. Higher is worse for all of these.
TRACKED_METRICS = [
    "connect.p50_ms",
    "connect.p95_ms",
    "connect.p99_ms",
    "connect.p999_ms",
    "memory.per_conn_bytes",
    "churn.cliff_ratio",
]

GH = os.environ.get("GITHUB_ACTIONS") == "true"


# ── small helpers ────────────────────────────────────────────────────────────


def dig(obj, dotted):
    """Fetch obj["a"]["b"] for "a.b"; None if any segment is missing/null."""
    cur = obj
    for seg in dotted.split("."):
        if not isinstance(cur, dict) or cur.get(seg) is None:
            return None
        cur = cur[seg]
    return cur


def annotate(level, msg):
    """Emit a GitHub Actions annotation when running under Actions."""
    if GH:
        print(f"::{level}::{msg}")


def fmt(v):
    if v is None:
        return "—"
    if isinstance(v, float):
        return f"{v:.2f}"
    return str(v)


# ── core evaluation ──────────────────────────────────────────────────────────


def check_correctness(report, correctness):
    """Return a list of hard-failure strings (empty == passed)."""
    fails = []
    cfg = report.get("config", {})
    connect = report.get("connect", {})
    requested = cfg.get("connections")
    established = connect.get("established")
    failed = connect.get("failed")

    if correctness.get("require_ok", True) and not report.get("ok", False):
        fails.append("harness reported ok=false")
    if correctness.get("require_zero_failed", True) and failed:
        fails.append(f"{failed} connect failure(s)")
    if correctness.get("require_all_established", True):
        if requested is not None and established != requested:
            fails.append(f"established {established}/{requested} connections")
    if correctness.get("require_zero_reconnect_failed", True):
        churn = report.get("churn")
        if churn and churn.get("reconnect_failed"):
            fails.append(f"{churn['reconnect_failed']} churn reconnect failure(s)")
    return fails


def evaluate_metric(name, observed, spec):
    """Classify one metric.

    Returns (status, detail) where status is one of:
      "pass" | "record" | "unavailable" | "regress"
    """
    baseline = spec.get("baseline")
    tol = spec.get("tolerance_pct", 5.0)
    ceiling = spec.get("ceiling")

    if observed is None:
        # Baseline expected a value but the run produced none (e.g. RSS
        # unavailable). Surface it, but don't hard-fail the gate on a missing
        # measurement — that is an environment problem, not a regression.
        return ("unavailable", "metric absent from report")

    if ceiling is not None and observed > ceiling:
        return ("regress", f"exceeds absolute ceiling {fmt(ceiling)}")

    if baseline is None:
        return ("record", "record-only (baseline not calibrated)")

    limit = baseline * (1.0 + tol / 100.0)
    delta_pct = ((observed - baseline) / baseline * 100.0) if baseline else 0.0
    if observed > limit:
        return ("regress", f"{delta_pct:+.1f}% vs baseline (allowed +{tol:.0f}%)")
    return ("pass", f"{delta_pct:+.1f}% vs baseline (allowed +{tol:.0f}%)")


def run_gate(report, baseline, override):
    """Evaluate a report against a baseline. Returns (exit_code, printed lines)."""
    lines = []
    metrics_spec = baseline.get("metrics", {})
    correctness = baseline.get("correctness", {})

    # 1. Correctness — hard, non-overridable.
    corr_fails = check_correctness(report, correctness)

    # 2. Regression table.
    rows = []
    regressions = []
    for name in TRACKED_METRICS:
        if name not in metrics_spec:
            continue
        spec = metrics_spec[name]
        observed = dig(report, name)
        status, detail = evaluate_metric(name, observed, spec)
        rows.append((name, spec.get("baseline"), observed, status, detail))
        if status == "regress":
            regressions.append((name, detail))

    # ── render ──
    lines.append("")
    lines.append("ws_idle_holder bench gate")
    lines.append("=" * 74)
    tier = baseline.get("tier", {})
    cfg = report.get("config", {})
    lines.append(
        f"  tier: {cfg.get('connections', tier.get('connections'))} conns, "
        f"concurrency {cfg.get('concurrency', tier.get('concurrency'))}, "
        f"churn {cfg.get('churn_rounds', tier.get('churn_rounds'))} round(s)"
    )
    lines.append("")
    lines.append(f"  {'metric':<24}{'baseline':>12}{'observed':>12}  {'verdict':<8} detail")
    lines.append("  " + "-" * 72)
    symbol = {
        "pass": "ok",
        "record": "rec",
        "unavailable": "n/a",
        "regress": "FAIL",
    }
    for name, base, observed, status, detail in rows:
        lines.append(
            f"  {name:<24}{fmt(base):>12}{fmt(observed):>12}  "
            f"{symbol[status]:<8} {detail}"
        )
    lines.append("")

    # ── correctness verdict ──
    if corr_fails:
        for f in corr_fails:
            annotate("error", f"bench gate correctness failure: {f}")
        lines.append("  CORRECTNESS: FAILED (not overridable)")
        for f in corr_fails:
            lines.append(f"    - {f}")
        lines.append("")
        lines.append("RESULT: FAIL (server did not hold the load)")
        return (1, lines)
    lines.append("  correctness: passed (all established, 0 failed, 0 churn failures)")

    # ── regression verdict ──
    if not regressions:
        lines.append("")
        lines.append("RESULT: PASS")
        return (0, lines)

    for name, detail in regressions:
        annotate("error", f"bench gate regression: {name} {detail}")
    lines.append("")
    lines.append(f"  REGRESSIONS ({len(regressions)}):")
    for name, detail in regressions:
        lines.append(f"    - {name}: {detail}")

    if override:
        lines.append("")
        lines.append(f"  OVERRIDE ACTIVE: {override}")
        annotate("warning", f"bench gate regressions overridden: {override}")
        lines.append("")
        lines.append("RESULT: PASS (regressions overridden with justification)")
        return (0, lines)

    lines.append("")
    lines.append(
        "  To override: set BENCH_GATE_OVERRIDE with a justification, or add"
    )
    lines.append("  '[bench-override: <reason>]' to the head commit message.")
    lines.append("")
    lines.append("RESULT: FAIL (regression over threshold)")
    return (1, lines)


def update_baseline(report, baseline):
    """Fold observed values into a baseline's `baseline` fields + `tier`."""
    baseline.setdefault("metrics", {})
    for name in TRACKED_METRICS:
        observed = dig(report, name)
        if observed is None:
            continue
        spec = baseline["metrics"].setdefault(name, {"tolerance_pct": 5.0})
        spec["baseline"] = round(observed, 3) if isinstance(observed, float) else observed
    cfg = report.get("config", {})
    baseline.setdefault("tier", {})
    for key in ("connections", "concurrency", "churn_rounds", "churn_fraction"):
        if key in cfg:
            baseline["tier"][key] = cfg[key]
    return baseline


# ── self-test (no build needed; runs in CI as a fast guard) ───────────────────


def _selftest():
    passed = 0

    def check(name, cond):
        nonlocal passed
        status = "ok" if cond else "FAIL"
        print(f"  [{status}] {name}")
        if cond:
            passed += 1
        return cond

    base = {
        "tier": {"connections": 2000},
        "metrics": {
            "connect.p99_ms": {"baseline": 40.0, "tolerance_pct": 50.0},
            "memory.per_conn_bytes": {"baseline": 14000.0, "tolerance_pct": 8.0},
            "churn.cliff_ratio": {"baseline": 1.2, "tolerance_pct": 50.0, "ceiling": 3.0},
        },
        "correctness": {
            "require_ok": True,
            "require_zero_failed": True,
            "require_all_established": True,
            "require_zero_reconnect_failed": True,
        },
    }

    def report(**over):
        r = {
            "ok": True,
            "config": {"connections": 2000, "concurrency": 200, "churn_rounds": 3},
            "connect": {"established": 2000, "failed": 0, "p99_ms": 38.0},
            "memory": {"per_conn_bytes": 14200.0},
            "churn": {"reconnect_failed": 0, "cliff_ratio": 1.1},
        }
        for k, v in over.items():
            if "." in k:
                seg, field = k.split(".")
                r.setdefault(seg, {})[field] = v
            else:
                r[k] = v
        return r

    all_ok = True

    # clean pass (within tolerance; observed memory +1.4% < 8%)
    code, _ = run_gate(report(), base, "")
    all_ok &= check("clean run passes", code == 0)

    # memory regression: 14000 -> 16000 = +14.3% > 8%
    code, _ = run_gate(report(**{"memory.per_conn_bytes": 16000.0}), base, "")
    all_ok &= check("memory regression fails", code == 1)

    # ...overridden -> pass
    code, _ = run_gate(report(**{"memory.per_conn_bytes": 16000.0}), base, "hotfix")
    all_ok &= check("memory regression overridden passes", code == 0)

    # improvement never fails (memory much lower)
    code, _ = run_gate(report(**{"memory.per_conn_bytes": 9000.0}), base, "")
    all_ok &= check("improvement passes", code == 0)

    # latency spike within wide tolerance passes (40 -> 58 = +45% < 50%)
    code, _ = run_gate(report(**{"connect.p99_ms": 58.0}), base, "")
    all_ok &= check("latency within tolerance passes", code == 0)

    # latency spike over tolerance fails (40 -> 70 = +75% > 50%)
    code, _ = run_gate(report(**{"connect.p99_ms": 70.0}), base, "")
    all_ok &= check("latency over tolerance fails", code == 1)

    # correctness: a connect failure hard-fails even with override
    code, _ = run_gate(report(**{"connect.failed": 5, "ok": False}), base, "force")
    all_ok &= check("correctness failure not overridable", code == 1)

    # correctness: short establishment count hard-fails
    code, _ = run_gate(report(**{"connect.established": 1990}), base, "")
    all_ok &= check("short establishment fails", code == 1)

    # cliff ceiling: 1.2 baseline, observed 3.5 > 3.0 ceiling -> fail
    code, _ = run_gate(report(**{"churn.cliff_ratio": 3.5}), base, "")
    all_ok &= check("cliff over ceiling fails", code == 1)

    # record-only metric (baseline null) never fails
    nb = json.loads(json.dumps(base))
    nb["metrics"]["connect.p99_ms"]["baseline"] = None
    code, _ = run_gate(report(**{"connect.p99_ms": 999.0}), nb, "")
    all_ok &= check("record-only metric does not gate", code == 0)

    # update-baseline folds observed numbers in
    ub = update_baseline(report(**{"memory.per_conn_bytes": 13333.3}), json.loads(json.dumps(base)))
    all_ok &= check(
        "update-baseline folds observed value",
        ub["metrics"]["memory.per_conn_bytes"]["baseline"] == 13333.3,
    )

    print(f"\n  {passed} checks passed")
    return 0 if all_ok else 1


# ── entry point ──────────────────────────────────────────────────────────────


def main(argv=None):
    p = argparse.ArgumentParser(description="ws_idle_holder CI benchmark gate")
    p.add_argument("--report", help="bench harness JSON output")
    p.add_argument("--baseline", help="committed baseline JSON")
    p.add_argument(
        "--update-baseline",
        action="store_true",
        help="fold the report's observed values into the baseline and rewrite it",
    )
    p.add_argument("--selftest", action="store_true", help="run gate-logic self-tests")
    args = p.parse_args(argv)

    if args.selftest:
        print("bench_gate self-test")
        return _selftest()

    if not args.report or not args.baseline:
        p.error("--report and --baseline are required (unless --selftest)")

    with open(args.report) as f:
        report = json.load(f)
    with open(args.baseline) as f:
        baseline = json.load(f)

    if args.update_baseline:
        updated = update_baseline(report, baseline)
        with open(args.baseline, "w") as f:
            json.dump(updated, f, indent=2)
            f.write("\n")
        print(f"baseline updated: {args.baseline}")
        return 0

    override = os.environ.get("BENCH_GATE_OVERRIDE", "").strip()
    code, lines = run_gate(report, baseline, override)
    print("\n".join(lines))
    return code


if __name__ == "__main__":
    sys.exit(main())
