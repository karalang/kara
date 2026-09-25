#!/usr/bin/env bash
# Self-test for bug-lint.sh rule 6b — the fix-SHA orphan detector.
#
# WHY A SELF-TEST. Rule 6b's failure mode is SILENCE: what it produces when it
# works is a lint that says nothing, which is indistinguishable from a lint that
# checks nothing. Rule 6 sat in exactly that state for months in every cloud
# container (it prints its skip, but nobody reads a WARN), and cell 1 here is
# what showed that rule 6's shallow-clone narrowing STILL misses a live orphan —
# it asks about object presence, which an orphan has. Rule 6b's candidate
# selection is deliberately narrow, so one wrong `-` in a set difference makes it
# go permanently quiet with no other symptom (B-2026-09-16-8).
#
# Runs in a THROWAWAY repo, never against this working copy: it needs to create
# and orphan commits, and a `git reset` in the real tree would eat uncommitted
# work (which it did, once, while this script's own rule was being written).
# Pure git + python, no network, ~2 seconds.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
G=(git -c user.name=selftest -c user.email=selftest@example.invalid -c commit.gpgsign=false -c advice.detachedHead=false)

fail() { echo "SELFTEST FAIL: $*" >&2; exit 1; }

# ---- a minimal upstream repo carrying the scripts and a two-row ledger -----
mkdir -p "$tmp/origin/docs" "$tmp/origin/scripts"
cp "$here/bug-lint.sh" "$here/bug-ledger-normalize.py" "$tmp/origin/scripts/"
python3 - "$tmp/origin/docs/bug-ledger.jsonl" <<'PY'
import json, sys
rows = [
    dict(id="B-2026-01-01-1", date="2026-01-01", source="internal", surface="other",
         **{"class": "other"}, severity="low", status="open", fix="", tracker="",
         title="selftest row — open", detail="selftest"),
    dict(id="B-2026-01-01-2", date="2026-01-01", source="internal", surface="other",
         **{"class": "other"}, severity="low", status="fixed",
         fix="FIXED by deadbee1. a sha that resolves nowhere, and must stay unjudged.",
         tracker="", title="selftest row — closed long ago", detail="selftest"),
]
open(sys.argv[1], "w").write("\n".join(json.dumps(r, ensure_ascii=False) for r in rows) + "\n")
PY
( cd "$tmp/origin" && "${G[@]}" init -q -b main && "${G[@]}" add -A && "${G[@]}" commit -q -m "selftest: seed" )

# ---- a SHALLOW clone of it, which is the environment under test ------------
"${G[@]}" clone -q --depth 1 "file://$tmp/origin" "$tmp/work"
w="$tmp/work"
# Cell 5 backdates its commits to 2026-02-01, and rule 6b tells a rebase orphan
# from a truncation artifact by HEAD's REFLOG. A reflog entry that old is past
# both expiry windows (90 days reachable, 30 unreachable), so any auto gc or
# maintenance a commit triggers can expire it, and the lint then WARNs instead
# of erroring: CI run 36147089592 (git 2.55) failed cell 5 that way while every
# local run passed. Measured: a `reflog expire --all` before cell 5's lint
# reproduces the failure exactly, and this config makes it pass.
"${G[@]}" -C "$w" config gc.auto 0
"${G[@]}" -C "$w" config maintenance.auto false
"${G[@]}" -C "$w" config gc.reflogExpire never
"${G[@]}" -C "$w" config gc.reflogExpireUnreachable never
[ "$("${G[@]}" -C "$w" rev-parse --is-shallow-repository)" = true ] || fail "clone is not shallow"

lint() { ( cd "$w" && KARA_KATAS_DIR=/nonexistent ./scripts/bug-lint.sh 2>&1 ); }
set_fix() {  # $1 = B-ID, $2 = fix prose
    python3 - "$w/docs/bug-ledger.jsonl" "$1" "$2" <<'PY'
import json, sys
path, bid, fix = sys.argv[1:4]
out = []
for line in open(path):
    if line.strip():
        r = json.loads(line)
        if r["id"] == bid:
            r["fix"] = fix
            line = json.dumps(r, ensure_ascii=False) + "\n"
    out.append(line)
open(path, "w").writelines(out)
PY
}

# An orphan, made the way a rebase makes one: a commit that was HEAD and is not
# reachable any more, but is still in the reflog and still has its object.
base="$("${G[@]}" -C "$w" rev-parse HEAD)"
echo probe > "$w/probe.txt"
"${G[@]}" -C "$w" add probe.txt
# The two commits must differ in SOMETHING or git hands back one object: same
# tree, same parent, same message and same one-second timestamp hash to the same
# sha, which is how this probe first "built nothing". The dates are what differ —
# a rebase's twin differs the same way, in its committer date.
GIT_AUTHOR_DATE="2026-01-01T10:00:00+0000" GIT_COMMITTER_DATE="2026-01-01T10:00:00+0000" \
    "${G[@]}" -C "$w" commit -q -m "fix(selftest): the orphaned twin"
