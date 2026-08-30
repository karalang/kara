#!/usr/bin/env bash
# Integrity gate for docs/bug-ledger.jsonl — makes the B-ID convention ENFORCED
# rather than hoped-for. Run locally or in CI. Exits non-zero on any violation.
#
# Checks:
#   1. every line is valid JSON with the required fields
#   2. B-IDs match B-YYYY-MM-DD-N and are unique
#   3. enum fields (status/severity/surface) are in range
#   4. a `fixed` row carries a fix SHA (warn-only — pre-convention rows may lack one)
#   5. cross-repo (if kara-katas is found): every `kata:N` ledger row is cited by
#      that kata's README, and every B-ID in a kata README exists in the ledger
#   6. every SHA cited in a `fix` field resolves to a commit in this repo
#   7. canonical JSON encoding — see scripts/bug-ledger-normalize.py
set -euo pipefail
cd "$(dirname "$0")/.."
LEDGER="docs/bug-ledger.jsonl"
KATAS="${KARA_KATAS_DIR:-../kara-katas}"

python3 - "$LEDGER" "$KATAS" <<'PY'
import json, re, sys, pathlib, glob
ledger, katas_dir = sys.argv[1], sys.argv[2]
errs, warns = [], []
REQ = ["id","date","source","surface","class","severity","status","fix","title","tracker"]
IDRE = re.compile(r"^B-\d{4}-\d{2}-\d{2}-\d+$")
SURF = {"codegen","typecheck","interp","ownership","effect","lexer","parser","runtime","resolver","cli","autopar","other"}
# Failure-mode class — CONTROLLED vocabulary (canonicalized 2026-07-17). One
# primary class per bug; nuance goes in `detail`, never into new class strings.
CLASS = {"miscompile","double-free","use-after-free","leak","crash","codegen-gap",
         "missing-feature","false-positive","soundness","run-vs-build",
         "diagnostics","perf","other"}
# source = family[:slug] — the family token is a closed set (canonicalized
# 2026-07-17); free-text provenance goes in `detail` as a SOURCE NOTE.
FAM = {"kata","kata-gap","kata-gap-audit","selfhost","dogfood","probe","spike",
       "internal","followup","test-infra","example"}
seen = {}
rows = []
for i, line in enumerate(pathlib.Path(ledger).read_text().splitlines(), 1):
    if not line.strip():
        continue
    try:
        r = json.loads(line)
    except Exception as e:
        errs.append(f"line {i}: invalid JSON ({e})"); continue
    rows.append(r)
    for f in REQ:
        if f not in r:
            errs.append(f"line {i}: missing field '{f}'")
    bid = r.get("id","")
    if not IDRE.match(bid):
        errs.append(f"line {i}: bad B-ID format '{bid}'")
    if bid in seen:
        errs.append(f"line {i}: duplicate B-ID '{bid}' (also line {seen[bid]})")
    seen[bid] = i
    # `open` is the WORK QUEUE — bug-curve.py renders open rows in full
    # precisely because you are expected to act on them. A row that is real and
    # reproduced but has no action left is `wontfix`, NOT `open` (it would sit
    # in the queue forever) and NOT `invalid` (that means the premise was
    # refuted; saying so about a reproducible finding puts a false claim in the
    # ledger). `wontfix` rows render as their own collapsed section, so the
    # measurements that closed the question stay visible without being work.
    #
    # `relocated` is the fourth closed-without-a-fix value and it is NOT
    # `wontfix`: the work is real, wanted, and still scheduled — it simply
    # lives on a canonical tracker now (a `[->]` checklist entry, a roadmap
    # item, a deferred.md tier) because it has no action item today and a
    # concrete external trigger. `wontfix` says "measured to a standstill";
    # `relocated` says "tracked elsewhere, here is where". Collapsing the two
    # loses exactly the information a future reader needs, which is the
    # pointer — so a relocated row MUST carry a non-empty `tracker`.
    if r.get("status") not in {"open","fixed","invalid","not-reproduced","wontfix","relocated"}:
        errs.append(f"{bid}: bad status '{r.get('status')}'")
    if r.get("status") == "relocated" and r.get("tracker","").strip() in ("", "none", "closed"):
        errs.append(
            f"{bid}: status 'relocated' requires a `tracker` naming where the "
            f"work now lives (got '{r.get('tracker','')}')"
        )
    if r.get("severity") not in {"high","medium","low"}:
        errs.append(f"{bid}: bad severity '{r.get('severity')}'")
    # surface: one base value, or a '+'-joined compound of base values
    # (a multi-phase bug counts under each segment in the rollup).
    if not all(seg in SURF for seg in r.get("surface","").split("+")):
        errs.append(f"{bid}: bad surface '{r.get('surface')}'")
    if r.get("class") not in CLASS:
        errs.append(f"{bid}: bad class '{r.get('class')}' (allowed: {sorted(CLASS)})")
    if r.get("source","").split(":")[0] not in FAM:
        errs.append(f"{bid}: bad source family '{r.get('source','').split(':')[0]}' (allowed: {sorted(FAM)})")
    if r.get("status")=="fixed" and not r.get("fix"):
        warns.append(f"{bid}: fixed but no fix SHA")

