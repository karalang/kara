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
    if r.get("status") not in {"open","fixed","invalid","not-reproduced"}:
        errs.append(f"{bid}: bad status '{r.get('status')}'")
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
    ledger_bids = set(seen)
    for r in rows:
        src = r.get("source","")
        if src.startswith("kata:"):
            num = src.split(":")[1]
            rp = readmes.get(num)
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

for w in warns: print(f"WARN  {w}")
for e in errs: print(f"ERROR {e}")
print(f"\n{len(rows)} ledger rows · {len(errs)} errors · {len(warns)} warnings")
sys.exit(1 if errs else 0)
PY