orphan="$("${G[@]}" -C "$w" rev-parse --short HEAD)"
"${G[@]}" -C "$w" reset -q --soft "$base"
GIT_AUTHOR_DATE="2026-01-01T11:00:00+0000" GIT_COMMITTER_DATE="2026-01-01T11:00:00+0000" \
    "${G[@]}" -C "$w" commit -q -m "fix(selftest): the orphaned twin"   # same subject, new sha
live="$("${G[@]}" -C "$w" rev-parse --short HEAD)"
[ "$orphan" != "$live" ] || fail "orphan and live sha are identical — the probe built nothing"

# ---- 1. an orphaned sha in a row THIS clone wrote is an ERROR --------------
set_fix B-2026-01-01-1 "FIXED by $orphan. selftest."
out="$(lint)" && fail "rule 6b did not fail the lint on an orphaned fix sha:
$out"
grep -q "reachable from neither HEAD nor origin/main" <<<"$out" \
    || fail "wrong failure — expected the rule-6b message, got:
$out"
# and it must name the post-rebase twin, which is the actionable half
grep -q "same subject: $live" <<<"$out" || fail "rule 6b did not identify the live twin $live:
$out"

# ---- 2. a live sha is silent ----------------------------------------------
set_fix B-2026-01-01-1 "FIXED by $live. selftest."
out="$(lint)" || fail "rule 6b flagged a reachable fix sha:
$out"

# ---- 3. an UNTOUCHED row's unresolvable sha stays unjudged ----------------
# The vacuity guard in the other direction: rule 6b must not put a row it did
# not write on trial. Row 2 cites `deadbee1`, which resolves to nothing, and is
# byte-identical to its origin/main copy — so it is not a candidate at all.
set_fix B-2026-01-01-1 ""
out="$(lint)" || fail "rule 6b flagged an untouched row's unresolvable sha:
$out"
grep -q "deadbee1" <<<"$out" && fail "rule 6b judged an untouched row's sha:
$out"

# ---- 4. the recorded-close path fires with a CLEAN ledger -----------------
# This is the post-push moment: the worktree diff against origin/main is empty,
# so candidate source (i) finds nothing and only the `.git` record can catch it.
# --absolute-git-dir, because a `--git-path` answer is relative to the REPO and
# this redirect runs in the caller's cwd — writing the record into whatever repo
# the selftest was launched from, where it is invisible and wrong.
record="$("${G[@]}" -C "$w" rev-parse --absolute-git-dir)/kara-closed-fix-shas"
printf 'B-2026-01-01-1 %s\n' "$orphan" > "$record"
out="$(lint)" && fail "rule 6b missed the orphan recorded in .git/kara-closed-fix-shas:
$out"
grep -q "B-2026-01-01-1: fix cites $orphan" <<<"$out" || fail "wrong message for the recorded path:
$out"

# ---- 5. an ALL-DIGIT short sha is a sha (B-2026-09-22-12) -----------------
# The headline token rule demanded both a digit and a letter, so a short sha
# that happens to be all digits was not recognised as a sha at all -- 3.8% of
# them, and 47 of the 1380 closed rows on the real ledger, for which rules 6
# and 6b were not lenient but BLIND. Cell 1 draws a fresh sha every run, so it
# met this about once in 26 runs and reported it as a CI flake rather than as
# the standing hole it is. This cell grinds a commit until its short sha has no
# a-f in it, so the case is pinned instead of sampled.
rm -f "$record"
set_fix B-2026-01-01-1 ""
digits=""
for hh in $(seq 0 23); do
    for mm in $(seq 0 59); do
        "${G[@]}" -C "$w" reset -q --soft "$base"
        d="$(printf '2026-02-01T%02d:%02d:00+0000' "$hh" "$mm")"
        GIT_AUTHOR_DATE="$d" GIT_COMMITTER_DATE="$d" \
            "${G[@]}" -C "$w" commit -q -m "fix(selftest): the all-digit twin"
        s="$("${G[@]}" -C "$w" rev-parse --short HEAD)"
        case "$s" in *[a-f]*) ;; *) digits="$s"; break 2 ;; esac
    done
done
[ -n "$digits" ] || fail "could not grind an all-digit short sha in 1440 tries"
# orphan it, the way cell 1 does: it was HEAD, it is not reachable now.
"${G[@]}" -C "$w" reset -q --soft "$base"
GIT_AUTHOR_DATE="2026-03-01T00:00:00+0000" GIT_COMMITTER_DATE="2026-03-01T00:00:00+0000" \
    "${G[@]}" -C "$w" commit -q -m "fix(selftest): the all-digit twin"
set_fix B-2026-01-01-1 "FIXED by $digits. selftest."
out="$(lint)" && fail "rule 6b ignored an ALL-DIGIT orphaned fix sha $digits:
$out"
grep -q "reachable from neither HEAD nor origin/main" <<<"$out" \
    || fail "wrong failure for the all-digit sha $digits — got:
$out"

echo "bug-lint selftest: 5/5 rule-6b cells pass (orphan detected, live sha silent, untouched row unjudged, recorded close caught, all-digit sha not skipped)"