# cross-repo kata link check
kd = pathlib.Path(katas_dir)
if kd.exists():
    # map kata key -> README path. LeetCode katas key by number
    # (`leetcode/<range>/<N>-slug/`); bespoke katas key by directory name
    # (`bespoke/<slug>/`), matched by `source: "kata:<slug>"`.
    readmes = {}
    for p in glob.glob(str(kd/"leetcode/*/*/README.md")):
        m = re.search(r"/(\d+)-[^/]+/README\.md$", p)
        if m:
            readmes[m.group(1)] = pathlib.Path(p)
    for p in glob.glob(str(kd/"bespoke/*/README.md")):
        m = re.search(r"/([^/]+)/README\.md$", p)
        if m:
            readmes[m.group(1)] = pathlib.Path(p)
    # A leetcode slug resolves by NUMBER, but ledger rows spell the source
    # four ways — bare `kata:147`, slug `kata:147-insertion-sort-list`,
    # slug-plus-parenthetical `kata:77-combinations (bitmask, k==0)`, and
    # `kata:leetcode-95-unique-bst-ii`. Keying only on the exact string
    # silently demoted every non-bare form to a warning, so the "README must
    # cite the B-ID" check never ran on them (27 rows, 8 of them real
    # violations). Resolve on the leading number — with an optional
    # `leetcode-` prefix — before giving up; a source with no leading number
    # (bespoke slug, or free-text provenance that belongs in `detail`) still
    # warns.
    def resolve_kata(key):
        rp = readmes.get(key)
        if rp:
            return rp
        m = re.match(r"^(?:leetcode-)?(\d+)\b", key)
        return readmes.get(m.group(1)) if m else None

    ledger_bids = set(seen)
    for r in rows:
        src = r.get("source","")
        if src.startswith("kata:"):
            # A withdrawn row has no claim on the kata's README: `invalid` /
            # `not-reproduced` mean the kata did NOT surface a compiler bug,
            # so there is nothing for the README to cite and requiring a
            # citation would push a false claim into the kata corpus.
            if r.get("status") in {"invalid", "not-reproduced"}:
                continue
            num = src.split(":", 1)[1]
            rp = resolve_kata(num)
            if not rp:
                warns.append(f"{r['id']}: source {src} but no README found for kata {num}")
            elif r["id"] not in rp.read_text():
                errs.append(f"{r['id']}: source {src} but kata {num} README does not cite the B-ID")
    # reverse: B-IDs cited in kata READMEs must exist in the ledger
    for num, rp in readmes.items():
        for bid in set(re.findall(r"B-\d{4}-\d{2}-\d{2}-\d+", rp.read_text())):
            if bid not in ledger_bids:
                errs.append(f"kata {num} README cites {bid} which is not in the ledger")
else:
    warns.append(f"kata repo not found at {katas_dir} (set KARA_KATAS_DIR) — skipped cross-repo link check")

# ── 6. fix-SHA resolvability ─────────────────────────────────────────────
# A `fix` field's whole job is to point at the commit that fixed the bug. A
# citation that resolves to nothing is worse than an empty field: it reads as
# an answer and silently is not one, and the ledger is the project's memory —
# `git show <sha>` is how anyone re-derives WHY a fix looks the way it does.
#
# These go dangling by an ordinary, blameless route: the local dev flow commits
# inside a worktree, records the SHA, then rebases before the fast-forward, so
# the recorded SHA is the PRE-rebase one and dies with the old commit. Nothing
# ever noticed, because nothing ever looked.
#
# Token rule: 7-40 hex containing BOTH a digit and a letter. The digit
# requirement drops ordinary words that happen to be all-hex ("defaced",
# "effaced"); the letter requirement drops line numbers, counts and dates
# ("1809 allocations"). It costs a real SHA only when all 8 chars land in a-f,
# which is ~0.04% of commits — and the cost there is a skipped check, never a
# false accusation.
def _git(*a, inp=None):
    import subprocess
    return subprocess.run(["git", *a], input=inp, capture_output=True, text=True)

