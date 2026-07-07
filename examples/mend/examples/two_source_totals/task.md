# Task: two_source_totals

## Prompt fed to the LLM

> Write a Kāra program that reads a count from two separate data sources
> and returns their combined total. Define:
>
> - a `Source` trait with `load(ref self) -> i64`,
> - two effect resources `UserDB` and `OrderDB`, both backed by `Source`,
> - `fetch_users()` and `fetch_orders()` — each loads from its resource,
> - `total()` — a `pub fn` returning the sum of the two, whose signature
>   declares that it reads from *both* resources,
> - `main()` — prints `total()`.
>
> The compiler will check your code with `karac check --output=json` and
> report structured diagnostics. If any error carries a `replacement`
> field, run `karac fix` to apply it mechanically; patch any descriptive
> errors yourself and re-check.

## Why this task

It exercises the **parse-phase machine-applicable fix** for effect
clauses. A `pub fn` that touches two resources needs a multi-effect
signature — `with reads(UserDB) reads(OrderDB)`. Kāra effect clauses are
**space-separated**, but every mainstream language comma-separates lists,
so the overwhelmingly common LLM/porting mistake is to write
`with reads(UserDB), reads(OrderDB)`.

Before the parser learned this shape, the stray comma surfaced as a
baffling `Expected LeftBrace, found Comma` with no fix, and the loop
could only recover by having the LLM re-reason from prose. Now the
compiler emits a focused diagnostic *and* a machine-applicable
delete-the-comma edit, so `karac fix` resolves it in a single pass with
zero LLM reasoning — the `clean-after-karac-fix` outcome the scorer
counts toward the machine-fix rate.

This is the corpus's first example whose natural LLM mistake lands in the
*machine-fixable* class rather than the LLM-reasoning class.
