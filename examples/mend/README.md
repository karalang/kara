# Mend

A worked example of Kāra's AI-first thesis: an LLM writes Kāra code,
the compiler returns structured diagnostics + machine-applicable fix
diffs, the LLM applies them mechanically, and the loop converges to a
clean build in a few iterations.

## What it demonstrates

The thesis: *Kāra is a language designed to be written by AI.* The
demonstration:

```
User prompt → LLM writes Kāra
    → karac check --output=json
    → LLM reads structured diagnostics + machine-applicable fixes
    → karac fix applies precise text replacements
    → LLM patches whatever's left descriptively
    → repeat until clean build
```

The structured-output surface used by the loop is the same one shipped
in Phase 5:

| Surface                      | Role in the loop                                   |
|------------------------------|----------------------------------------------------|
| `karac check --output=json`  | Per-diagnostic JSON envelope: span, severity, code, message, optional `replacement: { offset, length, text }` |
| `karac fix`                  | Mechanical application of every diagnostic that carries a `replacement` (resolver `did you mean`, ownership prefix rewrites). |
| `karac query effects`        | Inferred effect set per function — the LLM uses this to fix under-declared `with` clauses. |
| `karac query ownership`      | Inferred parameter modes — the LLM uses this to fix `ref` / `mut ref` annotations. |

No language change is needed for Mend. The compiler's existing
machine-readable surface IS the demo; Mend just exercises it under
realistic LLM use to surface friction.

## The before/after

The demo's punchline isn't the round trip itself — it's the contrast
with Python. The same task, written in Python by the same LLM, produces
code that *looks correct*, that mypy and pyright accept, and that has
a subtle concurrency bug — typically a race on shared mutable state.

Python's tools cannot catch the bug statically. Kāra's effect checker
and ownership checker do. Each example pair under `examples/` ships
both versions side by side.

## Layout

```
examples/mend/
├── README.md                  (this file)
├── harness/                   the loop driver
│   ├── mend.py                Python driver — dry-run + live (claude -p) modes
│   ├── system_prompt.md       primer the LLM is given before each run
│   └── README.md              how to run the harness
├── examples/                  example pairs — the corpus
│   ├── welcome_emails/        ownership: use-after-move on Vec
│   ├── order_status/          pattern exhaustiveness on enum match
│   ├── user_lookup/           type mismatch: Option<T> from Map.get
│   └── concurrent_emails/     effect: shared-counter race in a par { } block
├── casts/                     asciinema recordings (recordable artifact)
│   ├── demo.sh                narrated wrapper for recording
│   ├── welcome_emails.cast
│   └── order_status.cast
└── runs/                      harness output (per-iteration logs, gitignored)
```

Each example directory contains:

| File                    | Role                                                                |
|-------------------------|---------------------------------------------------------------------|
| `task.md`               | Natural-language prompt fed to the LLM.                             |
| `solution.kara`         | Reference Kāra solution that compiles clean.                        |
| `python_buggy.py`       | Same task in Python — looks correct, has a runtime bug.             |
| `go_buggy.go`           | Same task in Go (`concurrent_emails` only) — compiles + vets clean, races at runtime. |
| `canned_responses.json` | LLM responses for `--dry-run` (deterministic offline replay).       |
| `notes.md`              | What Python misses, what Kāra catches.                              |

## The corpus

| Example          | Compiler shape                  | Bug class                                     | Diagnostic         |
|------------------|---------------------------------|-----------------------------------------------|--------------------|
| `welcome_emails` | Ownership                       | Use-after-move on `Vec` consumed by a `for` loop | `E0500` (descriptive) |
| `order_status`   | Pattern exhaustiveness          | Missing variant in `match` over enum          | `E0205` (descriptive) |
| `user_lookup`    | Type mismatch                   | Returning `Option<String>` where `String` was declared (forgot to handle the None case from `Map.get`) | `E0200` (descriptive) |
| `concurrent_emails` | Effect / `par`-conflict      | Shared-counter data race written from inside a `par { }` block | `E0408` (descriptive) |

Each example targets a distinct *compiler axis* — the same loop
machinery exercises ownership, exhaustiveness, type checking, and the
effect checker's `par`-conflict path without needing different harness
code.

## Current state

**Slice 0 — scaffolding.** Directory layout, harness skeleton, first
example pair (welcome_emails), `--dry-run` mode against canned
responses.

**Slice 1 — live mode.** Live LLM iteration via `claude -p` (Claude
Code's non-interactive mode). Auth inherited from the user's existing
Claude Code login (Max subscription) — no separate API key, no
incremental cost. End-to-end verified on welcome_emails: LLM made a
use-after-move error on iter 0, compiler caught it, LLM restructured
on iter 1, build clean.

**Slice 2 — corpus expansion (this commit).** Two additional example
pairs (`order_status`, `user_lookup`), covering pattern exhaustiveness
and Option-handling. System prompt tightened: teaches Kāra-specific
syntax (effect annotations, generic brackets, Vec/Map construction)
and explicitly tells the LLM to let the compiler teach the *semantic*
rules (exhaustiveness, ownership, Option/Result discipline). Live
verification:
  - welcome_emails: 2 iters (ownership)
  - order_status: 2 iters (qualified-path patterns → bare names)
  - user_lookup: 0 iters under Claude (the model already knows
    `Option<T>` discipline from Rust priors; dry-run forces a buggy
    v0 to demonstrate the loop)

**Slice 3 — recordable artifact (this commit).** asciinema casts of
the harness running on `welcome_emails` and `order_status` under live
Claude. Stored as portable asciicast-v3 files in `casts/`. The
narrated wrapper (`casts/demo.sh`) walks each run through the task
prompt, the LLM's per-iteration response, the compiler diagnostics,
and the converged final source. `user_lookup` is excluded — Claude
already knows `Option<T>` discipline from its Rust priors and that
example consistently converges on iter 0, which makes for documentary
evidence of working infrastructure but not a compelling cast. See
`casts/README.md` for playback / re-recording instructions.

**Slice 4 — concurrency axis (`concurrent_emails`).** A fourth example
pair exercising the effect checker's `par`-conflict path: the LLM
increments a shared module-level counter from inside a `par { }`
block, the compiler rejects it with `E0408`, and the LLM lifts the
counter into an `Atomic[i64]` (`par struct`) — clean build, parallelism
preserved. Both canned iterations and `solution.kara` are verified
against the in-tree `karac` (iter 0 → `E0408` only; iter 1 → clean).
Ships a Go comparator (`go_buggy.go`) alongside the Python one: both
incumbents compile-and-ship the race (Go's caught only by the opt-in
runtime `-race` detector). This is a Python/Go-beating case, **not** a
Rust-beating one — Rust catches the same class — and `notes.md` is
explicit about it. Live `claude -p` run + cast: pending.

## See also

- [`docs/demo_ideas.md § Demo 2`](../../docs/demo_ideas.md) — the
  demo's design storyboard.
- [`examples/parallax/`](../parallax/) — sister demo (auto-concurrency).
  Parallax + Mend together are the minimum viable showcase.
