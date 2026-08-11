#!/usr/bin/env python3
"""Close a ledger row by B-ID — verifying, before writing, that the row on disk
is still the bug the caller means to close.

WHY THIS EXISTS. On 2026-08-11 two concurrent sessions filed ledger rows within
the same few minutes. B-IDs are allocated by reading the highest id in the file,
so both computed `B-2026-08-11-11`; one landed first. The second session then
ran its own ad-hoc close script, which found the row BY ID ALONE and wrote its
title, fix and status onto the other session's row. The result was a single
hybrid row — one session's `source`/`class`/`detail` under the other's title and
fix — and the second bug, a FIXED HIGH-SEVERITY DOUBLE FREE, had no row anywhere
in the ledger. It came within one push of vanishing from the queue's history.

`scripts/bug-lint.sh` could not have caught it. The lint checks that B-IDs are
unique, and they were: the write was in-place, so there was only ever one row
with that id. The corruption was in the row's CONTENT, which no integrity check
on the file can detect — the guard has to run at the moment of the write, in the
one place that knows which bug the caller believes it is closing.

Until now no such place existed. There is no canonical closer in this repo, so
every lane hand-rolls a script that mutates the row keyed on id and nothing
else. This is that canonical closer, and its whole point is the `--expect`
assertion: a close must state what it is closing, and refuse if the file
disagrees.

USAGE
    scripts/bug-close.py B-2026-08-11-9 \\
        --expect "comparison-op" \\
        --sha 22ba601 \\
        --fix fix.txt \\
        --append-detail correction.txt

    # check without writing
    scripts/bug-close.py B-2026-08-11-9 --expect "..." --sha ... --fix ... --dry-run

`--fix` / `--append-detail` take a file path, or `-` for stdin. `--status`
defaults to `fixed`; the other closed-without-a-fix values (`wontfix`,
`invalid`, `not-reproduced`) are accepted and are NOT interchangeable — see
CLAUDE.md for which means what.

After a successful write this regenerates `docs/bug-ledger.md`, because a close
that skips the regeneration leaves the rollup lying about the queue.
"""

import argparse
import json
import pathlib
import subprocess
import sys
import typing

ROOT = pathlib.Path(__file__).resolve().parent.parent
LEDGER = ROOT / "docs" / "bug-ledger.jsonl"
ROLLUP = ROOT / "docs" / "bug-ledger.md"

CLOSED = {"fixed", "wontfix", "invalid", "not-reproduced"}


def die(msg: str) -> typing.NoReturn:
    print(f"bug-close: {msg}", file=sys.stderr)
    raise SystemExit(1)


def read_text_arg(val: str | None) -> str | None:
    if val is None:
        return None
    if val == "-":
        return sys.stdin.read()
    p = pathlib.Path(val)
    if not p.is_file():
        die(f"no such file: {val}")
    return p.read_text()


def describe(row: dict) -> str:
    return (
        f"      id:     {row['id']}\n"
        f"      status: {row['status']}\n"
        f"      source: {row['source']}\n"
        f"      class:  {row['class']}   surface: {row['surface']}\n"
        f"      title:  {row['title'][:160]}"
    )


def main() -> None:
    ap = argparse.ArgumentParser(add_help=True)
    ap.add_argument("bid", help="B-ID to close, e.g. B-2026-08-11-9")
    ap.add_argument(
        "--expect",
        required=True,
        action="append",
        help="REQUIRED identity assertion: a substring that must appear in the "
        "row's title or source. Repeatable; ALL must match. This is the guard "
        "— it is what makes a concurrent session's row refuse to be clobbered.",
    )
    ap.add_argument("--sha", required=True, help="fix commit SHA")
    ap.add_argument("--fix", required=True, help="file with the fix prose, or -")
    ap.add_argument("--append-detail", help="file with prose to append to detail, or -")
    ap.add_argument("--title", help="replace the row's title (file path, or -)")
    ap.add_argument("--status", default="fixed", choices=sorted(CLOSED))
    ap.add_argument("--allow-reclose", action="store_true",
                    help="permit closing a row that is already closed (default: refuse)")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    lines = LEDGER.read_text().splitlines(True)
    rows = [(i, json.loads(l)) for i, l in enumerate(lines) if l.strip()]

    matches = [(i, r) for i, r in rows if r.get("id") == args.bid]
    if not matches:
        die(f"{args.bid} not found in {LEDGER.relative_to(ROOT)}")
    if len(matches) > 1:
        die(f"{args.bid} appears {len(matches)} times — run scripts/bug-lint.sh")
    idx, row = matches[0]

    # ---- the guard -------------------------------------------------------
    # Everything below this line is why the script exists. Check the row's
    # IDENTITY before touching it, and report what is actually there so a
    # collision is legible at a glance rather than after the fact.
    haystack = f"{row.get('title', '')}\n{row.get('source', '')}"
    missing = [e for e in args.expect if e not in haystack]
    if missing:
        die(
            f"REFUSING TO WRITE — {args.bid} is not the bug you think it is.\n"
            f"    expected to find: {missing}\n"
            f"    in the row's title or source, but the row on disk is:\n"
            f"{describe(row)}\n"
            f"    If a concurrent session took this id, file yours under a fresh\n"
            f"    id instead of closing this one. Do NOT relax --expect to make\n"
            f"    this pass — that is exactly the clobber this check exists to stop."
        )

    if row.get("status") in CLOSED and not args.allow_reclose:
        die(
            f"REFUSING TO WRITE — {args.bid} is already '{row['status']}'.\n"
            f"{describe(row)}\n"
            f"    Someone else may have closed it. Re-read it first; pass\n"
            f"    --allow-reclose only if you intend to overwrite that close."
        )
    # ---------------------------------------------------------------------

    fix_text = read_text_arg(args.fix).rstrip("\n")
    if args.sha not in fix_text:
        print(
            f"bug-close: note — the fix prose does not mention {args.sha}; "
            f"conventionally it opens 'FIXED by {args.sha}.'",
            file=sys.stderr,
        )

    new = dict(row)
    new["status"] = args.status
    new["fix"] = fix_text
    if args.title:
        new["title"] = read_text_arg(args.title).strip()
    extra = read_text_arg(args.append_detail)
    if extra:
        new["detail"] = new.get("detail", "").rstrip("\n") + "\n" + extra.rstrip("\n")

    if args.dry_run:
        print(f"bug-close: DRY RUN — {args.bid} would become:")
        print(describe(new))
        print(f"      fix:    {fix_text.splitlines()[0][:150] if fix_text else '(empty)'}")
        return

    lines[idx] = json.dumps(new, ensure_ascii=False) + "\n"  # canonical form
    LEDGER.write_text("".join(lines))
    print(f"bug-close: {args.bid} -> {args.status} ({args.sha})")

    r = subprocess.run(
        [sys.executable, str(ROOT / "scripts" / "bug-curve.py"), "--inject", str(ROLLUP)],
        capture_output=True,
        text=True,
    )
    if r.returncode != 0:
        die(f"row written, but rollup regeneration FAILED:\n{r.stderr}")
    print("bug-close: regenerated docs/bug-ledger.md")
    print("bug-close: now run scripts/bug-lint.sh before committing")


if __name__ == "__main__":
    main()
