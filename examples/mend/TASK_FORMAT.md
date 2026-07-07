# Mend task + oracle format

Mend measures the AI-first wedge: when an LLM writes Kāra, what fraction of the
mistakes it makes does the compiler resolve *for it*? This doc defines how any
unit of Kāra work — a kata, a dogfooding function, a self-hosting module —
becomes a **Mend task** with a correctness **oracle**, so the whole corpus can
be run through the loop and scored uniformly.

## The loop

```
task prompt → LLM writes Kāra → karac check --output=json
  → apply karac fix for machine-applicable diagnostics
  → feed remaining diagnostics back → repeat until clean
  → run the ORACLE: is the result correct, not just compiling?
```

`harness/mend.py` (one task) and `harness/mend_batch.py` (the corpus) drive
everything up to "clean build"; `harness/mend_score.py` scores the transcripts.
The **oracle** adds the final correctness gate on top.

## A task is a directory

`examples/mend/examples/<name>/`:

| File | Required | Role |
|---|---|---|
| `task.md` | yes | The natural-language spec — the "write X" prompt. |
| an oracle | yes | How to check the RESULT is correct (see below). |
| `context/` | optional | Interfaces / surrounding code the LLM needs for a non-self-contained unit. |
| `canned_responses.json` | dry-run only | Canned LLM replies for deterministic `--dry-run`. |
| `notes.md` | optional | What the mistake is, and why it is (or isn't) machine-fixable. |

## Oracles

Pick the one that fits the unit. All answer *"is it correct,"* not *"does it
compile":*

- **`expected.txt`** — build + run, diff stdout. Katas and examples with
  deterministic output.
- **`cases.json`** — `[{"input": …, "expected": …}]`, driven by a small
  harness. Functions with enumerable behavior.
- **`solution.kara`** — a known-correct reference; run both, diff behavior.
  (How the current examples already work.)
- **fixpoint** — self-hosting: the ported unit's output must match the
  bootstrap compiler on the same input.

## Scored outcomes

The oracle enriches the outcome beyond "who closed the loop":

| outcome | meaning |
|---|---|
| `clean-on-arrival` + oracle pass | LLM nailed it unaided (ergonomics, not the loop) |
| `fixed-by-karac` + oracle **pass** | **the strong wedge result** — the compiler resolved it *and* it's correct |
| `fixed-by-karac` + oracle **FAIL** | the fix compiled but changed behavior — a bug a compile-only oracle misses |
| `fixed-by-llm` + oracle pass/fail | diagnostic quality (the LLM re-reasoned from prose) |
| non-converged | hit the iteration cap |

`fixed-by-karac + oracle FAIL` is the category worth hunting: it catches a
machine fix (or an LLM patch) that makes the program build but wrong.

## Granularity rule

The unit must be **writable from its prompt + `context/` as one function or
file**:

- A **kata** → the whole program (self-contained: `task.md` is the problem
  statement, `expected.txt` the sample output).
- A **dogfood / self-host function** → one function, with its signature and the
  interfaces it calls supplied in `context/`.
- **Never** "port the whole typechecker" — a fresh LLM won't produce it and you
  would measure noise, not the wedge.

## Practice vs. measurement — the honesty rule

Two modes, kept strictly apart:

- **Practice (do always).** Author and verify *any* new Kāra through the loop:
  `karac fix` is the primary fix path, then the oracle. This continuously
  dogfoods the wedge and surfaces diagnostic/fix gaps. Every gap is a backlog
  item — fix the compiler or open a `docs/bug-ledger.jsonl` entry, never route
  around it.
- **Measurement (the pitch number).** The machine-fix **rate** is a statistic
  only over **fresh, blind** LLM authorship — a model instance that never saw
  the diagnostics, writing from `task.md` alone (`mend_batch.py`, live).
  Authoring by someone who already knows the language is biased (they won't make
  the known mistakes); it counts as dogfooding + gap-finding, **never** as the
  rate. Do not quote a machine-fix rate from non-blind authoring.

Live mode needs an authenticated `claude` CLI (it 401s in headless/CI), so the
measurement runs in a developer environment on a periodic cadence, not in CI.

## Turning the existing axes into Mend tasks

Mend is a measurement *layer over* the existing axes — it does not replace them
(katas still find interaction bugs, self-hosting still has its fixpoint oracle,
dogfooding still surfaces ergonomics):

- **Katas** — already a task+oracle: problem statement → `task.md`, expected
  output → `expected.txt`.
- **Dogfooding functions** — `task.md` = the function's spec; `context/` = the
  module's other signatures; oracle = the project's tests.
- **Self-hosting** — feed function/module-by-module; `context/` = the interfaces
  the unit depends on; oracle = the bootstrap fixpoint.
