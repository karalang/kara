# render_status — a machine-fixable dropped match arm

## The mistake

A `match` over a four-variant enum, with the last variant forgotten:

```kara
match o {
    Pending => 1,
    Shipped => 2,
    Delivered => 3,
    // Cancelled dropped
}
```

Dropping the tail case of an enum match is one of the most common LLM
errors — the model handles the variants it "expects" and forgets the one
declared last. It's not a reasoning gap: the compiler *already knows*
exactly which variant is uncovered.

## Why it's the machine-fixable class

`karac check --output=json` reports `E0205` with a `replacement` that
inserts the missing arm:

```
[E0205] typecheck: non-exhaustive match: missing variants: Cancelled
    replacement: {offset: …, length: 0, text: ", Cancelled => todo()"}
```

`karac fix` inserts `Cancelled => todo()` and the match is exhaustive.
The arm body is a `todo()` stub — the fix makes the program *compile*,
leaving the real handler for the author (or the next LLM turn), which is
the right division of labor: the compiler does the mechanical part it can
prove, the human/LLM supplies the semantics.

The exhaustiveness checker yields one missing variant at a time, so a
match dropping several arms is completed one arm per `karac fix` pass —
the Mend loop iterates to convergence.

## Where it sits in the corpus

Machine-fixable, like `welcome_emails` (resolver did-you-mean) and
`two_source_totals` (effect-clause comma) — and the first example whose
fix comes from the **typecheck** phase rather than resolve or parse.
`concurrent_emails` remains the contrast: its `E0408` fix is a structural
refactor with a three-way choice, correctly descriptive (`fixed-by-llm`).