SHA = re.compile(r"\b(?=[0-9a-f]*\d)(?=[0-9a-f]*[a-f])[0-9a-f]{7,40}\b")

# Rows whose dangling SHA could not be recovered by the 2026-08-11 repair
# sweep. Two shapes: the fix commit's message never cited the B-ID (so there is
# nothing to match on), or the row cites several SHAs and which one died is
# ambiguous. THIS LIST EXISTS TO SHRINK — if you can identify the real commit
# for one of these, fix the row and delete the entry. Do NOT add to it to make
# a new row pass; a fresh dangling SHA means the SHA is wrong, and the fix is
# to correct it while you still remember what it was.
DANGLING_GRANDFATHERED = {
    "B-2026-06-12-6", "B-2026-06-19-14", "B-2026-06-20-7", "B-2026-06-20-8",
    "B-2026-06-20-9", "B-2026-06-20-10", "B-2026-06-20-11", "B-2026-06-20-12",
    "B-2026-06-20-13", "B-2026-06-20-18", "B-2026-06-30-10", "B-2026-07-12-1",
    "B-2026-07-12-2", "B-2026-07-12-31", "B-2026-07-16-20", "B-2026-07-16-21",
    "B-2026-07-17-16", "B-2026-07-18-13", "B-2026-07-23-15", "B-2026-07-23-17",
    "B-2026-07-23-18", "B-2026-07-23-21", "B-2026-07-23-23", "B-2026-07-23-26",
    "B-2026-07-28-5", "B-2026-07-29-9", "B-2026-07-29-11", "B-2026-08-03-3",
    "B-2026-08-03-10", "B-2026-08-04-11", "B-2026-08-05-7", "B-2026-08-05-34",
    "B-2026-08-05-38", "B-2026-08-06-9", "B-2026-08-06-31", "B-2026-08-07-13",
    "B-2026-08-07-20", "B-2026-08-07-25", "B-2026-08-08-5", "B-2026-08-08-6",
    "B-2026-08-08-17", "B-2026-08-08-19", "B-2026-08-09-10", "B-2026-08-09-12",
    "B-2026-08-09-21", "B-2026-08-10-3", "B-2026-08-10-21",
}

# A SHALLOW clone has almost no history, so every SHA would look dangling and
# the check would report ~900 false violations. Skip loudly rather than lie —
# and note that `actions/checkout` is depth-1 BY DEFAULT, which is why the CI
# job that runs this sets `fetch-depth: 0`.
if _git("rev-parse", "--git-dir").returncode != 0:
    warns.append("not a git repository — skipped fix-SHA resolvability check")
elif _git("rev-parse", "--is-shallow-repository").stdout.strip() == "true":
    warns.append(
        "shallow clone — skipped fix-SHA resolvability check "
        "(needs full history; set `fetch-depth: 0` on actions/checkout)"
    )
