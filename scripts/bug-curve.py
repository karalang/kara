#!/usr/bin/env python3
"""Render the Kāra bug curve from docs/bug-ledger.jsonl.

The point of the ledger is to make "are we still finding bugs?" a number you
can watch flatten — per surface (codegen/ownership/…) and per source
(kata/selfhost/dogfood/internal). This script is dependency-free: it prints a
markdown report to stdout and writes a minimal cumulative-curve SVG.

Usage:
    python3 scripts/bug-curve.py                  # report to stdout
    python3 scripts/bug-curve.py --svg out.svg    # also write the SVG
    python3 scripts/bug-curve.py --inject f.md    # splice the generated state
                                                  # block into f.md (between the
                                                  # GEN markers — never edit it
                                                  # by hand; the ledger is canon)
"""
import json
import sys
from collections import Counter, defaultdict
from datetime import date, timedelta
from pathlib import Path

LEDGER = Path(__file__).resolve().parent.parent / "docs" / "bug-ledger.jsonl"


def load():
    rows = [json.loads(l) for l in LEDGER.read_text(encoding="utf-8").splitlines() if l.strip()]
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

    # Surfaces split on '+' so a "typecheck+codegen" bug counts under each
    # (the compound value itself no longer fragments the table).
    surf = Counter()
    surfo = Counter()
    for r in rows:
        for seg in r["surface"].split("+"):
            surf[seg] += 1
            if r["status"] == "open":
                surfo[seg] += 1
    out.append("## By surface\n")
    out.append("| surface | total | open | |")
    out.append("|---|---|---|---|")
    for k, n in surf.most_common():
        out.append(f"| {k or '—'} | {n} | {surfo.get(k, 0)} | {bar(n)} |")
    out.append("")
    # Failure-mode class — a CONTROLLED vocabulary (canonicalized 2026-07-17):
    # miscompile, double-free, use-after-free, leak, crash, codegen-gap,
    # missing-feature, false-positive, soundness, run-vs-build, diagnostics,
    # perf, other. New entries must use one of these (nuance goes in `detail`).
    breakdown("class", "class")
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
    Path(path).write_text("\n".join(s), encoding="utf-8", newline="\n")


GEN_BEGIN = "<!-- BUG-LEDGER:GENERATED:BEGIN -->"
GEN_END = "<!-- BUG-LEDGER:GENERATED:END -->"


def short_title(r) -> str:
    """A compact one-line title for the collapsed Fixed index — the first
    sentence, hard-capped. The full prose stays in `bug-ledger.jsonl` (grep by
    id for it); dumping every fixed entry's full multi-paragraph title into the
    `.md` is what bloated the rendered view to ~0.5 MB, which an LLM opening the
    file would swallow whole. Pipes are escaped so a title never breaks the
    markdown table."""
    t = " ".join(r["title"].split())
    s = t.split(". ", 1)[0]
    if len(s) > 150:
        s = s[:149].rstrip() + "…"
    return s.replace("|", "\\|")


def ledger_view(rows) -> str:
    """The human-readable rolled-up state — Open bugs in full, Fixed as a
    compact one-line-per-entry index.

    This is what makes `bug-ledger.jsonl` (the canonical, machine-countable
    source of truth) readable without a second hand-maintained tracker. It is
    regenerated by `--inject`; do not hand-edit the block it lands in. Open bugs
    render in full (you act on them); fixed bugs collapse to id · surface · sev ·
    one-line title · fix SHA — the regression test is the durable artifact and
    the full write-up lives in the ledger, grep-able by id.
    """
    total = len(rows)
    opens = [r for r in rows if r["status"] == "open"]
    fixed = [r for r in rows if r["status"] == "fixed"]
    out = []
    out.append(GEN_BEGIN)
    # Compact retro tables: failure-mode class and surface (compound surfaces
    # count under each '+' segment).
    from collections import Counter as _C
    cls = _C(r["class"] for r in rows)
    clso = _C(r["class"] for r in rows if r["status"] == "open")
    surf, surfo = _C(), _C()
    for r in rows:
        for seg in r["surface"].split("+"):
            surf[seg] += 1
            if r["status"] == "open":
                surfo[seg] += 1
    out.append("\n### By class\n")
    out.append("| class | total | open |")
    out.append("|---|---|---|")
    for k, n in cls.most_common():
        out.append(f"| {k or '—'} | {n} | {clso.get(k, 0)} |")
    out.append("\n### By surface\n")
    out.append("| surface | total | open |")
    out.append("|---|---|---|")
    for k, n in surf.most_common():
        out.append(f"| {k} | {n} | {surfo.get(k, 0)} |")
    out.append("## Current state")
    out.append("")
    out.append(
        f"_Generated from `bug-ledger.jsonl` by `scripts/bug-curve.py` — "
        f"**{total} surfaced · {len(opens)} open · {len(fixed)} fixed** "
        f"({rows[0]['date']} → {rows[-1]['date']}). Do not edit this block by "
        f"hand; edit the ledger and regenerate._"
    )
    out.append("")
    out.append(f"### Open ({len(opens)})")
    out.append("")
    if opens:
        out.append("| id | date | surface | sev | title | tracker |")
        out.append("|---|---|---|---|---|---|")
        for r in sorted(opens, key=lambda r: r["date"]):
            out.append(
                f"| {r['id']} | {r['date']} | {r['surface']} | {r['severity']} "
                f"| {r['title']} | {r.get('tracker','') or '—'} |"
            )
    else:
        out.append("_None — the ledger is fully drained._")
    out.append("")
    out.append(f"### Fixed ({len(fixed)})")
    out.append("")
    out.append(
        "<details><summary>"
        f"{len(fixed)} fixed — compact index (one-line titles; full write-up + "
        "cross-refs live in `bug-ledger.jsonl`, grep by id). The regression test "
        "is the durable artifact.</summary>\n"
    )
    out.append("| id | surface | sev | title | fix |")
    out.append("|---|---|---|---|---|")
    for r in sorted(fixed, key=lambda r: r["date"]):
        fix = r.get("fix", "") or "—"
        out.append(
            f"| {r['id']} | {r['surface']} | {r['severity']} | {short_title(r)} "
            f"| {fix} |"
        )
    out.append("\n</details>")
    out.append("")
    out.append(GEN_END)
    return "\n".join(out)


def inject(rows, md_path):
    p = Path(md_path)
    text = p.read_text(encoding="utf-8")
    block = ledger_view(rows)
    if GEN_BEGIN in text and GEN_END in text:
        pre = text[: text.index(GEN_BEGIN)]
        post = text[text.index(GEN_END) + len(GEN_END) :]
        text = pre + block + post
    else:
        # First injection: append the block after the hand-written standard.
        if not text.endswith("\n"):
            text += "\n"
        text += "\n" + block + "\n"
    p.write_text(text, encoding="utf-8", newline="\n")


def main():
    # The report (and the generated block) carry non-ASCII — box-drawing bars,
    # arrows, "Kāra". Force UTF-8 for stdout so report mode doesn't die on a
    # Windows cp1252 console (file writes pin encoding="utf-8" at each call site).
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    rows = load()
    if "--inject" in sys.argv:
        out = sys.argv[sys.argv.index("--inject") + 1]
        inject(rows, out)
        print(f"_injected generated state into {out}_", file=sys.stderr)
    else:
        print(report(rows))
    if "--svg" in sys.argv:
        out = sys.argv[sys.argv.index("--svg") + 1]
        svg(rows, out)
        print(f"\n_wrote {out}_", file=sys.stderr)


if __name__ == "__main__":
    main()
