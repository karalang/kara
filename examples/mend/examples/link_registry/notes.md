# link_registry — what this task is designed to provoke

Not a claim about what an LLM *did* — this solution was authored by someone who
already knew the answers, so it measures nothing about the wedge. These are the
shapes the task deliberately routes through, and what each is expected to cost
a fresh author.

## 1. Module-level mutable state, written from a function

`shorten` mutates four module-level bindings. A `let` (not `let mut`) on any of
them is an error at the write site, and the naming rule
(`E_MODULE_BINDING_NAMING`) wants SCREAMING_SNAKE_CASE — both carry
machine-applicable `replacement` fields, so `karac fix` should close them
without the LLM re-reasoning. B-2026-07-31-31/-32/-33 were all found sharpening
exactly that path, so this task doubles as their regression surface.

## 2. Use-after-move on a `String` used as both key and value

`shorten` needs `code` as a `LINKS` key, a `HITS` key, an `ORDER` element, and
the return value. Three of the four need `.clone()`. This is the E0500 shape,
and its fix is machine-applicable — `karac fix` inserted exactly these clones
when the parent example (`examples/shortener/`) was written.

`follow` has the same shape at lower density: `code` is consumed by the second
`HITS.insert`, so the two lookups before it need clones.

## 3. `char_at` returns `Option[char]`, not `char`

`encode_code` must `match` it. Indexing a `StringSlice` directly, or treating
the result as a `char`, is a type error with no machine fix — the LLM has to
read the diagnostic and restructure. Good diagnostic-quality signal.

## 4. Map iteration order is backend-dependent

The prompt says to fold `HITS` into totals rather than print per entry. An
author who ignores that gets a program whose stdout matches the oracle on one
backend and not the other — the `fixed-by-karac` + oracle-FAIL category, and a
true positive: it IS wrong to depend on that order. Worth watching whether
blind authors take the instruction.

## Oracle

`expected.txt`, byte-identical under `karac run --interp` and a `karac build`
binary (checked at authoring time). The interp == compiled equality is the part
that catches a silent codegen divergence; stdout equality alone would not.
