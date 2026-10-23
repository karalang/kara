#!/usr/bin/env python3
"""Split the bug ledger at M3 (M3 plan item 10; review/m3-item10-design.md).

Before: one file, docs/bug-ledger.jsonl.
After:
  docs/legacy/bug-ledger.jsonl   the archive, read-only from then on
  docs/bugs-v2.jsonl             the live ledger

What the split does, computed from whatever the ledger holds when it runs:
  1. The archive is the old file, moved.
  2. Every row still `open` is copied VERBATIM into docs/bugs-v2.jsonl, keeping
     its id, so every citation in code and katas still names one row.
  3. In the archive, each copied row becomes a stub: status `relocated`,
     tracker `docs/bugs-v2.jsonl`. Its other fields are kept, so the archive
     still reads as history and bug-lint's "no published row disappears" rule
     holds without a special case.
  4. Rows relocated to the M3 legacy deletion (tracker starting "v2 redesign
     M3") are retracked. A row whose id a corpus program cites points at that
     program; every other row points at the archive ref holding the deleted
     code and at the legacy test programs now in corpus/fixtures.

It refuses to run twice (the old file must exist and the new ones must not), and
it rewrites only what the four steps name. Re-run it on the day rather than
reusing an old split: rows keep arriving.

USAGE
    python3 scripts/ledger-split.py --archive-ref <ref>   # e.g. archive/legacy-m3
    python3 scripts/ledger-split.py --archive-ref <ref> --dry-run
"""

import argparse
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
OLD = ROOT / "docs" / "bug-ledger.jsonl"
OLD_MD = ROOT / "docs" / "bug-ledger.md"
ARCHIVE = ROOT / "docs" / "legacy" / "bug-ledger.jsonl"
ARCHIVE_MD = ROOT / "docs" / "legacy" / "bug-ledger.md"
LIVE = ROOT / "docs" / "bugs-v2.jsonl"
M3_TRACKER = "v2 redesign M3"
CORPUS_EXT = {".toml", ".kara", ".md", ".out"}
BID = re.compile(r"B-20\d\d-\d\d-\d\d-\d+")


def corpus_citations(ids):
    """id -> sorted corpus entry dirs whose files cite it."""
    cites = {}
    for p in (ROOT / "corpus").rglob("*"):
        if not p.is_file() or p.suffix not in CORPUS_EXT:
            continue
        try:
            text = p.read_text(errors="ignore")
        except OSError:
            continue
        for bid in set(BID.findall(text)) & ids:
            cites.setdefault(bid, set()).add(str(p.parent.relative_to(ROOT)))
    return {k: sorted(v) for k, v in cites.items()}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--archive-ref", required=True,
                    help="the git ref Gowtham created for the legacy code")
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args()

    if not OLD.exists():
        sys.exit(f"ledger-split: {OLD.relative_to(ROOT)} not found (already split?)")
    for p in (ARCHIVE, LIVE):
        if p.exists():
            sys.exit(f"ledger-split: {p.relative_to(ROOT)} already exists; refusing to split twice")

    lines = [l for l in OLD.read_text(encoding="utf-8").splitlines() if l.strip()]
    rows = [json.loads(l) for l in lines]

    m3 = {r["id"] for r in rows
          if r.get("status") == "relocated" and (r.get("tracker") or "").startswith(M3_TRACKER)}
    cites = corpus_citations(m3)

    live, archive = [], []
    n_open = n_corpus = n_ref = 0
    for line, r in zip(lines, rows):
        if r.get("status") == "open":
            live.append(line)  # verbatim
            r = dict(r, status="relocated", tracker="docs/bugs-v2.jsonl")
            archive.append(json.dumps(r, ensure_ascii=False))
            n_open += 1
        elif r["id"] in m3:
            if r["id"] in cites:
                tracker = "; ".join(cites[r["id"]])
                n_corpus += 1
            else:
                tracker = (f"archived with legacy at {a.archive_ref}; legacy test programs "
                           "are in corpus/fixtures (src:legacy-tests)")
                n_ref += 1
            archive.append(json.dumps(dict(r, tracker=tracker), ensure_ascii=False))
        else:
            archive.append(line)

    print(f"rows {len(rows)}: {n_open} open -> docs/bugs-v2.jsonl; "
          f"{n_corpus} M3 rows -> corpus path; {n_ref} M3 rows -> {a.archive_ref}")
    if a.dry_run:
        return 0

    ARCHIVE.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "mv", str(OLD), str(ARCHIVE)], cwd=ROOT, check=True)
    ARCHIVE.write_text("\n".join(archive) + "\n", encoding="utf-8")
    LIVE.write_text("\n".join(live) + "\n", encoding="utf-8")
    if OLD_MD.exists():
        subprocess.run(["git", "mv", str(OLD_MD), str(ARCHIVE_MD)], cwd=ROOT, check=True)
    subprocess.run(["git", "add", str(ARCHIVE), str(LIVE)], cwd=ROOT, check=True)
    print("wrote docs/legacy/bug-ledger.jsonl and docs/bugs-v2.jsonl; "
          "now run `python3 scripts/bug-curve.py --inject docs/bugs-v2.md` and scripts/bug-lint.sh")
    return 0


if __name__ == "__main__":
    sys.exit(main())