else:
    # A `fix` field may reference a commit in the SIBLING kara-katas repo (an
    # `audit`/`kata-gap` source often does), written `kara-katas <sha>`. That
    # sha legitimately does not resolve HERE, and rule 6 is a same-repo check —
    # flagging it is the "false accusation" the token rule above is written to
    # avoid. Mask those references out before extracting, so only THIS repo's
    # fix SHAs are validated. The mask is narrow: it drops a sha ONLY when
    # immediately preceded by `kara-katas` (with an optional `commit`/`repo`),
    # so it can never hide a genuine dangling same-repo SHA.
    KATAS_SHA = re.compile(
        r"kara-katas\s+(?:commit\s+|repo\s+)?"
        r"(?=[0-9a-f]*\d)(?=[0-9a-f]*[a-f])[0-9a-f]{7,40}\b"
    )
    def _repo_shas(fix):
        return set(SHA.findall(KATAS_SHA.sub("kara-katas", fix or "")))

    # HEADLINE sha vs sha merely MENTIONED in the prose. What this rule
    # protects is traceability: a closed row must land you on its commit via
    # `git show`, and that is the sha in the opening `FIXED by <sha>.` clause —
    # the one `bug-close.py` requires and the one a reader reaches for first.
    #
    # A sha further down the prose is a REFERENCE, and a dangling one there is
    # frequently DELIBERATE: the row is recording its own history ("this note
    # first cited `a6f6572` … that sha is orphaned; the live commit is
    # `d135d02`" — B-2026-08-29-48). Erroring on that punishes a row for being
    # honest about a rebase, and the only way to silence it is to delete the
    # record, which is the opposite of what the ledger is for. So: headline
    # dangling is an ERROR, prose dangling is one aggregated WARN.
    #
    # A row whose fix does NOT open with the convention (about a thousand
    # legacy rows do not) has no distinguishable headline, so every sha in it
    # is treated as headline-class — the pre-2026-08-30 behaviour, which is
    # what DANGLING_GRANDFATHERED is calibrated against.
    OPENER = re.compile(r"(?i)^\s*(?:fixed|fix|closed|resolved)\s+(?:by|in|at|via|with)\b")
    def _split_shas(fix):
        masked = KATAS_SHA.sub("kara-katas", fix or "")
        allshas = set(SHA.findall(masked))
        if not OPENER.match(masked):
            return allshas, set()
        # First sentence only: a period that ends a sentence is followed by
        # whitespace or end-of-string, which leaves `docs/foo.md` intact.
        head = re.split(r"\.(?=\s|$)", masked, maxsplit=1)[0]
        headline = set(SHA.findall(head))
        return headline, allshas - headline

    split = {r["id"]: _split_shas(r.get("fix", "")) for r in rows}
    split = {k: v for k, v in split.items() if v[0] or v[1]}
    cites = {k: (v[0] | v[1]) for k, v in split.items()}
    every = sorted({t for v in cites.values() for t in v})
    if every:
        # One batch call, not one per token — ~940 tokens would otherwise be
        # ~940 process spawns and turn a millisecond gate into half a minute.
        # `--batch-check` echoes the RESOLVED sha for a hit and `<input>
        # missing` for a miss, so only the miss lines carry the input token.
        probe = _git("cat-file", "--batch-check",
                     inp="\n".join(t + "^{commit}" for t in every)).stdout
        gone = {l.split("^{commit}")[0] for l in probe.splitlines()
                if l.rstrip().endswith(("missing", "ambiguous"))}
        stale = 0
        mentioned = []
        for bid, (headline, prose) in sorted(split.items()):
            dead_head = sorted(headline & gone)
            dead_prose = sorted(prose & gone)
            if not dead_head and not dead_prose:
                continue
            if bid in DANGLING_GRANDFATHERED:
                stale += 1
            elif dead_head:
                errs.append(
                    f"{bid}: fix cites {', '.join(dead_head)}, which resolve(s) to no commit "
                    f"in this repo (pre-rebase SHA?)")
            else:
                mentioned.append(f"{bid} ({', '.join(dead_prose)})")
        if mentioned:
            warns.append(
                f"{len(mentioned)} row(s) MENTION a sha that resolves to no commit, outside the "
                f"`FIXED by <sha>.` opener — check each is a deliberately recorded orphan and not "
                f"a typo: {'; '.join(mentioned)}")
        if stale:
            warns.append(
                f"{stale} grandfathered row(s) still cite a dangling fix SHA — "
                "see DANGLING_GRANDFATHERED in scripts/bug-lint.sh; the list exists to shrink"
            )
        for bid in sorted(DANGLING_GRANDFATHERED - {b for b in cites if cites[b] & gone}):
            warns.append(f"{bid}: grandfathered as dangling but now resolves — "
                         "remove it from DANGLING_GRANDFATHERED")

for w in warns: print(f"WARN  {w}")
for e in errs: print(f"ERROR {e}")
print(f"\n{len(rows)} ledger rows · {len(errs)} errors · {len(warns)} warnings")
sys.exit(1 if errs else 0)
PY

# 6. Canonical JSON encoding (B-2026-08-07-13). The ledger has no canonical
#    writer — every lane appends with its own script — and `json.dumps`
#    ASCII-escapes by DEFAULT while `ensure_ascii=False` does not. Both forms
#    round-trip losslessly, so nothing noticed, and the file flipped between
#    them four times on 2026-08-07 alone, each flip rewriting ~850 of ~1000
#    rows and burying the one-line change in an 850-line diff.
#
#    Runs LAST, and `set -e` means a content failure above aborts before we get
#    here — encoding is the least interesting thing to be told about when a row
#    is actually malformed.
python3 scripts/bug-ledger-normalize.py --check
