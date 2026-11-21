# canonical_request — what this task is designed to provoke

Shapes the task routes through, not a record of what any LLM did. This solution
was authored by someone who already knew the answers, so it measures nothing
about the wedge.

## 1. `i64 as char` is rejected

The obvious lowercase is `(ch as i64) + 32) as char`, which is
`E_INT_AS_CHAR` — not every integer is a scalar value. `u8 as char` IS allowed,
so the fix is to work in bytes via `s.bytes()`. No machine fix; the author has
to read the diagnostic and restructure. This is the mistake the parent artifact
actually made.

## 2. `&&` / `||`

Rust habit. Kāra uses `and` / `or`. The diagnostic is a parse error, so it
masks everything behind it; it is reported once per occurrence and carries a
`replacement`, which `karac fix` applies (`B-2026-08-01-25`, fixed). Only then
does typecheck see §1.

## 3. Moving out of a bare parameter

A bare parameter borrows (core-semantics.md §4.1), and a bare `for` borrows the
collection it walks (§4.6). `sort_headers(headers: Vec[Header])` walks
`headers` and inserts each element into the `Vec` it returns, so the element
cannot be moved out: `E0200`, `expected 'Header', found 'ref Header'`, with
both halves of the fix as one `replacement`, `headers: own Vec[Header]` and
`for h in headers.into_iter()`. `sort_params` is the same, and the bare
`String` parameters that `canonical_request` stores in a struct literal each
get `own` (`cannot give away parameter`). Every one is machine-applicable, the
`fixed-by-karac` path this corpus exists to count; the callers that then hand
on a parameter of their own get the same fix on the next pass.

The sort keys need no copy: `lower(h.name)` and `uri_encode(p.key, false)`
borrow, so a `.clone()` written there out of Rust habit is the
`redundant_clone` warning, whose fix drops it.

## 4. Two sorts, two different keys

Headers sort by *lowercased name*; params sort by *encoded key*. Reusing one
comparator for both is the natural simplification and is wrong — but only
observably wrong on inputs where the orders differ, which is why the fixture
supplies `Zed`, `alpha`, `Mid` (uppercase sorts before lowercase in byte order,
so `Mid Zed alpha` is correct and `alpha Mid Zed` is the plausible error).

## 5. The `/` rule inverts

`/` is encoded in a query VALUE (`x/y` → `x%2Fy`) and left alone in the PATH.
An implementation with one encoder and no flag passes the path check or the
value check, never both. The `---` section prints the same input encoded both
ways so the oracle pins the difference directly.

## Oracle

`expected.txt`, verified byte-identical under `karac run --interp` and a
`karac build` binary. The equality is load-bearing rather than decorative: the
by-value-`Vec` element move in `sort_headers` is exactly the shape that
double-freed under codegen while the interpreter stayed correct
(`B-2026-08-01-24`).
