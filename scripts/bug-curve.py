#!/usr/bin/env python3
"""Render the Kāra bug curve from docs/bug-ledger.jsonl.

The point of the ledger is to make "are we still finding bugs?" a number you
can watch flatten — per surface (codegen/ownership/…) and per source
(kata/selfhost/dogfood/internal). This script is dependency-free: it prints a
markdown report to stdout and writes a minimal cumulative-curve SVG.

Usage:
    python3 scripts/bug-curve.py                  # report to stdout
    python3 scripts/bug-curve.py --svg out.svg    # also write the SVG
"""
import json
import sys
from collections import Counter, defaultdict
from datetime import date, timedelta
from pathlib import Path

LEDGER = Path(__file__).resolve().parent.parent / "docs" / "bug-ledger.jsonl"


def load():
    rows = [json.loads(l) for l in LEDGER.read_text().splitlines() if l.strip()]
    rows.sort(key=lambda r: r["date"])
    return rows


def iso_week(d: str) -> str:
    y, w, _ = date.fromisoformat(d).isocalendar()
    return f"{y}-W{w:02d}"


def bar(n: int, scale: int = 1) -> str:
    return "█" * (n * scale)


def report(rows) -> str:
    out = []
    total = len(rows)
    openn = sum(1 for r in rows if r["status"] == "open")
    out.append(f"# Kāra bug curve — {total} surfaced, {openn} open, {total - openn} fixed\n")
    out.append(f"_Span: {rows[0]['date']} → {rows[-1]['date']}_\n")

    # Per-week new + cumulative — the flattening signal.
    per_week = Counter(iso_week(r["date"]) for r in rows)
    out.append("## Per-week (new surfaced → cumulative)\n")
    out.append("| week | new | cumulative | |")
    out.append("|---|---|---|---|")
    cum = 0
    for wk in sorted(per_week):
        n = per_week[wk]
        cum += n
        out.append(f"| {wk} | {n} | {cum} | {bar(n)} |")
    out.append("")

    # Per-day (finer grain, recent activity).
    per_day = Counter(r["date"] for r in rows)
    out.append("## Per-day\n")
    out.append("| date | new | |")
    out.append("|---|---|---|")
    for d in sorted(per_day):
        out.append(f"| {d} | {per_day[d]} | {bar(per_day[d])} |")
    out.append("")

    def breakdown(title, key):
        c = Counter(r[key] for r in rows)
        co = Counter(r[key] for r in rows if r["status"] == "open")
        out.append(f"## By {title}\n")
        out.append(f"| {title} | total | open | |")
        out.append("|---|---|---|---|")
        for k, n in c.most_common():
            out.append(f"| {k or '—'} | {n} | {co.get(k, 0)} | {bar(n)} |")
        out.append("")

    breakdown("surface", "surface")
    breakdown("severity", "severity")
    # source grouped by family (kata: / selfhost: / dogfood: / internal)
    fam = Counter(r["source"].split(":")[0] for r in rows)
    famo = Counter(r["source"].split(":")[0] for r in rows if r["status"] == "open")
    out.append("## By source family\n")
    out.append("| source | total | open | |")
    out.append("|---|---|---|---|")
    for k, n in fam.most_common():
        out.append(f"| {k} | {n} | {famo.get(k, 0)} | {bar(n)} |")
    out.append("")

    # Katas as bug-finders — the launch-gate slice.
    katas = Counter(r["source"] for r in rows if r["source"].startswith("kata:"))
    out.append("## Kata bug-finders (launch-gate slice)\n")
    out.append(f"_{sum(katas.values())} bugs across {len(katas)} katas._\n")
    out.append("| kata | bugs surfaced |")
    out.append("|---|---|")
    for k, n in katas.most_common():
        out.append(f"| {k} | {n} |")
    out.append("")
    return "\n".join(out)


def svg(rows, path):
    """Minimal hand-rolled cumulative-curve SVG (no matplotlib dependency)."""
    days = sorted({r["date"] for r in rows})
    d0 = date.fromisoformat(rows[0]["date"])
    d1 = date.fromisoformat(rows[-1]["date"])
    span = max((d1 - d0).days, 1)
    cum_by_day = defaultdict(int)
    for r in rows:
        cum_by_day[r["date"]] += 1
    pts, run = [], 0
    for dd in days:
        run += cum_by_day[dd]
        x = (date.fromisoformat(dd) - d0).days / span
        pts.append((x, run))
    total = run
    W, H, P = 720, 300, 40
    def px(x):
        return P + x * (W - 2 * P)
    def py(y):
        return H - P - (y / total) * (H - 2 * P)
    poly = " ".join(f"{px(x):.1f},{py(y):.1f}" for x, y in pts)
    s = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" font-family="monospace" font-size="12">']
    s.append(f'<rect width="{W}" height="{H}" fill="white"/>')
    s.append(f'<polyline fill="none" stroke="#e8730a" stroke-width="2" points="{poly}"/>')
    for x, y in pts:
        s.append(f'<circle cx="{px(x):.1f}" cy="{py(y):.1f}" r="2.5" fill="#e8730a"/>')
    s.append(f'<text x="{P}" y="{H-12}">{rows[0]["date"]}</text>')
    s.append(f'<text x="{W-P-70}" y="{H-12}">{rows[-1]["date"]}</text>')
    s.append(f'<text x="{P}" y="20">cumulative bugs surfaced: {total}</text>')
    s.append("</svg>")
    Path(path).write_text("\n".join(s))


def main():
    rows = load()
    print(report(rows))
    if "--svg" in sys.argv:
        out = sys.argv[sys.argv.index("--svg") + 1]
        svg(rows, out)
        print(f"\n_wrote {out}_", file=sys.stderr)


if __name__ == "__main__":
    main()
