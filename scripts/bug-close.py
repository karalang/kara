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
`invalid`, `not-reproduced`, `relocated`) are accepted and are NOT
interchangeable — see CLAUDE.md for which means what. `relocated` additionally
requires the row to carry a `tracker` naming where the work went; bug-lint.sh
enforces that, because a relocation whose pointer is missing is just a
disappearance.

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

CLOSED = {"fixed", "wontfix", "invalid", "not-reproduced", "relocated"}


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


def git(*a: str) -> subprocess.CompletedProcess:
    return subprocess.run(["git", "-C", str(ROOT), *a], capture_output=True, text=True)


def check_sha_live(sha: str, allow: bool) -> None:
    """Refuse a fix SHA that a rebase has ALREADY orphaned.

    A fix SHA is volatile until it is pushed: the workflow rebases onto
    `origin/main` before every push, which rewrites the very commit the row is
    about to cite. Recording it from memory — or closing after a rebase with the
    pre-rebase sha in hand — writes a row that resolves fine here and nowhere
    else. Eight rows carry a `SHA NOTE` about exactly that (B-2026-09-16-8), and
    `bug-lint.sh` rule 6 cannot catch it: it asks whether the sha RESOLVES, and
    an orphan keeps its object in the clone that made it, so it does.

    Reachable from `HEAD` or `origin/main` is the test, and it needs no history:
    a live commit this session made is a few steps down from HEAD, while an
    orphan is reachable from neither and survives only in the reflog. An
    UNRESOLVABLE sha is a different case and only a note — on a shallow clone it
    is usually a commit from before the graph was truncated, and `--sha` may
    legitimately name a commit in the sibling kara-katas repo.
    """
    if git("rev-parse", "--git-dir").returncode != 0:
        return
    if git("cat-file", "-e", sha + "^{commit}").returncode != 0:
        print(
            f"bug-close: NOTE — {sha} resolves to no commit in this clone, so it could "
            f"not be checked.\n"
            f"  On a shallow clone that is expected for an older commit; if {sha} is a "
            f"sibling-repo\n"
            f"  commit, write it in the prose as `kara-katas {sha}` so the lint masks it.",
            file=sys.stderr,
        )
        return
    for ref in ("HEAD", "origin/main"):
        if git("merge-base", "--is-ancestor", sha, ref).returncode == 0:
            return
    subj = git("log", "-1", "--format=%s", sha).stdout.strip()
    twin = git("log", "-n", "1", "--format=%h", "--fixed-strings",
               f"--grep={subj}", "HEAD").stdout.split() if subj else []
    hint = (f"\n    A commit in HEAD carries the same subject: {twin[0]} — that is almost\n"
            f"    certainly the post-rebase twin. Cite that one." if twin else "")
    if allow:
        print(f"bug-close: NOTE — {sha} is unreachable from HEAD and origin/main; "
              f"writing anyway (--allow-unreachable-sha).{hint}", file=sys.stderr)
        return
    sys.exit(
        f"bug-close: REFUSING TO WRITE — {sha} is reachable from neither HEAD nor\n"
        f"    origin/main, so a rebase has already orphaned it. A row citing it points\n"
        f"    at a commit that exists only in this container.{hint}\n"
        f"    Re-read the sha from `git log` and re-run. Pass --allow-unreachable-sha\n"
        f"    only if you know the commit will be pushed under this sha."
    )


def record_close(bid: str, sha: str) -> None:
    """Leave the (row, sha) pair where `bug-lint.sh` rule 6b can re-check it.

    The file lives in `.git`, outside the worktree, so it survives the push —
    which is what keeps the check meaningful when the rebase is triggered BY the
    push (`git push` -> non-fast-forward -> fetch, rebase, retry, all inside one
    command). In that shape "close after the final rebase" and "before the push"
    are the same instant, so a pre-push lint cannot see the orphan and only a
    later run can.
    """
    path = git("rev-parse", "--git-path", "kara-closed-fix-shas").stdout.strip()
    if not path:
        return
    try:
        with open(path, "a") as f:
            f.write(f"{bid} {sha}\n")
    except OSError:
        pass


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
    ap.add_argument("--tracker",
                    help="set the row's `tracker` field — where the work now "
                         "lives. REQUIRED by --status relocated (a relocation "
                         "whose pointer is missing is just a disappearance).")
    ap.add_argument("--allow-unreachable-sha", action="store_true",
                    help="permit a fix SHA that is reachable from neither HEAD "
                         "nor origin/main (default: refuse — a rebase orphaned it)")
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
        # HARD failure, not a note. This was advisory, printed to stderr, and
        # was missed twice in one session — both times because the prose
        # already opened with the words "FIXED by" (just followed by an
        # explanation rather than the SHA), which reads correct at a glance.
        # The result is a `fixed` row whose commit is unrecoverable from the
        # ledger, and no lint catches it: `bug-lint`'s "fixed but no fix SHA"
        # check did not fire on either row. Same reasoning as `--expect` —
        # a guard that only warns is a guard that gets skipped.
        sys.exit(
            f"bug-close: the fix prose does not mention {args.sha}.\n"
            f"  Ledger convention is for it to OPEN with 'FIXED by {args.sha}.'\n"
            f"  so a closed row stays traceable to the commit that closed it.\n"
            f"  Add the SHA to the prose and re-run — do not work around this by\n"
            f"  passing a SHA that happens to appear in the text."
        )

    check_sha_live(args.sha, args.allow_unreachable_sha)

    new = dict(row)
    new["status"] = args.status
    if args.tracker:
        new["tracker"] = args.tracker.strip()
    if new["status"] == "relocated" and new.get("tracker", "").strip() in ("", "none", "closed"):
        sys.exit(
            "bug-close: --status relocated requires a tracker naming where the "
            "work now lives.\n"
            "  Pass --tracker <path-or-anchor>. `relocated` means TRACKED "
            "ELSEWHERE, and\n"
            "  without the pointer the row says only that the work vanished.\n"
            "  If nothing tracks it, the honest status is wontfix, not relocated."
        )
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
    record_close(args.bid, args.sha)

    r = subprocess.run(
        [sys.executable, str(ROOT / "scripts" / "bug-curve.py"), "--inject", str(ROLLUP)],
        capture_output=True,
        text=True,
    )
    if r.returncode != 0:
        die(f"row written, but rollup regeneration FAILED:\n{r.stderr}")
    print("bug-close: regenerated docs/bug-ledger.md")
    print("bug-close: now run scripts/bug-lint.sh before committing")
    if not git("config", "core.hooksPath").stdout.strip():
        # `git clone` does not set core.hooksPath, so the committed pre-push
        # hook — whose whole job is to run this lint before a push — is INERT in
        # a fresh clone, which is every cloud container. Say so once, here,
        # because this is the moment a fix SHA starts being able to go stale.
        print("bug-close: NOTE — core.hooksPath is unset, so hooks/pre-push is not "
              "active in this clone; run scripts/install-hooks.sh to arm it.")


if __name__ == "__main__":
    main()
