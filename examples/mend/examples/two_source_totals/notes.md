# two_source_totals — a machine-fixable LLM mistake

## The mistake

The task needs a `pub fn` that reads from two resources, so its signature
declares two effects. The natural, correct Kāra form is
**space-separated**:

```kara
pub fn total() -> i64 with reads(UserDB) reads(OrderDB) { ... }
```

An LLM almost always writes it **comma-separated** instead:

```kara
pub fn total() -> i64 with reads(UserDB), reads(OrderDB) { ... }
```

This isn't a knowledge gap it can reason away — it's a structural habit.
Every language the model trained on (Rust attributes, Python decorators,
C params, Go, TypeScript) comma-separates lists, so "a list of things"
maps to commas by reflex.

## Why it's the machine-fixable class, not the reasoning class

`karac check --output=json` reports the stray comma as `E0001` with a
focused message and a machine-applicable `replacement` that deletes the
comma byte:

```
[E0001] parse: effect items are space-separated, not comma-separated; remove the `,`
    replacement: {offset: …, length: 1, text: ""}
```

`karac fix` applies it verbatim — no LLM reasoning, no guessing — and the
build goes clean in one pass. In the Mend loop this is the
`clean-after-karac-fix` outcome, which `mend_score.py` counts toward the
**machine-fix rate** (the compiler's fix machinery closed the loop),
distinct from `fixed-by-llm` (the model re-reasoned from prose).

## Contrast with the other examples

- `welcome_emails` — resolver-level typos (`count_user` → `count_users`),
  the other machine-fixable class.
- `concurrent_emails` — a shared-mutable-in-`par` race (`E0408`) whose fix
  is a *structural refactor* with a three-way choice; correctly
  descriptive (`fixed-by-llm`), not machine-applicable.

This example sits with `welcome_emails` on the machine-fixable side, and
is the first whose mistake is a *syntax habit* rather than a typo — the
kind that recurs on essentially every multi-effect signature an LLM
writes.
