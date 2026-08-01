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

Rust habit. Kāra uses `and` / `or`. The diagnostic names the replacement but
carries none (`B-2026-08-01-25`), and it is emitted 2–3× per occurrence, so this
is also a live measurement of that entry: the count of diagnostics an author
wades through here should drop sharply once it is fixed.

## 3. Use-after-move on the sort key

`sort_headers` needs `h.name` for the comparison key and again inside the
element it pushes; `canonical_request` needs each header's lowered name twice —
once for the `name:value` line and once for the signed-headers list. Both are
E0500 with machine-applicable `.clone()` fixes, which is the `fixed-by-karac`
path this corpus exists to count.

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
