# Task: render_status

## Prompt fed to the LLM

> Write a Kāra program with an `Order` enum whose variants are `Pending`,
> `Shipped`, `Delivered`, and `Cancelled`. Define `status_code(o: Order)
> -> i64` that returns a distinct code for each status via a `match`, and
> a `main` that prints the code for one order.
>
> The compiler will check your code with `karac check --output=json`. If
> an error carries a `replacement` field, run `karac fix` to apply it
> mechanically; patch any descriptive errors yourself and re-check.

## Why this task

It exercises the **E0205 non-exhaustive-match** machine fix. A `match`
over a four-variant enum is the single most common place an LLM drops a
case — it handles the "happy" variants (`Pending`/`Shipped`/`Delivered`)
and forgets the tail one (`Cancelled`), especially when the enum was
declared a few lines up.

The exhaustiveness checker already knows exactly which variant is
missing, so this is squarely the *machine-fixable* class: `karac check`
reports `E0205` with a `replacement` that inserts `Cancelled => todo()`,
and `karac fix` completes the match in one pass. In the Mend loop that is
the `clean-after-karac-fix` outcome — the compiler closing the loop, no
LLM reasoning.

It joins `welcome_emails` (resolver did-you-mean) and `two_source_totals`
(effect-clause comma) on the machine-fixable side of the corpus, and is
the first covering a **typecheck**-phase fix.
