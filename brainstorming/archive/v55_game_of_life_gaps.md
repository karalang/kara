# Language Design Gaps — Game of Life

Gaps found while building `examples/game_of_life/` — a Conway's Game of Life
simulator with layout blocks and an attempt to exploit Kāra's parallel execution model.

---

## GAP-H — Layout blocks with heap-allocated fields

**File:** `src/grid.kara`

**Observed:** The spec's layout block examples (`design.md § Feature 1`) show scalar
fields being grouped for SoA:

```kara
struct Particle { x: f32, y: f32, vx: f32, vy: f32 }
layout Particle { group { x, y }; group { vx, vy } }
```

For `Grid`, the `cells` field is `Vec[bool]` — heap-allocated. Three interpretations
are possible and the spec is silent:

1. **Header-level SoA:** group `{ width, height }` keeps the two scalar metadata
   fields contiguous (likely in one cache line). Group `{ cells }` is the Vec header
   (ptr + len + cap). The actual cell data is on the heap regardless.
2. **Inline buffer:** the compiler flattens `Vec[bool]` into an inline array when
   the size is statically known — not applicable here (width/height are runtime values).
3. **Ignored:** layout blocks only apply to scalar fields; heap-indirected fields
   are silently left as-is.

**Impact:** If interpretation 3 is correct, the layout block in `grid.kara` is
misleading — it looks like it affects the cell data but does nothing. The spec should
explicitly state whether layout blocks apply to heap-allocated fields and, if so,
what they control.

**Proposal:** Extend the layout block spec to cover three cases:
- `group { scalar_fields }` — current behaviour (SoA for scalars)
- `group { vec_field }` — controls Vec *header* placement (minor)
- `inline { vec_field }` — opt-in for fixed-size inline buffer (requires compile-time
  capacity annotation, related to `Array[T, N]` vs `Vec[T]`)

**Status:** RESOLVED. Interpretation 1 (header-level SoA only) is correct and now documented. Added "Heap-allocated fields in layout blocks" bullet to design.md §Layout Rules: `group` on a heap-allocated field moves only the field's `{ ptr, len, cap }` header into the SoA backing array; the actual heap buffer is unaffected. The `inline {}` directive is not in v1 — use `Array[T, N]` for fixed-size in-struct storage.

---

## GAP-I — No `par for` — intra-step parallelism requires manual decomposition

**File:** `src/grid.kara`

**Observed:** The Game of Life `step` function is an ideal auto-parallelism
candidate: reading `old_grid` and writing `new_grid` touches two distinct allocations
with non-conflicting effects. But exploiting this within a loop requires either:

1. A `par for y in 0..height { ... }` syntax (not in spec).
2. Manually splitting the grid into N chunks and issuing N statements in a `par {}`
   block (requires knowing N at compile time).
3. Using `spawn()` / `TaskGroup` for dynamic parallelism (API not specified).

The spec's `par {}` works on *statements*, not loop iterations. This means Kāra's
most natural form of data parallelism — running the same pure function over each
element of a collection — has no direct surface syntax.

**Spec ref:** `design.md § Feature 5 — Auto-Concurrency`, `§ Explicit Concurrency`.

**Proposal:** `par for item in collection { body }` as a first-class form.
The compiler verifies that `body`'s effect set is the same for all iterations
(no data dependence between iterations) before emitting parallel code.
Alternatively, a `parallel_map(collection, pure_fn)` stdlib function with explicit
purity requirement in its effect signature.

**Status:** DEFERRED (P2). Added to `docs/deferred.md §par for — Data-Parallel Loop Syntax`. The `TaskGroup` workaround covers the pattern today. `parallel_map` is identified as a potential stepping stone before full `par for` syntax.

---

## GAP-J — No `Vec.split_at_mut` / disjoint mutable slice decomposition

**File:** `src/grid.kara`

**Observed:** Even with a `par for` form, writing to two disjoint halves of a
`Vec[bool]` from parallel tasks requires the type system to prove the ranges don't
overlap. In Rust, `split_at_mut` creates two `&mut [T]` slices that the borrow
checker verifies are non-overlapping. Kāra has `Slice[T]` and `mut Slice[T]`,
but no `Vec.split_at_mut` or equivalent in the stdlib table.

Without this primitive, there is no safe way to express "task A writes cells[0..N/2]
and task B writes cells[N/2..N]" at the type level. Both tasks would have
`writes(cells_resource)` and be forced to serialize.

**Spec ref:** `design.md § Standard Data Structures — Slice[T]`.

