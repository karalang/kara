# link_registry — what this task is designed to provoke

Not a claim about what an LLM *did* — this solution was authored by someone who
already knew the answers, so it measures nothing about the wedge. These are the
shapes the task deliberately routes through, and what each is expected to cost
a fresh author.

## 1. Mutable state passed down from `main`

`shorten` and `follow` write four fields of a `Registry` that `main` owns, so
they take it as `reg: mut ref Registry` and each call site marks the argument
`mut reg`. A fresh author is likely to reach for module-level `let mut` state
instead; that is refused (`E_MODULE_LET`, B-2026-10-22-8) with no
machine-applicable fix, since moving the state into `main` is a restructuring
the diagnostic can only describe. A plain module `let` for the alphabet is
refused too, and that one does carry a `replacement` to `const`.

## 2. Use-after-move on a `String` used as both key and value

`shorten` needs `code` as a `links` key, a `hits` key, an `order` element, and
the return value. Three of the four need `.clone()`. This is the E0500 shape,
and its fix is machine-applicable — `karac fix` inserted exactly these clones
when the parent example (`examples/shortener/`) was written.

`follow` has the same shape at lower density: `code` is consumed by the second
`hits.insert`, so the two lookups before it need clones.

## 3. `char_at` returns `Option[char]`, not `char`

`encode_code` must `match` it. Indexing a `StringSlice` directly, or treating
the result as a `char`, is a type error with no machine fix — the LLM has to
read the diagnostic and restructure. Good diagnostic-quality signal.

## 4. Map iteration order is backend-dependent

The prompt says to fold `hits` into totals rather than print per entry. An
author who ignores that gets a program whose stdout matches the oracle on one
backend and not the other — the `fixed-by-karac` + oracle-FAIL category, and a
true positive: it IS wrong to depend on that order. Worth watching whether
blind authors take the instruction.

## Oracle

`expected.txt`, byte-identical under `karac run --interp` and a `karac build`
binary (checked at authoring time). The interp == compiled equality is the part
that catches a silent codegen divergence; stdout equality alone would not.
