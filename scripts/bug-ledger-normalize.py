#!/usr/bin/env python3
"""Rewrite docs/bug-ledger.jsonl in its canonical form (B-2026-08-07-13).

WHY THIS EXISTS. The ledger has no canonical writer — every lane appends to it
with an ad-hoc script, and `json.dumps` escapes non-ASCII by DEFAULT while
`ensure_ascii=False` leaves it raw. Both round-trip losslessly, so nothing
noticed, and the file flipped between the two forms FOUR times on 2026-08-07
alone. Each flip rewrites ~850 of the ~1000 rows, which buries the one-line
semantic change in an 850-line diff, defeats `git blame`, and guarantees a
conflict for every concurrent lane touching the file.

THE CANONICAL FORM is `json.dumps(row, ensure_ascii=False)` — one row per
line, keys in the row's own order, `json.dumps`'s default separators.

WHY RAW UTF-8 rather than the `\\uXXXX` default: CLAUDE.md's instruction for
this file is to GREP it ("open bugs are `grep '"status": "open"'`", "grep it
by id"), and it is read by people and by models. Escaped output turns every
em-dash in the prose into `\\u2014`, and a grep for a phrase containing one
fails outright. The file is a human-read artifact; the encoding should match.

USAGE:
    python3 scripts/bug-ledger-normalize.py          # rewrite in place
    python3 scripts/bug-ledger-normalize.py --check  # exit 1 if not canonical

`scripts/bug-lint.sh` runs the check, so the convention is ENFORCED rather
than hoped-for — which is the whole point, since a convention nobody can
check is what produced the flip.
"""

import json
import pathlib
import sys

LEDGER = pathlib.Path(__file__).resolve().parent.parent / "docs" / "bug-ledger.jsonl"


def canonical(line: str) -> str:
    return json.dumps(json.loads(line), ensure_ascii=False)


def main() -> int:
    check_only = "--check" in sys.argv[1:]
    lines = LEDGER.read_text(encoding="utf-8").splitlines()
    out, bad = [], []
    for i, line in enumerate(lines, start=1):
        if not line.strip():
            continue
        try:
            canon = canonical(line)
        except json.JSONDecodeError as e:
            # Invalid JSON is bug-lint.sh's check 1, not this one's; leave the
            # line alone so the caller reports it where the message belongs.
            print(f"line {i}: not valid JSON ({e}) — see bug-lint.sh check 1")
            return 1
        if canon != line:
            bad.append(i)
        out.append(canon)

    if check_only:
        if bad:
            print(
                f"{len(bad)} of {len(out)} ledger row(s) are not in canonical form "
                f"(first: line {bad[0]}).\n"
                "Almost always this is `json.dumps` ASCII-escaping: pass "
                "`ensure_ascii=False` when you write the file.\n"
                "Fix with: python3 scripts/bug-ledger-normalize.py"
            )
            return 1
        return 0

    LEDGER.write_text("\n".join(out) + "\n", encoding="utf-8")
    print(f"normalized {len(bad)} of {len(out)} row(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