**Proposal:**
```kara
fn split_at_mut(mut self, mid: i64) -> (mut Slice[T], mut Slice[T])
    with panics   // panics if mid > len
```
The two returned slices are guaranteed disjoint by construction; the compiler can
assign them distinct synthetic effect resources. This is the foundational primitive
for safe parallel writes into a shared buffer.

**Status:** RESOLVED. Added `split_at_mut` to design.md §Slices with full signature, disjointness guarantee, and explanation of why the "one mutable view at a time" aliasing rule is satisfied by construction. Available on both `Vec[T]` and `mut Slice[T]`.

---

## GAP-K — No string builder / efficient character accumulation

**File:** `src/display.kara`

**Observed:** Rendering a grid row requires concatenating one character per cell.
`String + String` allocates a new heap buffer on every `+`. For a 20-column grid,
rendering one row performs 19 heap allocations. The full grid (20 rows × 19 allocs)
is 380 allocations per frame.

The correct fix is a `StringBuilder` that pre-allocates a buffer and appends into it:

```kara
let mut sb = StringBuilder.with_capacity(grid.width);
for x in 0..grid.width {
    sb.push_char(if grid.get(x, y) { '█' } else { '·' });
}
println(sb.to_string());
```

**Spec ref:** `design.md § Standard Data Structures — String`.

**Proposal:** Add `StringBuilder` (or extend `String` with `with_capacity(n)` +
`push_char(c)` + `push_str(s)` mutating methods). This is a standard addition in
virtually every language's stdlib and is especially important for Kāra's
performance-conscious design.

**Status:** RESOLVED. Added `with_capacity`, `push_char`, and `push_str` to the `String` method table in design.md §Collection Core Methods. No `StringBuilder` type needed — the idiom is `String.with_capacity(n)` followed by `push_char` / `push_str` calls. Added a note to the `+` row directing users to `push_str` in loops.

---

## GAP-L — No terminal control in stdlib

**File:** `src/display.kara`

**Observed:** Clearing the screen requires the raw ANSI escape sequence `"\x1b[2J\x1b[H"`.
This is platform-specific, fragile (doesn't work on Windows without VT mode), and
unchecked (no effect declared — it writes to Stdout but that effect is only tracked
if the call goes through `print`/`println`).

**Proposal:** A `std.terminal` module with:
```kara
pub fn clear_screen() with writes(Stdout) blocks { ... }
pub fn move_cursor(row: i64, col: i64) with writes(Stdout) { ... }
pub fn set_color(fg: Color, bg: Color) with writes(Stdout) { ... }
```
These would carry `writes(Stdout)` so they participate in effect tracking, and they
would abstract over platform differences (ANSI on Unix/macOS, Console API on Windows).

**Status:** OUT OF SCOPE (v1). Terminal control is platform-specific and better owned by an ecosystem library than the core stdlib. For v1, raw ANSI escapes via `print`/`println` (carrying `writes(Stdout)`) are the intended path. Added to `docs/deferred.md §Terminal Control Library` as a P3 ecosystem item.

---

## GAP-U — Effect resources for distinct allocations are implicit

**File:** `src/grid.kara`

**Observed:** The Game of Life `step` function reads from `self` (old grid) and writes
to `next` (new grid). These are different heap allocations — logically, different
effect resources. The compiler *should* be able to prove they don't conflict, enabling
parallel cell updates.

But the spec does not state whether two distinct heap allocations of the same type
are assigned distinct synthetic effect resources. The module-level `let mut` synthetic
resource system applies only to named bindings, not to function-local heap allocations.

If both `self.cells` and `next.cells` are attributed to a single
`reads/writes(Vec[bool]_resource)`, intra-step parallelism becomes impossible to
express without introducing explicit effect resources for each allocation.

**Proposal:** Clarify in the spec whether distinct `Vec`/`shared struct` allocations
receive distinct synthetic effect resources for conflict analysis purposes, or whether
all allocations of the same type share one resource identity. This is a foundational
question for the auto-concurrency model's applicability to in-place or double-buffered
computation patterns.

**Status:** RESOLVED. Added "Function-local bindings and heap allocations" paragraph to design.md §Effect Attribution — Synthetic Per-Binding Resource. Clarifies: (1) synthetic per-binding resources apply only to module-level `let mut`; (2) struct field reads/writes in function bodies do not contribute to named-resource effects — they are governed by the ownership system's aliasing analysis; (3) two non-aliased function parameters of the same type (e.g., `ref self: Grid` and `next: Grid`) access independent heap allocations and produce no named-resource conflict, so the auto-concurrency model may parallelize their callers freely.
