# Bug ledger — the standard

`docs/bug-ledger.jsonl` is the **single, committed, machine-countable record of
every bug (or missing primitive) surfaced in `karac`.** It exists so one
question — *"are we still finding bugs, and where?"* — becomes a number you can
watch flatten, sliced by surface (codegen/ownership/…) and by source
(kata/selfhost/dogfood/internal). Flattening of the kata + dogfood slices is a
v1 launch gate; you cannot see it without consistent capture.

Before this ledger, bug records were scattered across phase trackers (by B-ID),
test comments, commit messages, and kata READMEs (by bare commit SHA), with the
`B-YYYY-MM-DD-N` convention followed only ~1 in 4 times — so the corpus was
**not countable**. This file is the fix: prose stays in the trackers/READMEs;
they just **reference the B-ID**, and the ledger is the index.

## The rule (lightweight, enforced)

1. **Every bug surfaced → one appended JSONL line**, keyed by a `B-YYYY-MM-DD-N`
   ID (the day it was surfaced; `N` = that day's sequence). Minting a B-ID is the
   first step of triaging any bug — the same moment it lands in a tracker.
2. **The detailed prose lives in its owning phase tracker** (or kata README);
   the ledger row carries only the countable fields + a `tracker` pointer.
3. **A kata that surfaces a bug cites its B-ID(s)** in its README (not a bare
   SHA). `selfhost`/`dogfood` bugs cite the B-ID in their spike/tracker entry.
4. `scripts/bug-lint.sh` enforces 1–3 (B-ID format + uniqueness, enum ranges,
   and the cross-repo kata↔ledger link). Run it in CI.

## Schema (one JSON object per line, fields in this order)

| field | values | notes |
|---|---|---|
| `id` | `B-YYYY-MM-DD-N` | primary key, unique |
| `date` | `YYYY-MM-DD` | surfaced date = the curve's x-axis |
| `source` | `kata:<num>` · `selfhost:<comp>` · `dogfood:<name>` · `internal` | who/what surfaced it |
| `surface` | codegen · typecheck · interp · ownership · effect · lexer · parser · runtime · resolver · cli · autopar · other | which compiler phase the defect was in |
| `class` | `unported` · `shared-logic` · `port-mistake` · `""` | port-triage taxonomy (phase-12 §); empty unless it's a self-hosting port bug |
| `severity` | high · med · low | `high` = soundness / miscompile / bootstrap-critical |
| `status` | open · fixed | |
| `fix` | commit SHA · `""` | the landing commit |
| `title` | one line, ≤110 chars | |
| `tracker` | `<file>#anchor` or `kata:<n>-README` | where the prose lives |

## Tooling

```bash
python3 scripts/bug-curve.py                 # markdown report → stdout
python3 scripts/bug-curve.py --svg docs/bug-curve.svg   # + cumulative-curve SVG
KARA_KATAS_DIR=../kara-katas ./scripts/bug-lint.sh      # integrity gate (CI)
```

## Reading the curve honestly

The historical rows are a **best-effort backfill** (2026-05 → 2026-06-13), and
the early slope reflects **when consistent record-keeping started, not the true
bug rate** — the `B-ID` convention only began ~2026-06-07, and the late-June
spike is the self-hosting + shared-enum-drop push, not a regression. The ledger
becomes a *true* signal **going forward**, where every bug is one append at
triage time. That's the whole reason for the standard: without it you can't
distinguish "bugs flattening" from "we stopped writing them down."

## Known backfill debt (does not block the curve)

- **`class` is empty** on all rows — the port-triage taxonomy was applied
  unreliably by the initial extraction, so it was blanked. Fill per-bug from the
  owning phase-12 triage when touched.
- **34 rows lack a `fix` SHA** — the trackers recorded the fix in prose, not a
  greppable SHA. `bug-lint.sh` warns (not errors) on these; backfill from
  `git log` opportunistically.
- **Pre-convention SHA-only bugs** (e.g. some early kata gaps) may still be
  uncaptured. Add them when found; don't trust the May/early-June counts as
  complete.
