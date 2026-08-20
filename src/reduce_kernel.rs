//! Shared, backend-agnostic vocabulary + interpreter math for the
//! Reduce / ElementwiseOrd family of statistical container operations
//! (`Tensor`, `Column`, and the `Stats.*` free functions).
//!
//! This is the same "one table, three consumers" model as
//! [`crate::float_math`]: a single definition the typechecker, interpreter,
//! and (in later slices) codegen all key off, so a reduction can't drift
//! between `karac run` and `karac build`. Today it backs the **interpreter
//! twin** — `eval_stats_fn`, `eval_column_reduce`, and the `Tensor`/`Column`
//! min/max helpers funnel their f64 math through the one implementation here
//! instead of each re-deriving mean/variance/median/quantile.
//!
//! **Plain data only.** No `inkwell`/LLVM types and no interpreter `Value`
//! references live here (the codegen-containment invariant, CLAUDE.md §
//! Architecture). The `ReduceOp` enum is the vocabulary the LLVM emitter will
//! consume in S1+ (see `docs/spikes/reduce-elementwise-trait-unification.md`);
//! the `Value`-shaped glue (min/max over `Value`, `Value → f64`) stays in the
//! interpreter.

/// Width of one GPU reduction workgroup — the shader's `@workgroup_size`, its
/// `scratch` array length, and the padding width of [`tree_reduce_f32`].
///
/// Defined HERE rather than in the emitter because all three must be the same
/// number: the tree order is language semantics (see [`tree_reduce_f32`]), and
/// a width that differed between the shader and its CPU twin would change the
/// answer while every individual piece still looked correct.
pub const GPU_REDUCE_WIDTH: usize = 64;

/// The CPU twin of the GPU tree reduction — **the definition of what a
/// `gpu.sum` / `gpu.prod` MEANS**, not merely a fallback (B-2026-08-19-10).
///
/// A GPU reduction is a tree and a CPU fold is a line. `f32` addition is not
/// associative, so the two genuinely disagree: 64 copies of `0.1` sum to
/// `6.400000` here and `6.399996` under `xs.iter().sum()`. Kāra specifies the
/// tree order and this function reproduces it exactly, so `karac run` and
/// `karac build` agree bit-for-bit rather than within an epsilon — which is
/// what keeps the A/B rule (kara-katas/CLAUDE.md, "a run/build divergence is a
/// compiler bug") intact for a feature that could easily have broken it.
///
/// The order, matching the emitted shader step for step: pad to
/// [`GPU_REDUCE_WIDTH`] with `identity`, then halve —
/// `s[t] = s[t] OP s[t + stride]` for stride 32, 16, 8, 4, 2, 1.
///
/// Handles any length: up to [`GPU_REDUCE_WIDTH`] is one workgroup's tree,
/// beyond that the buffer is chunked into per-workgroup partials which are
/// then folded the same way — the recursion the multi-workgroup dispatch
/// performs. See [`tree_fold_f32`].
///
/// Incidentally the more ACCURATE order: pairing keeps partial sums at similar
/// magnitudes instead of repeatedly adding a small value to a growing one.
pub fn tree_reduce_f32(xs: &[f32], op: ReduceOp) -> Option<f32> {
    // `None` for everything that needs more than one associative pass — see
    // `gpu_wgsl::emit_reduce_kernel`, which refuses the same set.
    let (combine, identity) = reduce_combiner_f32(op)?;
    Some(tree_fold_f32(xs, combine, identity))
}

/// The CPU twin of an INTEGER GPU reduction — **the definition of what
/// `gpu.sum(Vec[i32])` and friends mean, including which of them trap**
/// (B-2026-08-19-13).
///
/// Two layers of failure, and they are different questions:
///
///  * `None` — the op has no single-shader tree form (the outer layer, same
///    set [`tree_reduce_f32`] refuses).
///  * `Some(Err(IntFoldOverflow))` — the op ran and OVERFLOWED. Integer
///    reductions trap, exactly as `v.sum()` over a `Vec[i32]` already does on
///    both surfaces; wrapping instead would turn a trap into a wrong answer
///    the moment a reduction moved to the GPU (design.md § Integer reductions
///    overflow-check).
///
/// **The tree order decides WHETHER it traps, not just what it returns.**
/// Overflow is a property of the intermediate sums, and a tree forms different
/// intermediates than a line — both directions are reachable, with
/// `MAX = i32::MAX`:
///
/// | buffer | this tree | a left fold |
/// |---|---|---|
/// | `[MAX, MAX, -MAX, -MAX]` | `0` | overflows |
/// | `[MAX, -MAX, MAX, -MAX]` | overflows | `0` |
///
/// So `gpu.sum(v)` and `v.sum()` may legitimately disagree about failing on
/// the same integer buffer. That is specified behaviour, not a divergence —
/// but it does mean swapping one for the other is not a pure speedup on
/// integer data. Reproducing the trap POINTS here, rather than only the final
/// value, is what keeps `karac run` and `karac build` agreeing about which
/// programs fail.
///
/// `Min`/`Max` cannot overflow and never return `Err`.
pub fn tree_reduce_i32(xs: &[i32], op: ReduceOp) -> Option<Result<i32, IntFoldOverflow>> {
    let (combine, identity): CheckedCombiner = match op {
        ReduceOp::Sum => (i32::checked_add, 0),
        ReduceOp::Prod => (i32::checked_mul, 1),
        // Infallible, but typed the same so one fold serves all four.
        ReduceOp::Min => (|a, b| Some(a.min(b)), i32::MAX),
        ReduceOp::Max => (|a, b| Some(a.max(b)), i32::MIN),
        _ => return None,
    };
    Some(tree_fold_i32(xs, combine, identity))
}

/// One checked integer reduction as data: a combining function that reports
/// overflow by returning `None`, and the identity that pads a short chunk.
///
/// The integer sibling of [`Combiner`], differing only in that its combine can
/// FAIL — which is the whole of the integer-reduction decision in one type.
type CheckedCombiner = (fn(i32, i32) -> Option<i32>, i32);

/// The unsigned sibling of [`CheckedCombiner`]. Separate because the failing
/// combine is `u32::checked_add`, whose overflow condition (a carry) is not
/// the signed one (a shared-sign-then-flip).
type CheckedCombinerU = (fn(u32, u32) -> Option<u32>, u32);

/// The recursion the multi-workgroup dispatch performs, at `i32` — the integer
/// sibling of [`tree_fold_f32`], chunk for chunk.
fn tree_fold_i32(
    xs: &[i32],
    combine: fn(i32, i32) -> Option<i32>,
    identity: i32,
) -> Result<i32, IntFoldOverflow> {
    if xs.len() <= GPU_REDUCE_WIDTH {
        return one_workgroup_i32(xs, combine, identity);
    }
    let mut partials: Vec<i32> = Vec::with_capacity(xs.len().div_ceil(GPU_REDUCE_WIDTH));
    for chunk in xs.chunks(GPU_REDUCE_WIDTH) {
        partials.push(one_workgroup_i32(chunk, combine, identity)?);
    }
    tree_fold_i32(&partials, combine, identity)
}

/// One workgroup's halving tree at `i32`, failing on the first overflow.
///
/// Note it fails on the FIRST overflowing combine in the shader's own order,
/// so a buffer that overflows at stride 32 never reaches stride 16 — the
/// device does the same, because a lane that overflows raises the flag and the
/// host stops at the end of that dispatch.
fn one_workgroup_i32(
    xs: &[i32],
    combine: fn(i32, i32) -> Option<i32>,
    identity: i32,
) -> Result<i32, IntFoldOverflow> {
    debug_assert!(xs.len() <= GPU_REDUCE_WIDTH);
    let mut scratch = [identity; GPU_REDUCE_WIDTH];
    for (slot, &x) in scratch.iter_mut().zip(xs) {
        *slot = x;
    }
    let mut stride = GPU_REDUCE_WIDTH / 2;
    while stride > 0 {
        for t in 0..stride {
            scratch[t] = combine(scratch[t], scratch[t + stride]).ok_or(IntFoldOverflow)?;
        }
        stride /= 2;
    }
    Ok(scratch[0])
}

/// The index of the extremum a GPU `argmin`/`argmax` reports — **the
/// definition of what `gpu.argmin(buf)` MEANS** (B-2026-08-19-13). `None` iff
/// the buffer is empty.
///
/// A different shape from every other reduction here: the tree carries
/// (value, index) PAIRS, because an index alone cannot be compared and a value
/// alone cannot be reported. The combine is a lexicographic order — strictly
/// better value wins; on an exact tie the SMALLER index wins — which makes it
/// a proper semilattice and therefore tree-safe, so the answer is the same
/// whatever the grouping. That is what lets `argmin` promise
/// grouping-independence where `sum` cannot.
///
/// **NaN never wins, from either side.** This DIFFERS from `Stats.argmin`, and
/// deliberately. That one seeds its running best with element 0 and displaces
/// it only on a strict comparison, so a leading NaN is never displaced:
/// `Stats.argmin([NaN, 3.0, 1.0])` is `0` while `Stats.argmin([3.0, 1.0,
/// NaN])` is `1` — the answer depends on where the NaN sits. Position-
/// dependence is exactly what a halving tree cannot reproduce, since the
/// grouping decides the positions. Making NaN always lose restores
/// associativity, at the cost of disagreeing with the CPU function on inputs
/// containing NaN. Same trade, same reason, as `gpu.min`.
///
/// An ALL-NaN buffer reports index 0: no element ever wins, so the leftmost
/// survives. Padding does not interfere — a padded slot is marked by the
/// [`ARG_INVALID`] index sentinel rather than by a sentinel VALUE, so it can
/// never beat a real element regardless of what is in its value slot. (A value
/// sentinel would have had to be NaN to lose reliably, and NaN preservation is
/// optional in Vulkan — the index sentinel needs no such guarantee.)
pub fn tree_arg_f32(xs: &[f32], want_max: bool) -> Option<i64> {
    tree_arg_with(xs, want_max, arg_takes_b)
}

/// The `i32` sibling of [`tree_arg_f32`]. Same tree, same tie-break, no NaN
/// rule — integers are totally ordered, so the combine is just "strictly
/// better, or an equal value at a smaller index".
pub fn tree_arg_i32(xs: &[i32], want_max: bool) -> Option<i64> {
    tree_arg_with(xs, want_max, arg_takes_b_i32)
}

/// The `u32` sibling. Split from the signed one because the ORDER differs
/// above 2^31 — `4294967295` is `-1` read as `i32`, so a signed compare
/// answers argmin and argmax backwards on exactly the values unsigned data is
/// most likely to contain.
pub fn tree_arg_u32(xs: &[u32], want_max: bool) -> Option<i64> {
    tree_arg_with(xs, want_max, arg_takes_b_u32)
}

/// The tree itself, shared by every element type: level 0 seeds each element
/// as its own candidate, then the surviving candidates are folded in
/// workgroup-wide chunks until one remains. Only the COMBINE varies.
fn tree_arg_with<T>(
    xs: &[T],
    want_max: bool,
    takes_b: fn(u32, u32, &[T], bool) -> bool,
) -> Option<i64> {
    if xs.is_empty() {
        return None;
    }
    let mut level: Vec<u32> = (0..xs.len() as u32).collect();
    while level.len() > 1 {
        level = level
            .chunks(GPU_REDUCE_WIDTH)
            .map(|chunk| one_workgroup_arg(chunk, xs, want_max, takes_b))
            .collect();
    }
    Some(level[0] as i64)
}

/// The index sentinel marking a padded (non-existent) slot in an arg tree.
///
/// A slot is invalid iff its INDEX is this, never because of its value. That
/// keeps the padding rule independent of float semantics: a value-based
/// sentinel would have to be NaN to lose against everything, and NaN
/// preservation is an OPTIONAL Vulkan feature, so a device that flushed it
/// would silently let padding win.
pub const ARG_INVALID: u32 = u32::MAX;

/// One workgroup's halving tree over (value, index) pairs.
fn one_workgroup_arg<T>(
    candidates: &[u32],
    xs: &[T],
    want_max: bool,
    takes_b: fn(u32, u32, &[T], bool) -> bool,
) -> u32 {
    debug_assert!(candidates.len() <= GPU_REDUCE_WIDTH);
    let mut idxs = [ARG_INVALID; GPU_REDUCE_WIDTH];
    for (slot, &c) in idxs.iter_mut().zip(candidates) {
        *slot = c;
    }
    let mut stride = GPU_REDUCE_WIDTH / 2;
    while stride > 0 {
        for t in 0..stride {
            let (ia, ib) = (idxs[t], idxs[t + stride]);
            if takes_b(ia, ib, xs, want_max) {
                idxs[t] = ib;
            }
        }
        stride /= 2;
    }
    idxs[0]
}

/// The INTEGER combine: everything the float one does except the NaN rules,
/// which have nothing to bite on. `<` and `>` on a signed slice are signed;
/// on an unsigned one, unsigned — which is the whole difference between the
/// two integer arms.
fn arg_takes_b_i32(ia: u32, ib: u32, xs: &[i32], want_max: bool) -> bool {
    if ib == ARG_INVALID {
        return false;
    }
    if ia == ARG_INVALID {
        return true;
    }
    let (a, b) = (xs[ia as usize], xs[ib as usize]);
    let strictly_better = if want_max { b > a } else { b < a };
    strictly_better || (b == a && ib < ia)
}

/// The unsigned combine. Identical in shape; the comparison is what differs.
fn arg_takes_b_u32(ia: u32, ib: u32, xs: &[u32], want_max: bool) -> bool {
    if ib == ARG_INVALID {
        return false;
    }
    if ia == ARG_INVALID {
        return true;
    }
    let (a, b) = (xs[ia as usize], xs[ib as usize]);
    let strictly_better = if want_max { b > a } else { b < a };
    strictly_better || (b == a && ib < ia)
}

/// Does the right-hand candidate beat the left-hand one? The whole combine
/// rule in one place, so the shader has exactly one thing to reproduce.
fn arg_takes_b(ia: u32, ib: u32, xs: &[f32], want_max: bool) -> bool {
    if ib == ARG_INVALID {
        return false;
    }
    if ia == ARG_INVALID {
        return true;
    }
    let (a, b) = (xs[ia as usize], xs[ib as usize]);
    if a.is_nan() {
        // A NaN loses to anything real, and ties with another NaN — where the
        // smaller index (the left one) survives.
        return !b.is_nan();
    }
    if b.is_nan() {
        return false;
    }
    let strictly_better = if want_max { b > a } else { b < a };
    // Exact tie: the SMALLER index wins, matching `Stats.argmin`'s
    // first-occurrence rule. `ib < ia` is possible because the fold levels
    // carry absolute indices that are not in scratch order.
    strictly_better || (b == a && ib < ia)
}

/// The `u32` sibling of [`tree_reduce_i32`] — same tree, same trap rule, same
/// two-layer result (B-2026-08-19-13).
///
/// Separate from the signed twin rather than generic over it because the
/// OVERFLOW CONDITION is genuinely different: a signed add overflows when the
/// operands share a sign and the result does not, an unsigned one when it
/// carries. `checked_add` encodes each correctly for its own type, and that is
/// the whole reason both exist.
///
/// Note what is NOT different: the runtime entry point. It moves 4-byte words
/// without interpreting them — `in_ptr` is cast straight to `*const u8` and
/// `out` is written from raw little-endian bytes — so signedness never reaches
/// it. The only place it matters on the compiled path is the widening of the
/// 32-bit result into Kāra's i64 carrier, where `u32` needs a ZERO-extend; a
/// sign-extend would report every value at or above 2^31 as negative.
pub fn tree_reduce_u32(xs: &[u32], op: ReduceOp) -> Option<Result<u32, IntFoldOverflow>> {
    let (combine, identity): CheckedCombinerU = match op {
        ReduceOp::Sum => (u32::checked_add, 0),
        ReduceOp::Prod => (u32::checked_mul, 1),
        ReduceOp::Min => (|a, b| Some(a.min(b)), u32::MAX),
        ReduceOp::Max => (|a, b| Some(a.max(b)), u32::MIN),
        _ => return None,
    };
    Some(tree_fold_u32(xs, combine, identity))
}

/// The recursion the multi-workgroup dispatch performs, at `u32`.
fn tree_fold_u32(
    xs: &[u32],
    combine: fn(u32, u32) -> Option<u32>,
    identity: u32,
) -> Result<u32, IntFoldOverflow> {
    if xs.len() <= GPU_REDUCE_WIDTH {
        return one_workgroup_u32(xs, combine, identity);
    }
    let mut partials: Vec<u32> = Vec::with_capacity(xs.len().div_ceil(GPU_REDUCE_WIDTH));
    for chunk in xs.chunks(GPU_REDUCE_WIDTH) {
        partials.push(one_workgroup_u32(chunk, combine, identity)?);
    }
    tree_fold_u32(&partials, combine, identity)
}

/// One workgroup's halving tree at `u32`, failing on the first overflow.
fn one_workgroup_u32(
    xs: &[u32],
    combine: fn(u32, u32) -> Option<u32>,
    identity: u32,
) -> Result<u32, IntFoldOverflow> {
    debug_assert!(xs.len() <= GPU_REDUCE_WIDTH);
    let mut scratch = [identity; GPU_REDUCE_WIDTH];
    for (slot, &x) in scratch.iter_mut().zip(xs) {
        *slot = x;
    }
    let mut stride = GPU_REDUCE_WIDTH / 2;
    while stride > 0 {
        for t in 0..stride {
            scratch[t] = combine(scratch[t], scratch[t + stride]).ok_or(IntFoldOverflow)?;
        }
        stride /= 2;
    }
    Ok(scratch[0])
}

/// The CPU twin of `gpu.mean(buf)` — **the definition of what a GPU mean
/// MEANS** (B-2026-08-19-13). `None` iff the buffer is empty.
///
/// Deliberately "the specified tree sum, divided by the count, once". Not a
/// compensated mean, not a higher-precision accumulation: exactly
/// `tree_reduce_f32(xs, Sum) / n`, in `f32`, so `gpu.mean(v)` and
/// `gpu.sum(v) / (v.len() as f32)` are the same number to the last bit. Mean
/// therefore inherits the sum's specified grouping and adds exactly ONE
/// further rounding, which is the whole of its precision story.
///
/// The division happens on the HOST, after the fold has converged — never in
/// the shader. A shader cannot know it is running the last level of the tree,
/// so a per-dispatch division would divide once per level. That is why `mean`
/// needs no shader of its own: it reuses the sum kernel unchanged, and the
/// only new thing in the whole operation is one `f32` divide.
///
/// Empty is `None` rather than `0.0 / 0 == NaN`. The mean of nothing is not a
/// number, and NaN is precisely the plausible-looking value that propagates
/// silently through everything downstream — the same reasoning that makes
/// `min`/`max` fallible here.
pub fn tree_mean_f32(xs: &[f32]) -> Option<f32> {
    if xs.is_empty() {
        return None;
    }
    let sum = tree_reduce_f32(xs, ReduceOp::Sum)?;
    Some(sum / xs.len() as f32)
}

/// The CPU twin of `gpu.variance(buf)` — **the definition of what a GPU
/// variance means** (B-2026-08-19-13). `None` iff the buffer is empty.
///
/// The first reduction here that is genuinely TWO PASSES: the mean has to
/// exist before a single squared deviation can be formed, so the device runs
/// a complete sum reduction, reads the answer back, and dispatches again with
/// the mean as a uniform. Every reduction before this one was a single
/// converging fold.
///
/// The order, matching the device step for step:
///
/// 1. `mean = tree_sum(xs) / n`, in `f32` — the same tree
///    [`tree_mean_f32`] uses, not a separate accumulation.
/// 2. `d_i = x_i - mean`, squared, formed ON LOAD in the second pass's
///    level-0 shader — so no `n`-element deviation buffer is ever written,
///    the same fusion `gpu.dot` uses.
/// 3. `ss = tree_sum(d_i^2)`, the ordinary sum tree again.
/// 4. `ss / n`, or `ss / (n - 1)` when `bessel`.
///
/// `bessel` selects the SAMPLE form. `Stats.variance` and `Stats.stddev` are
/// POPULATION (`bessel: false`, ÷ n), so `gpu.variance` is too — the two
/// answer the same number on the same buffer.
///
/// Both passes go through `tree_reduce_f32`, so the grouping caveat is
/// inherited rather than re-derived: a long variance is a tree of trees twice
/// over, and in `f32` that is part of the specified answer.
pub fn tree_variance_f32(xs: &[f32], bessel: bool) -> Option<f32> {
    if xs.is_empty() {
        return None;
    }
    let n = xs.len() as f32;
    let mean = tree_reduce_f32(xs, ReduceOp::Sum)? / n;
    let squared: Vec<f32> = xs
        .iter()
        .map(|x| {
            let d = x - mean;
            d * d
        })
        .collect();
    let ss = tree_reduce_f32(&squared, ReduceOp::Sum)?;
    Some(ss / if bessel { n - 1.0 } else { n })
}

/// The CPU twin of an INTEGER `gpu.variance` — **the definition of what
/// `gpu.variance(Vec[i32])` MEANS** (B-2026-08-19-13). `None` iff empty;
/// `Some(Err)` iff the sum of squared deviations overflows `u64`.
///
/// **EXACT, not f32-approximate, and that reverses what this row expected.**
/// The recorded objection was that `mean`'s promote-late trick cannot carry
/// over, because the deviations are formed ON THE DEVICE in f32, so
/// `(x - mean)²` quantises every element above 2²⁴. Both halves of that turn
/// out to be avoidable:
///
///  * **Shift by an INTEGER, not by the mean.** `Var(x) = Var(x - K)` for any
///    constant `K`, so the device subtracts `K = round(mean)` — an integer —
///    in exact integer arithmetic before anything reaches f32. The deviation's
///    magnitude is then bounded by the data's SPREAD rather than by its
///    position on the number line, which is the right dependency: a variance
///    is a function of the spread. Measured on the naive formulation, a buffer
///    centred at 2²⁸ with spread 100 reports 3472.0 where the true variance is
///    3265.28 — a 6% error on ordinary `i32` values. Shifted, it is exact.
///  * **WGSL can multiply exactly.** It has no widening-multiply INTRINSIC,
///    which is what "blocked on WGSL" meant on this row for several batches,
///    but a `u32 × u32 → u64` product is four 16-bit partial products and two
///    carries. So the squares need not be f32 at all: they are exact `u64`,
///    tree-accumulated with carry, and the fold traps on overflow exactly as
///    every other integer reduction here does.
///
/// The whole computation is therefore exact until one final rounding:
///
/// ```text
/// d_i  = x_i - K                      exact, integer
/// S1   = Σ d_i = Σ x_i - n·K          exact, from the integer sum already computed
/// S2   = Σ d_i²                       exact, u64 on the device
/// Var  = (n·S2 - S1²) / n²            exact rational, evaluated in i128
/// ```
///
/// `Var` is formed as a single `i128` numerator over `n²` and rounded ONCE on
/// the way into `f64`, so the result is the correctly-rounded variance. That
/// makes it MORE accurate than `Stats.variance`, which sums f64 deviations and
/// rounds at every step — the two therefore need not agree in the last bit,
/// and the GPU is the one that is right. That is a departure worth naming: for
/// every other op on this row the CPU is the reference.
///
/// **`K = round(mean)` is a choice, and it must be reproduced exactly**, since
/// a different `K` gives a different `S2` and hence different intermediate
/// magnitudes (though the same exact `Var`). Ties round away from zero, which
/// is `f64::round`'s rule and what the runtime does.
///
/// **`Σx` is accumulated at 64 bits rather than through the i32-trapping
/// fold**, which is a deliberate departure from `tree_mean_i32`. Sixty-four
/// values near 2³⁰ already overflow an `i32` sum, so reusing that fold would
/// refuse buffers whose variance is small, exactly representable, and of
/// obvious interest — a trap with nothing wrong behind it. `gpu.mean` has to
/// trap there because it RETURNS the mean, and a mean whose sum overflowed is
/// a number the integer type cannot justify; a variance never exposes the raw
/// sum, so it is not bound by that.
///
/// Overflow is therefore the ONLY failure, and it means the sum of squared
/// deviations does not fit in `u64`: `S2 ≤ n · max|d|²`, so the trap needs
/// roughly `n · spread² > 1.8e19` — for a million elements, a spread past
/// ~4.3e6. Reaching it means the answer genuinely does not fit, which is
/// where the integer reductions promise a trap.
pub fn tree_variance_i32(xs: &[i32], bessel: bool) -> Option<Result<f64, IntFoldOverflow>> {
    if xs.is_empty() {
        return None;
    }
    // The mean's sum is accumulated at 64 bits, NOT through the i32-trapping
    // `tree_reduce_i32`. That matters: 64 values near 2³⁰ already overflow an
    // i32 sum, so routing the mean through the trapping fold would refuse
    // buffers whose VARIANCE is perfectly small and perfectly representable —
    // a trap with nothing wrong behind it. The raw sum is never returned to
    // the caller here, so nothing depends on it being an i32.
    let sum: i128 = xs.iter().map(|&x| x as i128).sum();
    Some(variance_from_shifted(
        xs.iter().map(|&x| x as i128),
        sum,
        xs.len(),
        bessel,
    ))
}

/// The unsigned sibling of [`tree_variance_i32`]. Identical arithmetic — the
/// deviations are signed either way, so the only difference is which exact
/// integer sum feeds the mean.
pub fn tree_variance_u32(xs: &[u32], bessel: bool) -> Option<Result<f64, IntFoldOverflow>> {
    if xs.is_empty() {
        return None;
    }
    let sum: i128 = xs.iter().map(|&x| x as i128).sum();
    Some(variance_from_shifted(
        xs.iter().map(|&x| x as i128),
        sum,
        xs.len(),
        bessel,
    ))
}

/// `sqrt` of [`tree_variance_i32`], taken once at the end — the same
/// relationship `tree_stddev_f32` has to `tree_variance_f32`, so
/// `gpu.stddev(v)` and `gpu.variance(v).sqrt()` are the same number.
pub fn tree_stddev_i32(xs: &[i32], bessel: bool) -> Option<Result<f64, IntFoldOverflow>> {
    tree_variance_i32(xs, bessel).map(|r| r.map(f64::sqrt))
}

/// The unsigned sibling of [`tree_stddev_i32`].
pub fn tree_stddev_u32(xs: &[u32], bessel: bool) -> Option<Result<f64, IntFoldOverflow>> {
    tree_variance_u32(xs, bessel).map(|r| r.map(f64::sqrt))
}

/// The shared exact core: shift by `K = round(sum / n)`, accumulate `Σd` and
/// `Σd²` exactly, and form `(n·Σd² - (Σd)²) / (n · divisor)` with ONE rounding.
///
/// `i128` here stands in for the device's `u64` accumulator plus the host's
/// combining step. The device's `Σd²` is what can overflow — `u64`, not
/// `i128` — so the overflow test is against `u64::MAX` rather than against
/// this type's range, or the twin would accept buffers the device rejects.
fn variance_from_shifted(
    xs: impl Iterator<Item = i128>,
    sum: i128,
    n: usize,
    bessel: bool,
) -> Result<f64, IntFoldOverflow> {
    let n_i = n as i128;
    // Ties away from zero, matching `f64::round` — the runtime computes `K`
    // the same way, and a different `K` would change `Σd²`.
    let k = (sum as f64 / n as f64).round() as i128;
    let mut s1: i128 = 0;
    let mut s2: i128 = 0;
    for x in xs {
        let d = x - k;
        s1 += d;
        s2 += d * d;
        if s2 > u64::MAX as i128 {
            return Err(IntFoldOverflow);
        }
    }
    // `n · Σd² - (Σd)²` is `n²` times the population variance, exactly.
    // Bessel's correction divides by `n - 1` instead of `n`, so it scales the
    // denominator rather than changing the numerator.
    let numerator = n_i * s2 - s1 * s1;
    let divisor = if bessel { n_i - 1 } else { n_i };
    if divisor == 0 {
        // A single element has no sample variance — `n - 1` is zero. The
        // population variance of one element is 0, which the branch above
        // returns; this is only reachable with `bessel`.
        return Ok(f64::NAN);
    }
    Ok(numerator as f64 / (n_i * divisor) as f64)
}

/// The CPU twin of `gpu.stddev(buf)` — the square root of
/// [`tree_variance_f32`], taken once at the end.
///
/// Rooting the finished variance rather than accumulating anything different
/// is what makes `gpu.stddev(v)` and `gpu.variance(v).sqrt()` the same number:
/// there is only one computation, with one extra operation on the way out.
pub fn tree_stddev_f32(xs: &[f32], bessel: bool) -> Option<f32> {
    tree_variance_f32(xs, bessel).map(f32::sqrt)
}

/// The CPU twin of `gpu.mean` over an INTEGER buffer — **the definition of
/// what an integer GPU mean means** (B-2026-08-19-13). `None` iff empty;
/// `Some(Err)` iff the SUM overflows.
///
/// **It promotes rather than truncating**, matching `Stats.mean`, which
/// answers `1.5` for `[1, 2]` rather than `1`. Truncating would be a
/// different function — an integer average — and it would disagree with the
/// CPU on the simplest possible input.
///
/// **But it promotes LATER than `Stats.mean` does, and that is strictly more
/// accurate.** `Stats.mean` converts every element to `f64` and then sums, so
/// it is already lossy above 2^53 for `i64` data. This sums in the integer
/// type — exactly, or trapping — then widens the finished sum, which is
/// lossless (an `i32`/`u32` needs at most 32 bits and `f64` carries 53), and
/// divides once. One correctly-rounded operation on exact inputs is the best
/// answer available.
///
/// The cost of computing exactly is that the SUM has to fit. `[i32::MAX,
/// i32::MAX]` traps here even though its mean is perfectly representable,
/// where `Stats.mean` promotes first and sails through. That is the same
/// trade the whole integer reduction family makes — see
/// [`tree_reduce_i32`] — and the alternative is worse: promoting first on a
/// GPU means promoting to `f32`, whose 24-bit mantissa loses whole integers
/// above 16777216.
pub fn tree_mean_i32(xs: &[i32]) -> Option<Result<f64, IntFoldOverflow>> {
    if xs.is_empty() {
        return None;
    }
    Some(tree_reduce_i32(xs, ReduceOp::Sum)?.map(|sum| sum as f64 / xs.len() as f64))
}

/// The unsigned sibling of [`tree_mean_i32`]. Same promotion rule, same exact
/// widen — `u32::MAX` is far inside `f64`'s exact-integer range.
pub fn tree_mean_u32(xs: &[u32]) -> Option<Result<f64, IntFoldOverflow>> {
    if xs.is_empty() {
        return None;
    }
    Some(tree_reduce_u32(xs, ReduceOp::Sum)?.map(|sum| sum as f64 / xs.len() as f64))
}

/// The CPU twin of `gpu.dot(a, b)` — **the definition of what a GPU dot
/// product MEANS** (B-2026-08-19-13). `None` iff the lengths differ.
///
/// Deliberately expressed as "multiply, then [`tree_reduce_f32`] the
/// products", because that IS the guarantee: `gpu.dot(a, b)` and
/// `gpu.sum(a * b)` are the same number, in the same tree order, to the last
/// bit. The device earns that equality structurally rather than by
/// coincidence — its level-0 shader forms the product on load and then runs
/// the identical halving tree, and every level after that is the ordinary sum
/// shader, so after level 0 the two paths are literally the same computation.
///
/// The fusion is a device-traffic optimization (no `n`-element product buffer
/// is ever written), not a semantic one. Writing the twin as a separate
/// hand-rolled fold would let the two drift apart silently; writing it this
/// way makes drift impossible to express.
pub fn tree_dot_f32(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() {
        return None;
    }
    let products: Vec<f32> = a.iter().zip(b).map(|(x, y)| x * y).collect();
    tree_reduce_f32(&products, ReduceOp::Sum)
}

/// The CPU twin of an INTEGER `gpu.dot(a, b)` (B-2026-08-19-13). `None` iff
/// the lengths differ; `Some(Err)` iff any product or any accumulation
/// overflows.
///
/// **Written as products-then-`tree_reduce_i32`, not as a fused loop**, and
/// that is the specification rather than a convenience: `gpu.dot(a, b)` is
/// `gpu.sum(a * b)` to the last bit, and over integers that promise extends to
/// WHICH PROGRAMS TRAP. Building the twin out of the very functions the
/// equality names makes it true by construction — a fused loop would be a
/// second implementation to keep in step.
///
/// The product is checked BEFORE it reaches the sum, because it can overflow
/// on its own: `65536 * 65536` leaves `i32` in one term, with nothing yet
/// accumulated.
pub fn tree_dot_i32(a: &[i32], b: &[i32]) -> Option<Result<i32, IntFoldOverflow>> {
    if a.len() != b.len() {
        return None;
    }
    let mut products = Vec::with_capacity(a.len());
    for (x, y) in a.iter().zip(b) {
        match x.checked_mul(*y) {
            Some(p) => products.push(p),
            None => return Some(Err(IntFoldOverflow)),
        }
    }
    tree_reduce_i32(&products, ReduceOp::Sum)
}

/// The unsigned sibling of [`tree_dot_i32`]. Same shape; the overflow
/// condition differs only in that an unsigned product overflows on a carry
/// rather than on a sign flip, which `u32::checked_mul` already encodes.
pub fn tree_dot_u32(a: &[u32], b: &[u32]) -> Option<Result<u32, IntFoldOverflow>> {
    if a.len() != b.len() {
        return None;
    }
    let mut products = Vec::with_capacity(a.len());
    for (x, y) in a.iter().zip(b) {
        match x.checked_mul(*y) {
            Some(p) => products.push(p),
            None => return Some(Err(IntFoldOverflow)),
        }
    }
    tree_reduce_u32(&products, ReduceOp::Sum)
}

/// The CPU twin of `gpu.prefix_sum(v)` — **the definition of what a GPU
/// prefix sum MEANS** (B-2026-08-19-13). Inclusive: `out[i]` is the sum of
/// `v[0..=i]`. Empty in, empty out.
///
/// **The first reduction in this family whose result is a BUFFER**, not a
/// scalar, and the first that is not a fold at all — a prefix sum is a
/// different algorithm, which is why the row tracked it as a separate project
/// rather than another combine string.
///
/// INCLUSIVE, because every mainstream prefix sum is: NumPy's `cumsum`, C++'s
/// `partial_sum`, Python's `itertools.accumulate`. Kāra has no CPU prefix sum
/// to match — `scan` is taken, and means the ITERATOR ADAPTER (a stateful
/// map, as in Rust), which is why this is spelled `prefix_sum` rather than
/// reusing that name for a second thing.
///
/// **The order is Hillis-Steele, and it is specified rather than incidental.**
/// Within one [`GPU_REDUCE_WIDTH`] chunk, for stride 1, 2, 4, 8, 16, 32:
/// every lane at or past `stride` adds the lane `stride` below it, all lanes
/// reading the PRE-STEP values (the shader's workgroup barrier — expressed
/// here as the `prev` copy). Past one chunk the chunks are scanned
/// independently, their totals are prefix-summed by this same function one
/// level up, and each chunk's exclusive offset is added back — the recursion
/// the multi-dispatch performs.
///
/// **`prefix_sum(v).last()` need NOT equal `gpu.sum(v)` in f32, and cannot be
/// made to.** Both are the total, but they reach it by different summation
/// orders, and float addition is not associative. For `[a, b, c, d]` the
/// halving tree computes `(a+c) + (b+d)` — its strides run 32…1 over a
/// zero-padded 64-wide scratch — while Hillis-Steele's last lane computes
/// `(a+b) + (c+d)`: the same four values in two different GROUPINGS.
///
/// They agree far more often than not, which is what makes this worth writing
/// down rather than discovering later. Any uniform buffer agrees (both
/// groupings pair equal magnitudes), and so does any buffer whose two
/// partitions happen to pair the same way — measured over random buffers,
/// about 60% differ. No scan algorithm fixes it either: Blelloch's up-sweep
/// does form exactly the tree total, but its down-sweep overwrites the root
/// with the identity before any output is produced, so the total never
/// survives into the result. This is the same class of fact as the
/// multi-workgroup grouping already documented on [`tree_reduce_f32`] —
/// observable in f32, specified, and not a divergence.
///
/// Work-inefficient by design at this slice: Hillis-Steele performs
/// `O(n log n)` adds where Blelloch performs `O(n)`. It is chosen for having
/// a step order that can be written down exactly in one paragraph, which is
/// what the interpreter has to reproduce bit-for-bit. Blelloch is the
/// performance follow-up, and swapping to it CHANGES THE ANSWER in f32 — so
/// it is a semantics change, not an optimization.
pub fn tree_prefix_sum_f32(xs: &[f32]) -> Vec<f32> {
    if xs.is_empty() {
        return Vec::new();
    }
    // Phase 1 — every chunk scanned independently, exactly as one workgroup
    // does, and its total recorded. The zero padding is the Sum identity, so
    // a partial chunk's last lane still holds that chunk's real total.
    let mut out: Vec<f32> = Vec::with_capacity(xs.len());
    let mut totals: Vec<f32> = Vec::with_capacity(xs.len().div_ceil(GPU_REDUCE_WIDTH));
    for chunk in xs.chunks(GPU_REDUCE_WIDTH) {
        let scanned = one_workgroup_scan_f32(chunk);
        totals.push(scanned[GPU_REDUCE_WIDTH - 1]);
        out.extend_from_slice(&scanned[..chunk.len()]);
    }
    if totals.len() == 1 {
        return out;
    }
    // Phase 2 — the chunk totals are themselves prefix-summed, by this same
    // function. Self-similar, like `tree_fold_f32`'s partials.
    let chunk_prefix = tree_prefix_sum_f32(&totals);
    // Phase 3 — each chunk is shifted by the EXCLUSIVE prefix of the totals
    // before it, which is the inclusive prefix one position back. Chunk 0
    // has nothing before it.
    for (c, chunk) in out.chunks_mut(GPU_REDUCE_WIDTH).enumerate().skip(1) {
        let offset = chunk_prefix[c - 1];
        for x in chunk.iter_mut() {
            *x += offset;
        }
    }
    out
}

/// The tile edge of the GPU matmul — the shader's `@workgroup_size(TILE,
/// TILE)`, the side of both its workgroup-memory tiles, and the step of
/// [`tiled_matmul_f32`]'s `k` loop.
///
/// Sixteen, not [`GPU_REDUCE_WIDTH`]'s sixty-four, and the two are unrelated
/// numbers despite both being "the workgroup size". A reduction workgroup is a
/// LINE of 64 lanes; a matmul workgroup is a SQUARE of 16x16 = 256, which is
/// the portable `maxComputeInvocationsPerWorkgroup` floor. A 64x64 tile would
/// ask for 4096 invocations and 32 KiB of workgroup memory, both far past what
/// any baseline device guarantees.
pub const GPU_MATMUL_TILE: usize = 16;

/// The CPU twin of the GPU tiled matmul — **the definition of what
/// `gpu.matmul` MEANS**, in the same sense as [`tree_reduce_f32`].
///
/// `a` is `[m, k]` and `b` is `[k, n]`, both C-order (row-major); the result
/// is `[m, n]`.
///
/// **THE FINDING THIS FUNCTION EXISTS TO RECORD: tiling does not change the
/// answer.** Every other op in this family had to specify a grouping because
/// the GPU's order differs from the obvious CPU one — `gpu.sum` is a halving
/// tree where `v.sum()` is a line, and `gpu.prefix_sum`'s last element is not
/// `gpu.sum` for exactly that reason. A tiled matmul is the exception, and not
/// by luck: tiles are visited in ascending `k`, and within a tile the inner
/// loop runs `p = 0..TILE` in ascending order, so the accumulation order over
/// the whole contraction is `k = 0, 1, 2, ...` — element for element, the
/// order the naive triple loop uses. Tiling changes WHERE THE OPERANDS ARE
/// READ FROM (workgroup memory instead of global), not when they are added.
///
/// So `gpu.matmul(a, b)` is bit-for-bit `a.matmul(b)`, on all three surfaces,
/// and this function is written as the plain triple loop rather than as a
/// simulation of the tiling. Verified against an explicit tile-by-tile
/// simulation over 300 random shapes straddling the tile edge — see
/// `matmul_tiling_matches_naive_order` in the tests below, which is the
/// property, not an example of it.
///
/// **The zero padding is what keeps that true at a ragged edge**, and it is
/// only safe because BOTH tiles are padded at the same `k`. A thread whose
/// `k` is past the contraction reads `0.0` from the A tile and `0.0` from the
/// B tile, contributing `0.0 * 0.0`. Padding only one side would let a real
/// value meet a padded zero — and if that real value were an infinity, `inf *
/// 0.0` is NaN, which would poison an output element that has no business
/// being NaN. The accumulator itself cannot be `-0.0` (it starts at `+0.0`
/// and `0.0 + -0.0` is `+0.0`), so adding padded zeros is a true no-op rather
/// than an approximate one.
///
/// **f32 accumulation, rounding at every step**, matching codegen's triple
/// loop over the element LLVM type and the interpreter as of B-2026-08-20-21.
/// This is forced rather than chosen: WGSL has no f64, so a wider accumulator
/// would put the GPU permanently out of reach of its own CPU twin.
///
/// `None` when the inner dimensions disagree — the one shape error a caller
/// can make that no amount of checking upstream can turn into a meaningful
/// product.
pub fn tiled_matmul_f32(a: &[f32], b: &[f32], m: usize, k: usize, n: usize) -> Option<Vec<f32>> {
    if a.len() != m * k || b.len() != k * n {
        return None;
    }
    let mut out = vec![0.0f32; m * n];
    for i in 0..m {
        for j in 0..n {
            let mut acc = 0.0f32;
            for p in 0..k {
                acc += a[i * k + p] * b[p * n + j];
            }
            out[i * n + j] = acc;
        }
    }
    Some(out)
}

/// The CPU twin of an INTEGER tiled matmul — **the definition of what
/// `gpu.matmul(Tensor[i32])` MEANS**, including which programs trap
/// (B-2026-08-19-13). `None` on a shape mismatch; `Some(Err)` on overflow.
///
/// **Equal to `a.matmul(b)`, trap for trap.** The float form's promise was
/// that tiling does not change the accumulation ORDER; the integer form
/// inherits that and adds a second, sharper consequence: because the order is
/// identical, the set of intermediate values is identical, so the two agree
/// about WHICH CONTRACTIONS OVERFLOW as well as what they return. Reordering
/// the tile loop would break that even where it preserved the final value —
/// overflow is a property of the intermediates, exactly as
/// [`tree_reduce_i32`] records for the reductions.
///
/// Both the product and the accumulation are checked, because either can
/// overflow alone: `65536 * 65536` leaves `i32` in a single term.
///
/// `unsigned` selects the range, not a different algorithm — the arithmetic is
/// the same, and only the bound each intermediate is tested against differs.
pub fn tiled_matmul_int(
    a: &[i64],
    b: &[i64],
    m: usize,
    k: usize,
    n: usize,
    unsigned: bool,
) -> Option<Result<Vec<i64>, IntFoldOverflow>> {
    if a.len() != m * k || b.len() != k * n {
        return None;
    }
    let (lo, hi) = if unsigned {
        (0i128, u32::MAX as i128)
    } else {
        (i32::MIN as i128, i32::MAX as i128)
    };
    let mut out = vec![0i64; m * n];
    for i in 0..m {
        for j in 0..n {
            let mut acc: i128 = 0;
            for p in 0..k {
                let prod = a[i * k + p] as i128 * b[p * n + j] as i128;
                if prod < lo || prod > hi {
                    return Some(Err(IntFoldOverflow));
                }
                acc += prod;
                if acc < lo || acc > hi {
                    return Some(Err(IntFoldOverflow));
                }
            }
            out[i * n + j] = acc as i64;
        }
    }
    Some(Ok(out))
}

/// The CPU twin of an INTEGER `gpu.prefix_sum` — **the definition of what
/// `gpu.prefix_sum(Vec[i32])` MEANS, including which programs trap**
/// (B-2026-08-19-13).
///
/// Same Hillis-Steele order as [`tree_prefix_sum_f32`], same three phases,
/// same self-similar recursion. The one thing that genuinely differs is where
/// overflow can be observed, and it is the reason this was tracked separately
/// rather than folded into the float scan with a different combine:
///
/// **EVERY LANE HOLDS A LIVE OUTPUT, so every lane's overflow counts.** In a
/// reduction only lane 0 survives — a lane above the stride holds a partial
/// nobody reads, and its overflow is irrelevant. A scan writes all `n` values,
/// so an overflow anywhere is an overflow in the ANSWER. A checked scan that
/// reused the reduction's "flag lane 0" habit would silently drop overflows in
/// the elements the caller actually asked for, which is the exact
/// plausible-wrong-number shape this family exists to refuse.
///
/// **The PADDING lanes are not an exception, and must not be excluded.** A
/// lane past the chunk's length starts at the identity, but the scan sweeps
/// real values into it, so by the last step it holds the CHUNK TOTAL — which
/// feeds phase 2 and therefore every later chunk's offset. Its overflow is a
/// real overflow of a real quantity, just not one written to `out` directly.
///
/// Returns `Result`, not `Option`: the prefix sums of an empty buffer are the
/// empty buffer, so there is no missing answer to report — only a possible
/// trap. That asymmetry with the reductions is the same one the float scan
/// has, for the same reason.
pub fn tree_prefix_sum_i32(xs: &[i32]) -> Result<Vec<i32>, IntFoldOverflow> {
    prefix_sum_checked(xs, |a, b| a.checked_add(b))
}

/// The unsigned sibling of [`tree_prefix_sum_i32`]. The order is identical;
/// only the overflow condition differs (a carry rather than a sign flip),
/// which `u32::checked_add` already encodes.
pub fn tree_prefix_sum_u32(xs: &[u32]) -> Result<Vec<u32>, IntFoldOverflow> {
    prefix_sum_checked(xs, |a, b| a.checked_add(b))
}

/// The shared three-phase scan, generic over the checked add.
///
/// Phase 2 is this same function one level up over the chunk totals — a long
/// prefix sum is a prefix sum OF PREFIX SUMS, exactly as in the float twin, so
/// an overflow at any level propagates by returning rather than by a flag the
/// caller has to remember to test.
fn prefix_sum_checked<T: Copy + Default>(
    xs: &[T],
    add: fn(T, T) -> Option<T>,
) -> Result<Vec<T>, IntFoldOverflow> {
    if xs.is_empty() {
        return Ok(Vec::new());
    }
    let mut out: Vec<T> = Vec::with_capacity(xs.len());
    let mut totals: Vec<T> = Vec::with_capacity(xs.len().div_ceil(GPU_REDUCE_WIDTH));
    for chunk in xs.chunks(GPU_REDUCE_WIDTH) {
        let scanned = one_workgroup_scan_checked(chunk, add)?;
        totals.push(scanned[GPU_REDUCE_WIDTH - 1]);
        out.extend_from_slice(&scanned[..chunk.len()]);
    }
    if totals.len() == 1 {
        return Ok(out);
    }
    let chunk_prefix = prefix_sum_checked(&totals, add)?;
    // Phase 3 — the offset add is checked too. It is the step most easily
    // forgotten: phases 1 and 2 look like "the arithmetic", and this one looks
    // like bookkeeping, but `scanned[i] + offset` is an ordinary addition of
    // two real values and overflows exactly as readily.
    for (c, chunk) in out.chunks_mut(GPU_REDUCE_WIDTH).enumerate().skip(1) {
        let offset = chunk_prefix[c - 1];
        for x in chunk.iter_mut() {
            *x = add(*x, offset).ok_or(IntFoldOverflow)?;
        }
    }
    Ok(out)
}

/// One workgroup's inclusive Hillis-Steele scan with a checked add, padded to
/// [`GPU_REDUCE_WIDTH`] with the additive identity.
///
/// The `prev` copy is the shader's `workgroupBarrier()`, exactly as in
/// [`one_workgroup_scan_f32`] — every lane must read the values as they stood
/// BEFORE this step, or a low lane's new value feeds a high lane within the
/// same step and double-counts.
///
/// Overflow is checked for EVERY lane at EVERY step, not only for the lane
/// that ends up holding a written output: an intermediate that overflows has
/// already destroyed the values that depend on it.
fn one_workgroup_scan_checked<T: Copy + Default>(
    xs: &[T],
    add: fn(T, T) -> Option<T>,
) -> Result<[T; GPU_REDUCE_WIDTH], IntFoldOverflow> {
    let mut s = [T::default(); GPU_REDUCE_WIDTH];
    for (slot, &x) in s.iter_mut().zip(xs) {
        *slot = x;
    }
    let mut stride = 1;
    while stride < GPU_REDUCE_WIDTH {
        let prev = s;
        for t in stride..GPU_REDUCE_WIDTH {
            s[t] = add(prev[t], prev[t - stride]).ok_or(IntFoldOverflow)?;
        }
        stride *= 2;
    }
    Ok(s)
}

/// One workgroup's inclusive Hillis-Steele scan, zero-padded to
/// [`GPU_REDUCE_WIDTH`]. The full width is returned because the caller wants
/// both the scanned prefix AND the last lane, which is the chunk total.
///
/// The `prev` copy is load-bearing and is not a Rust artifact: it is the
/// shader's `workgroupBarrier()`. Every lane must read the values as they
/// stood BEFORE this step. Updating in place would let a low lane's new value
/// feed a high lane in the same step, double-counting — and the result would
/// still look like a plausible prefix sum.
fn one_workgroup_scan_f32(xs: &[f32]) -> [f32; GPU_REDUCE_WIDTH] {
    let mut s = [0.0f32; GPU_REDUCE_WIDTH];
    for (slot, &x) in s.iter_mut().zip(xs) {
        *slot = x;
    }
    let mut stride = 1;
    while stride < GPU_REDUCE_WIDTH {
        let prev = s;
        for t in stride..GPU_REDUCE_WIDTH {
            s[t] = prev[t] + prev[t - stride];
        }
        stride *= 2;
    }
    s
}

/// One GPU-expressible reduction as data: its combining function and the
/// identity that pads a short chunk.
type Combiner = (fn(f32, f32) -> f32, f32);

/// The [`Combiner`] for a GPU-expressible reduction, or `None` for the ops
/// that need more than one associative pass.
///
/// **`Min`/`Max` are NaN-IGNORING**, matching `f32::min` / `f32::max` and the
/// f64 twin at [`ReduceOp::Min`] — `min(x, NaN) == min(NaN, x) == x`. That is
/// not a detail: a NaN-PROPAGATING min is fine in a left fold but breaks a
/// tree, because whether a NaN meets a real value early or late changes the
/// answer. NaN-ignoring min is associative, so every grouping agrees, which is
/// the property this whole family is built on. The emitted shader spells the
/// NaN guard out by hand rather than calling WGSL's `min` builtin, whose
/// tie-break on NaN is positional (`min(e1, e2)` returns `e1` unless
/// `e2 < e1`, and every comparison against NaN is false) and would therefore
/// disagree with this in one of the two argument orders.
///
/// The padding identity is ±∞ rather than `f32::MAX`, so a chunk shorter than
/// the workgroup width cannot have its padding win over a real element — even
/// one as large as `f32::MAX`.
fn reduce_combiner_f32(op: ReduceOp) -> Option<Combiner> {
    match op {
        ReduceOp::Sum => Some((|a, b| a + b, 0.0)),
        ReduceOp::Prod => Some((|a, b| a * b, 1.0)),
        ReduceOp::Min => Some((f32::min, f32::INFINITY)),
        ReduceOp::Max => Some((f32::max, f32::NEG_INFINITY)),
        _ => None,
    }
}

/// The recursion the multi-workgroup dispatch performs, reproduced exactly.
///
/// Each workgroup reduces its own [`GPU_REDUCE_WIDTH`]-wide chunk to one
/// partial (`output[workgroup_id]`), and the host then folds the partials by
/// dispatching the SAME shader over them — so a long buffer is a TREE OF
/// TREES, and the shape of that tree is fixed by the chunking. Reproducing it
/// here is what keeps a 4096-element `gpu.sum` bit-identical between
/// `karac run` and `karac build`, exactly as the single-workgroup case is.
///
/// Note this is not the same number a flat 4096-wide tree would give, nor a
/// left fold: the grouping is observable in f32. That is fine — it is
/// *specified* — but it does mean the width is part of the language's answer,
/// which is why [`GPU_REDUCE_WIDTH`] lives here rather than in the emitter.
fn tree_fold_f32(xs: &[f32], combine: fn(f32, f32) -> f32, identity: f32) -> f32 {
    if xs.len() <= GPU_REDUCE_WIDTH {
        return one_workgroup_f32(xs, combine, identity);
    }
    let partials: Vec<f32> = xs
        .chunks(GPU_REDUCE_WIDTH)
        .map(|c| one_workgroup_f32(c, combine, identity))
        .collect();
    tree_fold_f32(&partials, combine, identity)
}

/// One workgroup's halving tree: pad to [`GPU_REDUCE_WIDTH`] with `identity`,
/// then `s[t] = s[t] OP s[t + stride]` for stride 32, 16, 8, 4, 2, 1.
fn one_workgroup_f32(xs: &[f32], combine: fn(f32, f32) -> f32, identity: f32) -> f32 {
    debug_assert!(xs.len() <= GPU_REDUCE_WIDTH);
    let mut scratch = [identity; GPU_REDUCE_WIDTH];
    for (slot, &x) in scratch.iter_mut().zip(xs) {
        *slot = x;
    }
    let mut stride = GPU_REDUCE_WIDTH / 2;
    while stride > 0 {
        for t in 0..stride {
            scratch[t] = combine(scratch[t], scratch[t + stride]);
        }
        stride /= 2;
    }
    scratch[0]
}

/// A statistical reduction, independent of container shape, element source
/// (contiguous / Arrow-nullable / slice), and backend.
///
/// The S6 surface traits will partition these into `Reduce`
/// (`Sum`/`Prod`/`Mean`/`Var`/`Std`) and `ElementwiseOrd`
/// (`Min`/`Max`/`Argmin`/`Argmax`/`Median`/`Sort`/`Argsort`); they share one
/// enum here because the interpreter dispatches them through one match.
/// `Quantile`/`Percentile` are *not* variants — they need a caller-computed
/// fractional position and go through [`quantile_linear_sorted`] directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReduceOp {
    /// Σ xᵢ. Empty → `0.0` (the additive identity; never traps).
    Sum,
    /// Π xᵢ. Empty → `1.0` (the multiplicative identity; never traps).
    Prod,
    /// Arithmetic mean. The caller guards emptiness (division by zero).
    Mean,
    /// Variance. `bessel` selects the **sample** (÷ n−1) form over the
    /// **population** (÷ n) form. The caller guards the required minimum
    /// count (n ≥ 1 population, n ≥ 2 sample).
    Var { bessel: bool },
    /// Standard deviation — `sqrt` of [`ReduceOp::Var`] with the same knob.
    Std { bessel: bool },
    /// Minimum (first on tie). Empty → `None`. NaN compares false against
    /// everything, so it neither displaces nor is taken (the scalar `<`
    /// posture, matching `f64::min`).
    Min,
    /// Maximum (first on tie). Empty → `None`.
    Max,
    /// Index of the first minimum. Empty → `None`.
    Argmin,
    /// Index of the first maximum. Empty → `None`.
    Argmax,
    /// Median (middle element, or mean of the two middle elements). The
    /// caller guards emptiness.
    Median,
    /// A fresh ascending copy of the input (the source is left unchanged).
    Sort,
    /// The indices that sort the input ascending — stable (ties keep input
    /// order).
    Argsort,
}

/// The result of [`reduce_f64`] / [`reduce_i64`], shaped by the op (and, for
/// the element-typed ops, the element kind — S5). The interpreter maps each
/// variant onto its `Value` representation (bare float/int, `Option[f64]`/
/// `Option[i64]`, `Vec[f64]`, `Vec[i64]`).
#[derive(Debug, Clone, PartialEq)]
pub enum ReduceOutcome {
    /// `Sum`, `Prod`, `Mean`, `Var`, `Std`, `Median` over f64 elements —
    /// plus the always-f64 forms (`Mean`/`Var`/`Std`/`Median`) over i64
    /// elements (integer statistics promote to float).
    Scalar(f64),
    /// `Sum`, `Prod` over i64 elements (S5) — the element-typed folds.
    IntScalar(i64),
    /// `Min`, `Max` over f64 elements — `None` iff the input was empty.
    OptScalar(Option<f64>),
    /// `Min`, `Max` over i64 elements (S5) — `None` iff the input was empty.
    OptIntScalar(Option<i64>),
    /// `Argmin`, `Argmax` — `None` iff the input was empty.
    OptIndex(Option<i64>),
    /// `Sort` over f64 elements.
    F64Vec(Vec<f64>),
    /// `Argsort` — and `Sort` over i64 elements (S5).
    I64Vec(Vec<i64>),
}

/// Evaluate `op` over `xs` for the interpreter. For the ops with an identity
/// (`Sum`/`Prod`) or an `Option`/collection result (`Min`/`Max`/`Argmin`/
/// `Argmax`/`Sort`/`Argsort`) an empty `xs` is well-defined; for
/// `Mean`/`Var`/`Std`/`Median` the **caller** must guarantee a non-empty
/// (and, for the sample `Var`/`Std`, ≥ 2-element) input — those forms would
/// divide by zero otherwise and each surface traps with its own message and
/// mechanism (`Stats.*` panics, `Column`/`Tensor` record a runtime error).
pub fn reduce_f64(xs: &[f64], op: ReduceOp) -> ReduceOutcome {
    match op {
        ReduceOp::Sum => ReduceOutcome::Scalar(xs.iter().sum()),
        ReduceOp::Prod => ReduceOutcome::Scalar(xs.iter().product()),
        ReduceOp::Mean => ReduceOutcome::Scalar(mean_f64(xs)),
        ReduceOp::Var { bessel } => ReduceOutcome::Scalar(variance_f64(xs, bessel)),
        ReduceOp::Std { bessel } => ReduceOutcome::Scalar(variance_f64(xs, bessel).sqrt()),
        ReduceOp::Min => ReduceOutcome::OptScalar(xs.iter().copied().reduce(f64::min)),
        ReduceOp::Max => ReduceOutcome::OptScalar(xs.iter().copied().reduce(f64::max)),
        ReduceOp::Argmin => ReduceOutcome::OptIndex(arg_extreme(xs, false)),
        ReduceOp::Argmax => ReduceOutcome::OptIndex(arg_extreme(xs, true)),
        ReduceOp::Median => ReduceOutcome::Scalar(median_f64(xs)),
        ReduceOp::Sort => ReduceOutcome::F64Vec(sorted_ascending(xs)),
        ReduceOp::Argsort => ReduceOutcome::I64Vec(argsorted_ascending(xs)),
    }
}

/// Evaluate `op` over an **i64** slice (S5 — the non-f64 element axis for
/// `Stats.*` over `Slice[i64]`/`Vec[i64]`). The genuinely-int ops stay exact
/// at all magnitudes: `Sum`/`Prod` are **checked** folds (`Err` on overflow —
/// the caller traps with the scalar `integer overflow` message, matching the
/// `+`/`*` operators and codegen's `compile_binop_typed` fold),
/// `Min`/`Max`/`Argmin`/`Argmax`/`Sort`/`Argsort` compare at i64 (no lossy
/// float round-trip above 2⁵³). The always-f64 statistics (`Mean`/`Var`/
/// `Std`) convert each element to f64 and delegate to [`reduce_f64`] — the
/// same per-element `sitofp`-then-accumulate order codegen emits, so the
/// rounding agrees. `Median` sorts exactly at i64, then converts only the
/// middle element(s) for the (possibly fractional) result. Empty policy
/// mirrors [`reduce_f64`] except the identities are integer: empty `Sum` →
/// `0`, empty `Prod` → `1`.
pub fn reduce_i64(xs: &[i64], op: ReduceOp) -> Result<ReduceOutcome, IntFoldOverflow> {
    Ok(match op {
        ReduceOp::Sum => ReduceOutcome::IntScalar(
            xs.iter()
                .try_fold(0i64, |a, &x| a.checked_add(x))
                .ok_or(IntFoldOverflow)?,
        ),
        ReduceOp::Prod => ReduceOutcome::IntScalar(
            xs.iter()
                .try_fold(1i64, |a, &x| a.checked_mul(x))
                .ok_or(IntFoldOverflow)?,
        ),
        ReduceOp::Mean | ReduceOp::Var { .. } | ReduceOp::Std { .. } => {
            let as_f64: Vec<f64> = xs.iter().map(|&x| x as f64).collect();
            reduce_f64(&as_f64, op)
        }
        ReduceOp::Min => ReduceOutcome::OptIntScalar(xs.iter().copied().min()),
        ReduceOp::Max => ReduceOutcome::OptIntScalar(xs.iter().copied().max()),
        ReduceOp::Argmin => ReduceOutcome::OptIndex(arg_extreme_i64(xs, false)),
        ReduceOp::Argmax => ReduceOutcome::OptIndex(arg_extreme_i64(xs, true)),
        ReduceOp::Median => {
            let mut sorted = xs.to_vec();
            sorted.sort_unstable();
            let n = sorted.len();
            ReduceOutcome::Scalar(if n.is_multiple_of(2) {
                (sorted[n / 2 - 1] as f64 + sorted[n / 2] as f64) / 2.0
            } else {
                sorted[n / 2] as f64
            })
        }
        ReduceOp::Sort => {
            let mut sorted = xs.to_vec();
            sorted.sort_unstable();
            ReduceOutcome::I64Vec(sorted)
        }
        ReduceOp::Argsort => {
            let mut idx: Vec<usize> = (0..xs.len()).collect();
            idx.sort_by_key(|&i| xs[i]);
            ReduceOutcome::I64Vec(idx.into_iter().map(|i| i as i64).collect())
        }
    })
}

/// A checked `Sum`/`Prod` fold overflowed — the caller traps with the scalar
/// `integer overflow` message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntFoldOverflow;

/// The index of the first max (`want_max`) / first min at exact i64
/// precision; `None` for empty. Strict comparison keeps the earliest
/// occurrence on a tie.
fn arg_extreme_i64(xs: &[i64], want_max: bool) -> Option<i64> {
    let mut best: Option<usize> = None;
    for (i, &x) in xs.iter().enumerate() {
        match best {
            None => best = Some(i),
            Some(b) => {
                let take = if want_max { x > xs[b] } else { x < xs[b] };
                if take {
                    best = Some(i);
                }
            }
        }
    }
    best.map(|i| i as i64)
}

/// Linear-interpolated order statistic of an **already-ascending-sorted**,
/// non-empty **i64** slice at fractional position `pos ∈ [0, n−1]` — the
/// integer-element twin of [`quantile_linear_sorted`]: the sort stayed exact
/// at i64, and only the two picked ranks convert to f64 for interpolation.
pub fn quantile_linear_sorted_i64(sorted: &[i64], pos: f64) -> f64 {
    let lo = pos.floor() as usize;
    let hi = if lo + 1 < sorted.len() { lo + 1 } else { lo };
    let frac = pos - lo as f64;
    sorted[lo] as f64 + frac * (sorted[hi] as f64 - sorted[lo] as f64)
}

/// The arithmetic mean of a non-empty slice.
fn mean_f64(xs: &[f64]) -> f64 {
    xs.iter().sum::<f64>() / xs.len() as f64
}

/// Variance of a non-empty slice: Σ(xᵢ − mean)² ÷ denom, where denom is
/// `n − 1` (sample, `bessel`) or `n` (population). The sample form requires
/// n ≥ 2 (guarded by the caller).
fn variance_f64(xs: &[f64], bessel: bool) -> f64 {
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    let ss: f64 = xs
        .iter()
        .map(|x| {
            let d = x - mean;
            d * d
        })
        .sum();
    ss / if bessel { n - 1.0 } else { n }
}

/// Median of a non-empty slice — the middle element (odd length) or the mean
/// of the two middle elements (even length), after an ascending sort.
fn median_f64(xs: &[f64]) -> f64 {
    let sorted = sorted_ascending(xs);
    let n = sorted.len();
    if n.is_multiple_of(2) {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    } else {
        sorted[n / 2]
    }
}

/// The index of the first max (`want_max`) or first min of a slice; `None`
/// for an empty slice. Strict comparison keeps the earliest occurrence on a
/// tie; NaN compares false, so it is never selected over a real value.
fn arg_extreme(xs: &[f64], want_max: bool) -> Option<i64> {
    let mut best: Option<usize> = None;
    for (i, &x) in xs.iter().enumerate() {
        match best {
            None => best = Some(i),
            Some(b) => {
                let take = if want_max { x > xs[b] } else { x < xs[b] };
                if take {
                    best = Some(i);
                }
            }
        }
    }
    best.map(|i| i as i64)
}

/// A fresh ascending copy (total order via `partial_cmp`, NaN treated as
/// equal so the sort is well-defined).
fn sorted_ascending(xs: &[f64]) -> Vec<f64> {
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v
}

/// The indices that sort `xs` ascending — stable (ties keep input order).
fn argsorted_ascending(xs: &[f64]) -> Vec<i64> {
    let mut idx: Vec<usize> = (0..xs.len()).collect();
    idx.sort_by(|&a, &b| {
        xs[a]
            .partial_cmp(&xs[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    idx.into_iter().map(|i| i as i64).collect()
}

/// Linear-interpolated order statistic of an **already-ascending-sorted**,
/// non-empty slice at fractional position `pos ∈ [0, n−1]` (NumPy/pandas
/// default `'linear'` method). Callers map their range onto `pos`:
/// `Stats.percentile` uses `p ∈ [0, 100] → (p/100)·(n−1)`, and
/// `Column.quantile` uses `q ∈ [0, 1] → q·(n−1)`.
pub fn quantile_linear_sorted(sorted: &[f64], pos: f64) -> f64 {
    let lo = pos.floor() as usize;
    let hi = if lo + 1 < sorted.len() { lo + 1 } else { lo };
    let frac = pos - lo as f64;
    sorted[lo] + frac * (sorted[hi] - sorted[lo])
}

#[cfg(test)]
mod tests {
    // ── GPU tree reduction (B-2026-08-19-10) ────────────────────────────

    #[test]
    fn tree_reduce_matches_the_shader_order_not_a_left_fold() {
        // The whole reason this function exists. 64 copies of 0.1: a left fold
        // drifts to 6.399996, the tree gives 6.400000, and the GPU computes the
        // tree — so the interpreter must too, or `karac run` and `karac build`
        // print different numbers for the same program.
        let xs = [0.1f32; 64];
        let tree = tree_reduce_f32(&xs, ReduceOp::Sum).unwrap();
        let left: f32 = xs.iter().fold(0.0, |a, b| a + b);
        assert_ne!(tree, left, "0.1 x 64 must expose the order difference");
        // Pinned against the value measured on lavapipe.
        assert_eq!(tree.to_bits(), 6.4f32.to_bits(), "tree sum of 0.1 x 64");
        // And the tree is the more accurate of the two.
        assert!((tree - 6.4).abs() < (left - 6.4).abs());
    }

    #[test]
    fn tree_reduce_pads_with_the_identity_per_op() {
        // A short buffer is padded to the workgroup width, so the identity has
        // to be the operation's own: padding a product with 0.0 would return 0
        // for every input shorter than 64.
        assert_eq!(tree_reduce_f32(&[1.0, 2.0, 3.0], ReduceOp::Sum), Some(6.0));
        assert_eq!(
            tree_reduce_f32(&[2.0, 3.0, 4.0], ReduceOp::Prod),
            Some(24.0)
        );
        // Empty reduces to the identity, not to None.
        assert_eq!(tree_reduce_f32(&[], ReduceOp::Sum), Some(0.0));
        assert_eq!(tree_reduce_f32(&[], ReduceOp::Prod), Some(1.0));
    }

    #[test]
    fn tree_reduce_refuses_only_the_multipass_ops() {
        // Length is no longer a limit — a buffer past one workgroup folds
        // through the multi-dispatch recursion instead of being refused.
        let long = vec![1.0f32; GPU_REDUCE_WIDTH + 1];
        assert_eq!(tree_reduce_f32(&long, ReduceOp::Sum), Some(65.0));
        // What IS still refused: the same set the emitter refuses, so the two
        // cannot disagree about which ops exist. `Mean` needs a count
        // division, `Var`/`Std` two passes, the Arg family an index carried
        // alongside the value.
        assert_eq!(tree_reduce_f32(&[1.0], ReduceOp::Mean), None);
        assert_eq!(tree_reduce_f32(&[1.0], ReduceOp::Argmin), None);
        assert_eq!(tree_reduce_f32(&[1.0], ReduceOp::Median), None);
    }

    #[test]
    fn tree_i32_traps_on_overflow_rather_than_wrapping() {
        // Integer reductions trap, matching `v.sum()` over a `Vec[i32]`, which
        // already fails with `integer overflow` on both surfaces. Wrapping
        // would turn a trap into a wrong answer the moment a reduction moved
        // to the GPU.
        assert_eq!(
            tree_reduce_i32(&[i32::MAX, 1], ReduceOp::Sum),
            Some(Err(IntFoldOverflow))
        );
        assert_eq!(
            tree_reduce_i32(&[i32::MAX, 2], ReduceOp::Prod),
            Some(Err(IntFoldOverflow))
        );
        assert_eq!(tree_reduce_i32(&[3, 1, 2], ReduceOp::Sum), Some(Ok(6)));
        assert_eq!(tree_reduce_i32(&[3, 1, 2], ReduceOp::Prod), Some(Ok(6)));
        // Integer identities on empty, not the float ones.
        assert_eq!(tree_reduce_i32(&[], ReduceOp::Sum), Some(Ok(0)));
        assert_eq!(tree_reduce_i32(&[], ReduceOp::Prod), Some(Ok(1)));
    }

    #[test]
    fn tree_order_decides_whether_an_integer_reduction_traps() {
        // THE consequence of specifying the order, and the reason it is in
        // design.md rather than only in a comment. Overflow is a property of
        // the INTERMEDIATE sums, and a tree forms different intermediates than
        // a line — so `gpu.sum(v)` and `v.sum()` can legitimately disagree
        // about whether they fail on the same buffer. Both directions are
        // reachable, which is what makes it a real semantic difference rather
        // than the tree merely being more forgiving.
        const MAX: i32 = i32::MAX;
        let left_fold = |xs: &[i32]| xs.iter().try_fold(0i32, |a, &x| a.checked_add(x));

        // Tree survives, left fold overflows: the tree pairs MAX with -MAX
        // before it ever pairs MAX with MAX.
        let a = [MAX, MAX, -MAX, -MAX];
        assert_eq!(tree_reduce_i32(&a, ReduceOp::Sum), Some(Ok(0)));
        assert_eq!(left_fold(&a), None, "left fold must overflow on {a:?}");

        // And the other way: the tree pairs MAX with MAX at stride 2, where
        // the left fold had already cancelled them.
        let b = [MAX, -MAX, MAX, -MAX];
        assert_eq!(
            tree_reduce_i32(&b, ReduceOp::Sum),
            Some(Err(IntFoldOverflow))
        );
        assert_eq!(left_fold(&b), Some(0), "left fold must survive {b:?}");
    }

    #[test]
    fn tree_arg_takes_the_first_occurrence_on_a_tie() {
        // Matches `Stats.argmin`'s first-occurrence rule, and that rule is
        // what makes the combine a proper order rather than a coin flip: a
        // tie broken by "whichever side the grouping happened to put first"
        // would give different answers at different buffer lengths.
        assert_eq!(tree_arg_f32(&[3.0, 1.0, 1.0, 5.0], false), Some(1));
        assert_eq!(tree_arg_f32(&[3.0, 5.0, 5.0], true), Some(1));
        assert_eq!(tree_arg_f32(&[7.0], false), Some(0));
        assert_eq!(tree_arg_f32(&[], false), None);
        assert_eq!(tree_arg_f32(&[], true), None);
    }

    #[test]
    fn tree_arg_is_grouping_independent() {
        // The property the lexicographic combine buys, and the one `sum`
        // cannot have: the same answer at every length, across one workgroup,
        // a partial chunk, and two full fold levels. A tie-break that
        // depended on scratch position rather than absolute index would drift
        // here.
        for n in [1usize, 63, 64, 65, 128, 4096] {
            let xs: Vec<f32> = (0..n).map(|i| ((i * 37) % 101) as f32).collect();
            let want_min = xs
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
                .map(|(i, _)| i as i64);
            let want_max = xs
                .iter()
                .enumerate()
                .max_by(|(ia, a), (ib, b)| a.partial_cmp(b).unwrap().then(ib.cmp(ia)))
                .map(|(i, _)| i as i64);
            assert_eq!(tree_arg_f32(&xs, false), want_min, "argmin n={n}");
            assert_eq!(tree_arg_f32(&xs, true), want_max, "argmax n={n}");
        }
    }

    #[test]
    fn tree_arg_integer_orders_by_the_element_type_not_the_bits() {
        // Above 2^31 the signed and unsigned orders disagree, and they
        // disagree on BOTH ends: `4294967295` is the largest u32 and `-1` as
        // i32. So a signed compare on unsigned data answers argmin AND argmax
        // backwards — which is why these are two functions rather than one
        // over the raw words.
        assert_eq!(tree_arg_u32(&[u32::MAX, 1], true), Some(0));
        assert_eq!(tree_arg_u32(&[u32::MAX, 1], false), Some(1));
        let as_signed = [-1i32, 1];
        assert_eq!(tree_arg_i32(&as_signed, true), Some(1));
        assert_eq!(tree_arg_i32(&as_signed, false), Some(0));

        // Signed negatives order below zero.
        assert_eq!(tree_arg_i32(&[5, -7, 2], false), Some(1));
        assert_eq!(tree_arg_i32(&[5, -7, 2], true), Some(0));
    }

    #[test]
    fn tree_arg_integer_keeps_every_rule_except_the_nan_one() {
        // Ties take the first occurrence, empty is None, padding never wins —
        // the integer arms differ from the float one ONLY in dropping the NaN
        // rules, which have nothing to bite on.
        assert_eq!(tree_arg_i32(&[3, 1, 1, 5], false), Some(1));
        assert_eq!(tree_arg_u32(&[3, 5, 5], true), Some(1));
        assert_eq!(tree_arg_i32(&[], false), None);
        assert_eq!(tree_arg_u32(&[], true), None);

        // 65 elements: the winner sits alone in a chunk that is 63/64 padding.
        let mut xs = vec![5i32; 65];
        xs[64] = -3;
        assert_eq!(tree_arg_i32(&xs, false), Some(64));

        // Grouping-independent at every length, like the float arm.
        for n in [1usize, 63, 64, 65, 128, 4096] {
            let xs: Vec<i32> = (0..n).map(|i| ((i * 37) % 101) as i32 - 50).collect();
            let want = xs
                .iter()
                .enumerate()
                .min_by_key(|(i, &v)| (v, *i))
                .map(|(i, _)| i as i64);
            assert_eq!(tree_arg_i32(&xs, false), want, "n={n}");
        }
    }

    #[test]
    fn tree_arg_makes_nan_always_lose() {
        // DIFFERS FROM `Stats.argmin`, deliberately. That one seeds its best
        // with element 0 and displaces only on a strict comparison, so a
        // LEADING NaN is never displaced — `Stats.argmin([NaN, 3.0, 1.0])` is
        // 0 while `[3.0, 1.0, NaN]` is 1. Position-dependence cannot survive a
        // halving tree, where the grouping decides the positions. Making NaN
        // always lose restores associativity.
        assert_eq!(tree_arg_f32(&[f32::NAN, 3.0, 1.0], false), Some(2));
        assert_eq!(tree_arg_f32(&[3.0, 1.0, f32::NAN], false), Some(1));
        assert_eq!(tree_arg_f32(&[f32::NAN, 3.0, 1.0], true), Some(1));

        // All-NaN: nothing ever wins, so the leftmost survives.
        assert_eq!(tree_arg_f32(&[f32::NAN; 3], false), Some(0));
        assert_eq!(tree_arg_f32(&[f32::NAN; 3], true), Some(0));

        // And a NaN past the first workgroup still loses — the padding rule
        // and the NaN rule are independent.
        let mut xs = vec![5.0f32; 200];
        xs[70] = f32::NAN;
        xs[150] = -1.0;
        assert_eq!(tree_arg_f32(&xs, false), Some(150));
    }

    #[test]
    fn tree_arg_padding_never_wins() {
        // A padded slot is marked by the INDEX sentinel, not by a value, so it
        // loses regardless of what a device leaves in the value slot. 65
        // elements is one full workgroup plus a chunk of one, so the second
        // chunk is 63/64 padding — if padding could win, the answer would come
        // back as the sentinel rather than an index.
        let mut xs = vec![5.0f32; 65];
        xs[64] = -3.0;
        assert_eq!(tree_arg_f32(&xs, false), Some(64));
        let mut xs = vec![5.0f32; 65];
        xs[64] = 9.0;
        assert_eq!(tree_arg_f32(&xs, true), Some(64));
        // The sentinel is never a legal answer.
        assert_ne!(tree_arg_f32(&xs, true), Some(ARG_INVALID as i64));
    }

    #[test]
    fn tree_variance_matches_the_textbook_population_form() {
        // `Stats.variance` / `Stats.stddev` are POPULATION (÷ n), so these are
        // too — the two answer the same number on the same buffer. The
        // canonical example: [2,4,4,4,5,5,7,9] has mean 5, variance 4, sd 2.
        let xs = [2.0f32, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        assert_eq!(tree_variance_f32(&xs, false), Some(4.0));
        assert_eq!(tree_stddev_f32(&xs, false), Some(2.0));

        // The SAMPLE form divides by n-1 instead: 32/7.
        assert_eq!(tree_variance_f32(&xs, true), Some(32.0 / 7.0));
    }

    #[test]
    fn tree_variance_handles_the_small_n_edges() {
        // A single element has zero population variance, and `Stats.variance`
        // says the same. Empty has none at all — the surface reports `None`
        // where `Stats.variance` raises, because every other GPU reduction
        // already answers `None` for an empty buffer.
        assert_eq!(tree_variance_f32(&[3.0], false), Some(0.0));
        assert_eq!(tree_stddev_f32(&[3.0], false), Some(0.0));
        assert_eq!(tree_variance_f32(&[], false), None);
        assert_eq!(tree_stddev_f32(&[], true), None);

        // n = 1 with Bessel divides by zero — an infinity, not a trap. Left as
        // IEEE gives it rather than special-cased, because a sample variance
        // of one observation is genuinely undefined and any invented finite
        // answer would be a lie.
        assert!(tree_variance_f32(&[3.0], true).unwrap().is_nan());
    }

    #[test]
    fn tree_stddev_is_exactly_the_root_of_the_variance() {
        // One computation, one extra operation on the way out — so the two
        // cannot drift.
        for n in [1usize, 7, 64, 65, 4096] {
            let xs: Vec<f32> = (0..n).map(|i| ((i * 37) % 101) as f32 * 0.25).collect();
            let v = tree_variance_f32(&xs, false).unwrap();
            assert_eq!(
                tree_stddev_f32(&xs, false).map(f32::to_bits),
                Some(v.sqrt().to_bits()),
                "n={n}"
            );
        }
    }

    #[test]
    fn tree_variance_reuses_the_sum_tree_for_both_passes() {
        // Both passes go through `tree_reduce_f32`, so the grouping story is
        // inherited rather than re-derived. Spelled out here so a future
        // "optimization" that folded the deviations some other way would fail
        // rather than silently change the specified answer.
        for n in [64usize, 65, 4096] {
            let xs: Vec<f32> = (0..n).map(|i| (i % 13) as f32).collect();
            let mean = tree_reduce_f32(&xs, ReduceOp::Sum).unwrap() / n as f32;
            let squared: Vec<f32> = xs
                .iter()
                .map(|x| {
                    let d = x - mean;
                    d * d
                })
                .collect();
            let ss = tree_reduce_f32(&squared, ReduceOp::Sum).unwrap();
            assert_eq!(
                tree_variance_f32(&xs, false).map(f32::to_bits),
                Some((ss / n as f32).to_bits()),
                "n={n}"
            );
        }
    }

    #[test]
    fn tree_integer_mean_promotes_rather_than_truncating() {
        // The decision, and it is `Stats.mean`'s: the mean of [1, 2] is 1.5,
        // not 1. Truncating would be a different function and would disagree
        // with the CPU on the simplest possible input.
        assert_eq!(tree_mean_i32(&[1, 2]), Some(Ok(1.5)));
        assert_eq!(tree_mean_u32(&[1, 2]), Some(Ok(1.5)));
        assert_eq!(tree_mean_i32(&[-3, -4]), Some(Ok(-3.5)));
        // Empty has no mean.
        assert_eq!(tree_mean_i32(&[]), None);
        assert_eq!(tree_mean_u32(&[]), None);
    }

    #[test]
    fn tree_integer_mean_widens_the_finished_sum_losslessly() {
        // Promoting LATE is what buys the accuracy. Every element here is
        // above 2^24, where an f32 promotion would quantise each one before
        // the sum ever happened; the exact integer sum widened to f64 gives
        // the true mean.
        let xs = [16777217i32, 16777219];
        assert_eq!(tree_mean_i32(&xs), Some(Ok(16777218.0)));
        // And a sum that is not evenly divisible still rounds once, at the
        // divide, rather than accumulating error per element.
        assert_eq!(tree_mean_i32(&[1, 1, 1]), Some(Ok(1.0)));
        assert_eq!(tree_mean_i32(&[1, 2, 2]), Some(Ok(5.0 / 3.0)));
        // u32 above 2^31 is exact in f64 too.
        assert_eq!(tree_mean_u32(&[u32::MAX]), Some(Ok(u32::MAX as f64)));
    }

    #[test]
    fn tree_integer_mean_traps_when_the_sum_does() {
        // The price of computing exactly: the SUM has to fit, even when the
        // mean would. `Stats.mean` promotes first and sails through this —
        // documented, because it is a real behavioural difference and not an
        // oversight. The alternative on a GPU is an f32 promotion, which
        // loses whole integers above 16777216.
        assert_eq!(
            tree_mean_i32(&[i32::MAX, i32::MAX]),
            Some(Err(IntFoldOverflow))
        );
        assert_eq!(
            tree_mean_u32(&[u32::MAX, u32::MAX]),
            Some(Err(IntFoldOverflow))
        );
        // The mean of that first buffer IS representable — this is a trap on
        // the intermediate, not on the answer.
        assert!(i32::MAX as f64 <= f64::MAX);
    }

    #[test]
    fn tree_integer_mean_is_the_tree_sum_over_the_count() {
        // Same relationship `tree_mean_f32` has to `tree_reduce_f32`: one
        // divide, at the end, of the tree's own sum. So the grouping story is
        // inherited rather than re-derived.
        for n in [1usize, 64, 65, 4096] {
            let xs: Vec<i32> = (0..n).map(|i| (i % 11) as i32 - 5).collect();
            let sum = tree_reduce_i32(&xs, ReduceOp::Sum).unwrap().unwrap();
            assert_eq!(tree_mean_i32(&xs), Some(Ok(sum as f64 / n as f64)), "n={n}");
        }
    }

    #[test]
    fn tree_u32_carries_rather_than_sign_flips() {
        // The unsigned overflow condition is a CARRY, not a sign flip — which
        // is why this is a separate twin rather than a generic one. The i32
        // rule would call `u32::MAX + 1` fine (no shared-sign-then-flip) and
        // the u32 rule would miss `i32::MAX + 1`.
        assert_eq!(
            tree_reduce_u32(&[u32::MAX, 1], ReduceOp::Sum),
            Some(Err(IntFoldOverflow))
        );
        // And the value an i32 tree would have TRAPPED on is perfectly fine
        // here — 2^31 fits u32 with room to spare.
        assert_eq!(
            tree_reduce_u32(&[2147483647, 1], ReduceOp::Sum),
            Some(Ok(2147483648))
        );
        assert_eq!(tree_reduce_u32(&[3, 1, 2], ReduceOp::Sum), Some(Ok(6)));
        assert_eq!(tree_reduce_u32(&[], ReduceOp::Sum), Some(Ok(0)));
    }

    #[test]
    fn tree_u32_min_max_span_the_full_unsigned_range() {
        // The identities are the type bounds, so a real `u32::MAX` is still
        // reachable as a max — the value a sign-extending result path would
        // report as `-1`.
        assert_eq!(
            tree_reduce_u32(&[u32::MAX], ReduceOp::Max),
            Some(Ok(u32::MAX))
        );
        assert_eq!(
            tree_reduce_u32(&[u32::MAX], ReduceOp::Min),
            Some(Ok(u32::MAX))
        );
        assert_eq!(tree_reduce_u32(&[0], ReduceOp::Max), Some(Ok(0)));
        // Above 2^31 the unsigned ordering differs from the signed one: as
        // i32 these bits are -1 and 1, so a signed compare would answer the
        // other way round on BOTH.
        assert_eq!(
            tree_reduce_u32(&[u32::MAX, 1], ReduceOp::Max),
            Some(Ok(u32::MAX))
        );
        assert_eq!(tree_reduce_u32(&[u32::MAX, 1], ReduceOp::Min), Some(Ok(1)));
    }

    #[test]
    fn tree_u32_chunks_exactly_like_its_siblings() {
        for n in [64usize, 65, 4096] {
            let xs: Vec<u32> = (0..n).map(|i| (i % 11) as u32).collect();
            let want: u32 = xs.iter().sum();
            assert_eq!(tree_reduce_u32(&xs, ReduceOp::Sum), Some(Ok(want)), "n={n}");
        }
        // `prod` is available over u32 as of B-2026-08-19-13 — the device
        // gained a checked multiply, so the twin has one too.
        assert_eq!(tree_reduce_u32(&[2, 3, 7], ReduceOp::Prod), Some(Ok(42)));
        assert_eq!(
            tree_reduce_u32(&[u32::MAX, 2], ReduceOp::Prod),
            Some(Err(IntFoldOverflow))
        );
        // `mean` is still not a single-shader tree fold: it needs a division
        // the shader cannot place, so it stays out of this combiner.
        assert_eq!(tree_reduce_u32(&[1], ReduceOp::Mean), None);
    }

    #[test]
    fn tree_i32_min_max_never_overflow_and_span_the_full_range() {
        // Min/max are unconditionally available over integers precisely
        // because no combine can leave the range. The identities are the type
        // bounds, so a real `i32::MAX` element is still reachable as a max and
        // a real `i32::MIN` as a min — the integer analogue of padding the
        // float tree with ±inf rather than a finite stand-in.
        assert_eq!(
            tree_reduce_i32(&[i32::MAX], ReduceOp::Max),
            Some(Ok(i32::MAX))
        );
        assert_eq!(
            tree_reduce_i32(&[i32::MIN], ReduceOp::Min),
            Some(Ok(i32::MIN))
        );
        assert_eq!(
            tree_reduce_i32(&[i32::MIN], ReduceOp::Max),
            Some(Ok(i32::MIN))
        );
        assert_eq!(tree_reduce_i32(&[3, -7, 2], ReduceOp::Min), Some(Ok(-7)));
        assert_eq!(tree_reduce_i32(&[3, -7, 2], ReduceOp::Max), Some(Ok(3)));
    }

    #[test]
    fn tree_i32_chunks_exactly_like_the_float_twin() {
        // The integer fold must reproduce the SAME grouping as the float one —
        // a different chunking would change which buffers trap, not just the
        // arithmetic. Checked across the three regimes.
        for n in [64usize, 65, 4096] {
            let xs: Vec<i32> = (0..n).map(|i| (i % 11) as i32 - 5).collect();
            let want: i32 = xs.iter().sum();
            assert_eq!(tree_reduce_i32(&xs, ReduceOp::Sum), Some(Ok(want)), "n={n}");
            assert_eq!(
                tree_reduce_i32(&xs, ReduceOp::Min),
                Some(Ok(*xs.iter().min().unwrap())),
                "n={n}"
            );
        }
        // A long buffer whose TOTAL fits but whose per-chunk partials do not
        // is still an overflow — the chunking is where it happens.
        let spill: Vec<i32> = std::iter::repeat_n(i32::MAX / 32, 4096).collect();
        assert_eq!(
            tree_reduce_i32(&spill, ReduceOp::Sum),
            Some(Err(IntFoldOverflow))
        );
    }

    #[test]
    fn tree_mean_is_the_tree_sum_divided_once() {
        // Mean's entire precision story: the SPECIFIED tree sum, divided by
        // the count, in f32, once. Not compensated, not accumulated wider —
        // so `gpu.mean(v)` and `gpu.sum(v) / n` are the same number to the
        // last bit, and mean inherits the sum's grouping rather than having
        // one of its own.
        for n in [1usize, 3, 64, 65, 4096] {
            let xs: Vec<f32> = (0..n).map(|i| 0.5 + (i % 7) as f32).collect();
            let sum = tree_reduce_f32(&xs, ReduceOp::Sum).unwrap();
            assert_eq!(
                tree_mean_f32(&xs).map(f32::to_bits),
                Some((sum / n as f32).to_bits()),
                "n={n}"
            );
        }

        // Exact where arithmetic is exact, so this is not merely
        // self-consistent: 64 twos average to 2.
        assert_eq!(tree_mean_f32(&[2.0f32; 64]), Some(2.0));
        assert_eq!(tree_mean_f32(&[1.0, 2.0, 3.0]), Some(2.0));

        // And the ORDER is still observable through the mean: 4096 tenths sum
        // to 409.6000061 as a tree of trees, so the mean carries that.
        let tenths = vec![0.1f32; 4096];
        let mean = tree_mean_f32(&tenths).unwrap();
        let left_fold_mean: f32 = tenths.iter().sum::<f32>() / 4096.0;
        assert_ne!(
            mean.to_bits(),
            left_fold_mean.to_bits(),
            "the tree order must reach the mean, not be washed out by it"
        );
    }

    #[test]
    fn tree_mean_of_an_empty_buffer_is_none_not_nan() {
        // `0.0 / 0` is NaN, which is precisely the plausible-looking value
        // that propagates silently through everything downstream. `Stats.mean`
        // refuses this input by trapping; the GPU family refuses it the way
        // its own siblings do.
        assert_eq!(tree_mean_f32(&[]), None);
    }

    #[test]
    fn tree_dot_is_exactly_the_tree_sum_of_the_products() {
        // The guarantee `gpu.dot` ships: it is `gpu.sum(a * b)`, not merely
        // close to it. Checked across all three regimes — inside one
        // workgroup, one partial chunk, and two full levels of folding —
        // because the device earns the equality only if its fused level-0
        // shader chunks exactly like the unfused one.
        for n in [3usize, 64, 65, 4096] {
            let a: Vec<f32> = (0..n).map(|i| 0.5 + (i % 7) as f32).collect();
            let b: Vec<f32> = (0..n).map(|i| 1.5 - (i % 3) as f32).collect();
            let products: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x * y).collect();
            assert_eq!(
                tree_dot_f32(&a, &b),
                tree_reduce_f32(&products, ReduceOp::Sum),
                "n={n}"
            );
        }

        // Empty is the additive identity, like an empty sum.
        assert_eq!(tree_dot_f32(&[], &[]), Some(0.0));

        // And the ORDER still matters, so this is not vacuously true of any
        // implementation: 4096 tenths through the tree of trees give
        // 409.6000061, where a left fold drifts elsewhere.
        let ones = vec![1.0f32; 4096];
        let tenths = vec![0.1f32; 4096];
        let tree = tree_dot_f32(&tenths, &ones).unwrap();
        let left: f32 = tenths.iter().zip(&ones).map(|(x, y)| x * y).sum();
        assert_ne!(tree.to_bits(), left.to_bits(), "order must be observable");
        assert_eq!(tree, tree_reduce_f32(&tenths, ReduceOp::Sum).unwrap());
    }

    #[test]
    fn tree_dot_refuses_mismatched_lengths_rather_than_truncating() {
        // Truncating to the shorter buffer (Rust's `zip`) would silently
        // answer a question nobody asked. The runtime entry point traps on the
        // same condition, so the two surfaces refuse the same programs rather
        // than merely agreeing on the ones they accept.
        assert_eq!(tree_dot_f32(&[1.0, 2.0, 3.0], &[4.0, 5.0]), None);
        assert_eq!(tree_dot_f32(&[], &[1.0]), None);
    }

    #[test]
    fn tree_min_max_ignore_nan_from_either_side() {
        // The property that makes min/max legal in a TREE at all. A
        // NaN-propagating min is fine in a left fold but not in a tree: the
        // halving decides whether a NaN meets a real value as the left or the
        // right operand, so a positional rule would make the answer depend on
        // the buffer length. NaN-ignoring min is associative, so it does not.
        assert_eq!(
            tree_reduce_f32(&[f32::NAN, 1.0, 2.0], ReduceOp::Min),
            Some(1.0)
        );
        assert_eq!(
            tree_reduce_f32(&[2.0, 1.0, f32::NAN], ReduceOp::Min),
            Some(1.0)
        );
        assert_eq!(
            tree_reduce_f32(&[f32::NAN, 1.0, 2.0], ReduceOp::Max),
            Some(2.0)
        );

        // An all-NaN buffer is the one case where the padding is observable:
        // every element is ignored, so the identity survives. Specified, not
        // accidental — the shader does the same, and the E2E test pins it.
        assert_eq!(
            tree_reduce_f32(&[f32::NAN; 3], ReduceOp::Min),
            Some(f32::INFINITY)
        );
    }

    #[test]
    fn tree_min_max_pad_with_infinity_so_a_real_max_element_still_wins() {
        // `f32::MAX` as the min-identity would be BEATEN by a real `f32::MAX`
        // element in a padded chunk — the min of `[f32::MAX]` would come back
        // as `f32::MAX` by luck rather than by computation, and the max of
        // `[f32::MIN]` would come back wrong outright.
        assert_eq!(tree_reduce_f32(&[f32::MAX], ReduceOp::Min), Some(f32::MAX));
        assert_eq!(tree_reduce_f32(&[f32::MIN], ReduceOp::Max), Some(f32::MIN));

        // Empty folds to the identity here; the SURFACE turns that into
        // `None`, because +inf is a plausible wrong answer for "the minimum of
        // nothing" and this function is not the place to invent one.
        assert_eq!(tree_reduce_f32(&[], ReduceOp::Min), Some(f32::INFINITY));
    }

    #[test]
    fn tree_min_max_agree_across_every_grouping() {
        // Associativity, checked rather than asserted: min/max must give the
        // same answer whether the buffer fits one workgroup, spills to two, or
        // needs two full levels. `sum` genuinely cannot promise this (f32
        // addition is not associative, which is why its grouping is part of
        // the specified answer) — min/max can, and this pins the difference.
        for n in [1usize, 63, 64, 65, 128, 4096] {
            let xs: Vec<f32> = (0..n).map(|i| ((i * 37) % 1000) as f32 - 500.0).collect();
            let want_min = xs.iter().copied().fold(f32::INFINITY, f32::min);
            let want_max = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            assert_eq!(tree_reduce_f32(&xs, ReduceOp::Min), Some(want_min), "n={n}");
            assert_eq!(tree_reduce_f32(&xs, ReduceOp::Max), Some(want_max), "n={n}");
        }
    }

    #[test]
    fn prefix_sum_is_inclusive_and_exact_on_small_integers() {
        // Every value here is exactly representable in f32 and every partial
        // sum is too, so the answer is order-independent — this leg pins WHAT
        // is computed, leaving the order to the tests below.
        assert_eq!(tree_prefix_sum_f32(&[]), Vec::<f32>::new());
        assert_eq!(tree_prefix_sum_f32(&[7.0]), vec![7.0]);
        assert_eq!(
            tree_prefix_sum_f32(&[1.0, 2.0, 3.0, 4.0]),
            vec![1.0, 3.0, 6.0, 10.0],
            "inclusive: out[i] is the sum through i, not up to i"
        );
        assert_eq!(
            tree_prefix_sum_f32(&[5.0, -5.0, 5.0, -5.0]),
            vec![5.0, 0.0, 5.0, 0.0],
            "negatives do not need a separate path"
        );
    }

    #[test]
    fn prefix_sum_crosses_the_workgroup_boundary() {
        // The three lengths that exercise the three phases: exactly one
        // chunk (no phase 2 at all), one chunk plus one element (phase 2 runs
        // over a 2-element total array), and a chunk boundary landing
        // mid-buffer. All-ones makes every expected value its own index + 1,
        // so an off-by-one in the chunk offset is visible at a glance rather
        // than as a plausible-looking float.
        for n in [
            GPU_REDUCE_WIDTH,
            GPU_REDUCE_WIDTH + 1,
            GPU_REDUCE_WIDTH * 2,
            GPU_REDUCE_WIDTH * 3 + 7,
        ] {
            let got = tree_prefix_sum_f32(&vec![1.0f32; n]);
            let want: Vec<f32> = (1..=n).map(|i| i as f32).collect();
            assert_eq!(got, want, "n = {n}");
        }
    }

    #[test]
    fn prefix_sum_recurses_past_one_level_of_chunk_totals() {
        // 64 * 64 + 1 elements means the chunk-total array is itself longer
        // than one workgroup, so phase 2 recurses a second time. A twin that
        // handled only one level would be correct up to 4096 and wrong after
        // — passing every small test, which is the failure shape this family
        // keeps having to design against.
        let n = GPU_REDUCE_WIDTH * GPU_REDUCE_WIDTH + 1;
        let got = tree_prefix_sum_f32(&vec![1.0f32; n]);
        assert_eq!(got.len(), n);
        assert_eq!(got[0], 1.0);
        assert_eq!(got[GPU_REDUCE_WIDTH - 1], GPU_REDUCE_WIDTH as f32);
        assert_eq!(got[n - 1], n as f32, "the last element is the total");
        // And it is monotone by exactly one everywhere, so no chunk was
        // shifted by the wrong offset.
        for i in 1..n {
            assert_eq!(got[i] - got[i - 1], 1.0, "step at {i}");
        }
    }

    #[test]
    fn prefix_sums_last_element_is_not_the_tree_sum_in_f32() {
        // PINNED BECAUSE IT IS SPECIFIED, not because it is desirable. Both
        // numbers are "the total"; they differ because the two algorithms
        // reach it by different summation orders and f32 addition is not
        // associative. The halving tree computes (a+c) + (b+d) — its strides
        // run 32..1 over a zero-padded scratch — while Hillis-Steele's last
        // lane computes (a+b) + (c+d).
        //
        // Finding a discriminating buffer takes care: a UNIFORM one does not
        // work (every 4-element grouping of equal values agrees), and neither
        // does any buffer whose two groupings happen to pair the same
        // magnitudes. Measured over random buffers, ~60% differ; this is the
        // smallest legible one.
        //
        //   fold = (a+c) + (b+d) = fl(1 + 2^-24) + fl(1 + 2^-23)
        //                        = 1.0 + (1 + 2^-23)  -> 2.0        (tie to even)
        //   scan = (a+b) + (c+d) = 2.0 + (2^-24 + 2^-23)
        //                        = 2.0 + 3*2^-24      -> 2 + 2^-22
        let xs = [
            1.0f32,
            1.0,
            f32::from_bits(0x3380_0000),
            f32::from_bits(0x3400_0000),
        ];

        let scan = tree_prefix_sum_f32(&xs);
        let fold = tree_reduce_f32(&xs, ReduceOp::Sum).unwrap();
        assert_ne!(
            scan[3].to_bits(),
            fold.to_bits(),
            "if these ever agree the summation orders have converged — \
             re-derive the docs on tree_prefix_sum_f32 before relaxing this"
        );

        // And each is what its own grouping predicts, so the test says which
        // is which rather than merely that they differ.
        let (a, b, c, d) = (xs[0], xs[1], xs[2], xs[3]);
        assert_eq!(
            fold.to_bits(),
            ((a + c) + (b + d)).to_bits(),
            "halving tree"
        );
        assert_eq!(
            scan[3].to_bits(),
            ((a + b) + (c + d)).to_bits(),
            "Hillis-Steele"
        );
        assert_eq!(fold.to_bits(), 2.0f32.to_bits());
        assert_eq!(
            scan[3].to_bits(),
            (2.0f32 + f32::from_bits(0x3480_0000)).to_bits()
        );
    }

    #[test]
    fn prefix_sum_reads_pre_step_values_at_every_stride() {
        // The `prev` copy in `one_workgroup_scan_f32` is the shader's
        // workgroup barrier. Updating in place instead would let a low lane's
        // freshly written value feed a high lane within the same step, so
        // element i would be counted more than once — and the result would
        // still be monotone and still look like a prefix sum, which is why
        // this needs its own test rather than trusting the all-ones legs.
        //
        // Powers of two make double-counting unmistakable: the correct
        // prefix sums are 1, 3, 7, 15, …, one below the next power.
        let xs: Vec<f32> = (0..GPU_REDUCE_WIDTH.min(20))
            .map(|i| (1u32 << i) as f32)
            .collect();
        let got = tree_prefix_sum_f32(&xs);
        for (i, &g) in got.iter().enumerate() {
            let want = ((1u64 << (i + 1)) - 1) as f32;
            assert_eq!(g, want, "at {i}: any double count shows up here");
        }
    }

    #[test]
    fn tree_reduce_folds_a_long_buffer_as_a_tree_of_trees() {
        // The GROUPING is the answer, not merely "a tree": 4096 elements is 64
        // workgroups collapsing to 64 partials, then one workgroup over those.
        // In f32 that is a different number from a flat 4096-wide tree and
        // from a left fold, so the twin has to reproduce the chunking exactly
        // — otherwise `karac run` and `karac build` disagree on long buffers
        // while still agreeing on short ones, which is the worst way to be
        // wrong (it passes every small test).
        let xs = vec![0.1f32; 4096];
        let got = tree_reduce_f32(&xs, ReduceOp::Sum).unwrap();

        let partials: Vec<f32> = xs
            .chunks(GPU_REDUCE_WIDTH)
            .map(|c| tree_reduce_f32(c, ReduceOp::Sum).unwrap())
            .collect();
        assert_eq!(partials.len(), 64);
        let expected = tree_reduce_f32(&partials, ReduceOp::Sum).unwrap();
        assert_eq!(got.to_bits(), expected.to_bits(), "chunk-then-fold");

        // Order-independent leg, so a twin that agreed with itself but not
        // with arithmetic would still fail: 4096 tenths is 409.6.
        assert!((got - 409.6).abs() < 0.01, "got {got}");

        // A partial chunk is padded with the identity, not dropped: 65 ones
        // is one full workgroup plus a chunk of one.
        let odd = vec![1.0f32; GPU_REDUCE_WIDTH + 1];
        assert_eq!(tree_reduce_f32(&odd, ReduceOp::Sum), Some(65.0));
        let odd_prod = vec![2.0f32; GPU_REDUCE_WIDTH + 1];
        assert_eq!(
            tree_reduce_f32(&odd_prod, ReduceOp::Prod),
            Some(2.0f32.powi(65))
        );
    }

    use super::*;

    fn scalar(o: ReduceOutcome) -> f64 {
        match o {
            ReduceOutcome::Scalar(f) => f,
            other => panic!("expected Scalar, got {other:?}"),
        }
    }

    #[test]
    fn sum_and_prod_identities_on_empty() {
        assert_eq!(scalar(reduce_f64(&[], ReduceOp::Sum)), 0.0);
        assert_eq!(scalar(reduce_f64(&[], ReduceOp::Prod)), 1.0);
    }

    #[test]
    fn mean_and_population_variance() {
        let xs = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        assert_eq!(scalar(reduce_f64(&xs, ReduceOp::Mean)), 5.0);
        // Population variance of the classic 8-point set is 4.
        assert_eq!(
            scalar(reduce_f64(&xs, ReduceOp::Var { bessel: false })),
            4.0
        );
        assert_eq!(
            scalar(reduce_f64(&xs, ReduceOp::Std { bessel: false })),
            2.0
        );
    }

    #[test]
    fn sample_variance_uses_n_minus_one() {
        let xs = [1.0, 2.0, 3.0, 4.0, 5.0];
        // population = 2, sample = 2.5
        assert_eq!(
            scalar(reduce_f64(&xs, ReduceOp::Var { bessel: false })),
            2.0
        );
        assert_eq!(scalar(reduce_f64(&xs, ReduceOp::Var { bessel: true })), 2.5);
    }

    #[test]
    fn median_odd_and_even() {
        assert_eq!(scalar(reduce_f64(&[3.0, 1.0, 2.0], ReduceOp::Median)), 2.0);
        assert_eq!(
            scalar(reduce_f64(&[4.0, 1.0, 3.0, 2.0], ReduceOp::Median)),
            2.5
        );
    }

    #[test]
    fn min_max_empty_is_none() {
        assert_eq!(
            reduce_f64(&[], ReduceOp::Min),
            ReduceOutcome::OptScalar(None)
        );
        assert_eq!(
            reduce_f64(&[], ReduceOp::Max),
            ReduceOutcome::OptScalar(None)
        );
        assert_eq!(
            reduce_f64(&[3.0, 1.0, 2.0], ReduceOp::Min),
            ReduceOutcome::OptScalar(Some(1.0))
        );
    }

    #[test]
    fn argmin_argmax_first_on_tie() {
        let xs = [1.0, 3.0, 1.0, 3.0];
        assert_eq!(
            reduce_f64(&xs, ReduceOp::Argmin),
            ReduceOutcome::OptIndex(Some(0))
        );
        assert_eq!(
            reduce_f64(&xs, ReduceOp::Argmax),
            ReduceOutcome::OptIndex(Some(1))
        );
        assert_eq!(
            reduce_f64(&[], ReduceOp::Argmin),
            ReduceOutcome::OptIndex(None)
        );
    }

    #[test]
    fn sort_and_argsort_are_stable_ascending() {
        assert_eq!(
            reduce_f64(&[3.0, 1.0, 2.0], ReduceOp::Sort),
            ReduceOutcome::F64Vec(vec![1.0, 2.0, 3.0])
        );
        assert_eq!(
            reduce_f64(&[3.0, 1.0, 2.0], ReduceOp::Argsort),
            ReduceOutcome::I64Vec(vec![1, 2, 0])
        );
    }

    #[test]
    fn quantile_endpoints_and_interpolation() {
        let sorted = [1.0, 2.0, 3.0, 4.0]; // n = 4
        assert_eq!(quantile_linear_sorted(&sorted, 0.0), 1.0); // min
        assert_eq!(quantile_linear_sorted(&sorted, 3.0), 4.0); // max
                                                               // median position (n-1)/2 = 1.5 → interpolate 2.0..3.0 → 2.5
        assert_eq!(quantile_linear_sorted(&sorted, 1.5), 2.5);
    }

    // ── S5: i64 element kind ──────────────────────────────────────────

    #[test]
    fn i64_sum_prod_are_checked_int_folds() {
        assert_eq!(
            reduce_i64(&[3, 1, 2], ReduceOp::Sum),
            Ok(ReduceOutcome::IntScalar(6))
        );
        assert_eq!(
            reduce_i64(&[3, 1, 2], ReduceOp::Prod),
            Ok(ReduceOutcome::IntScalar(6))
        );
        // Integer identities on empty (NOT the float -0.0 / 1.0).
        assert_eq!(
            reduce_i64(&[], ReduceOp::Sum),
            Ok(ReduceOutcome::IntScalar(0))
        );
        assert_eq!(
            reduce_i64(&[], ReduceOp::Prod),
            Ok(ReduceOutcome::IntScalar(1))
        );
        // Overflow is an Err, not a wrap.
        assert_eq!(
            reduce_i64(&[i64::MAX, 1], ReduceOp::Sum),
            Err(IntFoldOverflow)
        );
        assert_eq!(
            reduce_i64(&[i64::MAX, 2], ReduceOp::Prod),
            Err(IntFoldOverflow)
        );
    }

    #[test]
    fn i64_ordering_ops_are_exact_above_2_pow_53() {
        // 2^53 and 2^53 + 1 are indistinguishable as f64; the int paths
        // must order them exactly.
        let big = (1i64 << 53) + 1;
        let xs = [big, 1i64 << 53];
        assert_eq!(
            reduce_i64(&xs, ReduceOp::Max),
            Ok(ReduceOutcome::OptIntScalar(Some(big)))
        );
        assert_eq!(
            reduce_i64(&xs, ReduceOp::Argmax),
            Ok(ReduceOutcome::OptIndex(Some(0)))
        );
        assert_eq!(
            reduce_i64(&xs, ReduceOp::Sort),
            Ok(ReduceOutcome::I64Vec(vec![1i64 << 53, big]))
        );
        assert_eq!(
            reduce_i64(&xs, ReduceOp::Argsort),
            Ok(ReduceOutcome::I64Vec(vec![1, 0]))
        );
        assert_eq!(
            reduce_i64(&[], ReduceOp::Min),
            Ok(ReduceOutcome::OptIntScalar(None))
        );
    }

    #[test]
    fn i64_float_statistics_promote() {
        let xs = [2i64, 4, 4, 4, 5, 5, 7, 9];
        assert_eq!(
            reduce_i64(&xs, ReduceOp::Mean),
            Ok(ReduceOutcome::Scalar(5.0))
        );
        assert_eq!(
            reduce_i64(&xs, ReduceOp::Var { bessel: false }),
            Ok(ReduceOutcome::Scalar(4.0))
        );
        // Even-count median averages the two exact middles.
        assert_eq!(
            reduce_i64(&[4, 1, 3, 2], ReduceOp::Median),
            Ok(ReduceOutcome::Scalar(2.5))
        );
        assert_eq!(
            reduce_i64(&[3, 1, 2], ReduceOp::Median),
            Ok(ReduceOutcome::Scalar(2.0))
        );
    }

    #[test]
    fn i64_quantile_interpolates_exact_ranks() {
        let sorted = [1i64, 2, 3, 4];
        assert_eq!(quantile_linear_sorted_i64(&sorted, 0.0), 1.0);
        assert_eq!(quantile_linear_sorted_i64(&sorted, 3.0), 4.0);
        assert_eq!(quantile_linear_sorted_i64(&sorted, 1.5), 2.5);
    }

    /// A tile-by-tile simulation of the emitted shader: tiles in ascending
    /// `k`, `TILE` steps inside each, both operand tiles zero-padded past the
    /// contraction. Deliberately written as the LITERAL tiling rather than
    /// reusing [`tiled_matmul_f32`] — it is the thing under test, so sharing
    /// code with it would make the comparison vacuous.
    fn simulate_tiling(a: &[f32], b: &[f32], m: usize, k: usize, n: usize) -> Vec<f32> {
        let t = GPU_MATMUL_TILE;
        let mut out = vec![0.0f32; m * n];
        for i in 0..m {
            for j in 0..n {
                let mut acc = 0.0f32;
                for tile in 0..k.div_ceil(t) {
                    // The workgroup-memory staging both shaders do, including
                    // the padding: a lane past the contraction stages 0.0 on
                    // BOTH sides.
                    let mut a_sub = [0.0f32; GPU_MATMUL_TILE];
                    let mut b_sub = [0.0f32; GPU_MATMUL_TILE];
                    for p in 0..t {
                        let kk = tile * t + p;
                        if kk < k {
                            a_sub[p] = a[i * k + kk];
                            b_sub[p] = b[kk * n + j];
                        }
                    }
                    for p in 0..t {
                        acc += a_sub[p] * b_sub[p];
                    }
                }
                out[i * n + j] = acc;
            }
        }
        out
    }

    /// THE PROPERTY `gpu.matmul` RESTS ON: the tiled accumulation order is the
    /// naive one, so `gpu.matmul(a, b)` is bit-for-bit `a.matmul(b)`. If this
    /// ever fails, the GPU op has silently become a different function and
    /// every equality test elsewhere is testing a coincidence.
    ///
    /// Shapes are drawn to straddle the tile edge in all three dimensions
    /// (1, 2, 3, 15, 16, 17, 31, 33 against a tile of 16), because a matmul
    /// whose every dimension is a tile multiple exercises no padding at all —
    /// the case where a one-sided pad would go unnoticed.
    #[test]
    fn matmul_tiling_matches_naive_order() {
        // A deterministic LCG rather than a dependency: the values only need
        // to be non-uniform and reproducible.
        let mut seed: u64 = 0x243f_6a88_85a3_08d3;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as f32 / (1u32 << 31) as f32) * 10.0 - 5.0
        };
        let dims = [1usize, 2, 3, 15, 16, 17, 31, 33];
        let mut checked = 0;
        for &m in &dims {
            for &k in &dims {
                for &n in &dims {
                    let a: Vec<f32> = (0..m * k).map(|_| next()).collect();
                    let b: Vec<f32> = (0..k * n).map(|_| next()).collect();
                    let naive = tiled_matmul_f32(&a, &b, m, k, n).unwrap();
                    let tiled = simulate_tiling(&a, &b, m, k, n);
                    assert_eq!(
                        naive, tiled,
                        "tiled order diverged from naive at [{m}x{k}] x [{k}x{n}] — \
                         `gpu.matmul` is no longer `a.matmul(b)`, and docs/design.md \
                         § Tiled matmul needs re-deriving"
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, dims.len().pow(3));
    }

    /// The inner dimensions have to agree, and a length that does not match
    /// the claimed shape is the same error one step earlier. Both are `None`
    /// rather than a silently-truncated product.
    #[test]
    fn matmul_rejects_mismatched_shapes() {
        let a = vec![1.0f32; 6]; // [2 x 3]
        let b = vec![1.0f32; 6]; // [3 x 2]
        assert!(tiled_matmul_f32(&a, &b, 2, 3, 2).is_some());
        // `a` is not [2 x 4].
        assert!(tiled_matmul_f32(&a, &b, 2, 4, 2).is_none());
        // Right shape for `a`, wrong length for `b`.
        assert!(tiled_matmul_f32(&a, &[1.0f32; 5], 2, 3, 2).is_none());
    }

    /// An empty contraction (`k == 0`) is `[m, n]` of zeros, not an error: the
    /// empty sum is the additive identity, which is the same answer the naive
    /// loop gives when it never runs. Distinct from the reductions, where an
    /// empty input has no answer and returns `None`.
    #[test]
    fn matmul_empty_contraction_is_zeros() {
        assert_eq!(tiled_matmul_f32(&[], &[], 2, 0, 3), Some(vec![0.0f32; 6]));
        assert_eq!(tiled_matmul_f32(&[], &[], 0, 0, 0), Some(Vec::new()));
    }

    /// Padding is applied to BOTH operand tiles at the same `k`, so an
    /// infinity in the last partial tile never meets a padded zero. A
    /// one-sided pad would compute `inf * 0.0` = NaN and poison an output
    /// element whose real value is finite.
    ///
    /// `k = 17` puts exactly one live lane in the second tile, so 15 of that
    /// tile's 16 steps are padded — the widest padding a two-tile contraction
    /// can have, and the most chances to get it wrong.
    #[test]
    fn matmul_padding_does_not_poison_infinities() {
        let k = 17;
        let mut a = vec![1.0f32; k];
        a[k - 1] = f32::INFINITY;
        let mut b = vec![1.0f32; k];
        b[k - 1] = 0.0;
        // The real product of the last pair is inf * 0.0 = NaN, which is the
        // honest answer and must survive; what must NOT happen is the padded
        // lanes manufacturing a second NaN from the infinity.
        let out = tiled_matmul_f32(&a, &b, 1, k, 1).unwrap();
        // `assert_eq!` is useless on a NaN (it never equals itself), so the
        // agreement between the two orders is checked as NaN-ness.
        assert!(out[0].is_nan(), "inf * 0.0 in the DATA is a real NaN");
        assert!(simulate_tiling(&a, &b, 1, k, 1)[0].is_nan());

        // With no inf/0 pair in the data, a long padded tile stays finite.
        let a2 = vec![f32::MAX; k];
        let b2 = vec![0.0f32; k];
        let out2 = tiled_matmul_f32(&a2, &b2, 1, k, 1).unwrap();
        assert_eq!(out2, vec![0.0f32]);
        assert_eq!(simulate_tiling(&a2, &b2, 1, k, 1), out2);
    }

    /// The integer variance must be EXACT — equal to the correctly-rounded
    /// rational value — at magnitudes where forming `(x - mean)` in f32 would
    /// have failed. This is the property the decision rests on.
    ///
    /// The oracle is exact rational arithmetic in `i128`, computed a different
    /// way from the implementation (deviations from the true mean scaled by
    /// `n`, rather than shifted by `round(mean)`), so agreement is evidence
    /// rather than a restatement.
    #[test]
    fn integer_variance_is_exact_at_large_magnitudes() {
        fn oracle(xs: &[i32]) -> f64 {
            let n = xs.len() as i128;
            let sum: i128 = xs.iter().map(|&x| x as i128).sum();
            // Σ(n·x - Σx)² = n²·Σ(x - mean)², all exact.
            let num: i128 = xs
                .iter()
                .map(|&x| {
                    let t = n * x as i128 - sum;
                    t * t
                })
                .sum();
            num as f64 / (n * n * n) as f64
        }
        // Centres that straddle f32's exact-integer limit (2²⁴) and run up to
        // the top of i32 — the range where the naive f32 deviation is wrong.
        for centre in [0i64, 1_000, 1 << 24, 1 << 28, 1 << 30, 2_000_000_000] {
            let xs: Vec<i32> = (0..64)
                .map(|i| (centre + (i * 7 % 201) - 100) as i32)
                .collect();
            let got = tree_variance_i32(&xs, false).unwrap().unwrap();
            assert_eq!(
                got,
                oracle(&xs),
                "centre {centre}: integer variance is not exact — an f32 \
                 deviation path would fail here, which is why this is the \
                 fixture"
            );
        }
    }

    /// `Var(x) = Var(x + c)` for any shift `c`, exactly, at any magnitude.
    /// Translation invariance is the property the whole approach rests on, so
    /// it is asserted directly rather than inferred from the values above.
    #[test]
    fn integer_variance_is_translation_invariant() {
        let base: Vec<i32> = vec![-5, 3, 17, 0, 42, -100, 8, 8];
        let want = tree_variance_i32(&base, false).unwrap().unwrap();
        for shift in [1i64, 1000, 1 << 24, 1 << 28, 1_000_000_000] {
            let shifted: Vec<i32> = base.iter().map(|&x| (x as i64 + shift) as i32).collect();
            assert_eq!(
                tree_variance_i32(&shifted, false).unwrap().unwrap(),
                want,
                "shifting by {shift} changed the variance"
            );
        }
    }

    /// Agreement with the f32 twin on data where f32 is exact — small integers
    /// held in f32 are represented exactly, so the two routes must produce the
    /// same number. Without this the integer path could be self-consistently
    /// wrong.
    #[test]
    fn integer_variance_agrees_with_the_f32_twin_where_f32_is_exact() {
        let xs: Vec<i32> = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let as_f32: Vec<f32> = xs.iter().map(|&x| x as f32).collect();
        let int = tree_variance_i32(&xs, false).unwrap().unwrap();
        let flt = tree_variance_f32(&as_f32, false).unwrap() as f64;
        assert!(
            (int - flt).abs() < 1e-9,
            "integer {int} vs f32 {flt} on exactly-representable data"
        );
    }

    /// Empty has no variance (`None`, like every reduction here). A single
    /// element has population variance 0 but NO sample variance — `n - 1` is
    /// zero — which is NaN rather than a trap or a lie.
    #[test]
    fn integer_variance_empty_and_singleton() {
        assert!(tree_variance_i32(&[], false).is_none());
        assert_eq!(tree_variance_i32(&[7], false).unwrap().unwrap(), 0.0);
        assert!(tree_variance_i32(&[7], true).unwrap().unwrap().is_nan());
    }

    /// The sum of squared deviations is a `u64` on the device, so a buffer
    /// whose spread genuinely does not fit TRAPS rather than returning a
    /// wrapped or saturated number — the same contract `gpu.sum` over
    /// integers already makes.
    #[test]
    fn integer_variance_traps_when_the_squared_deviations_overflow() {
        // Alternating ±2^31 around a mean near zero: each d² is about 2^62,
        // so eight of them pass 2^64.
        let xs: Vec<i32> = (0..64)
            .map(|i| if i % 2 == 0 { i32::MIN } else { i32::MAX })
            .collect();
        // The SUM no longer traps (it is accumulated at 64 bits), so this
        // reaching Err proves the SQUARED-DEVIATION accumulator is what
        // overflowed — which is the only failure this operation has.
        assert!(tree_variance_i32(&xs, false).unwrap().is_err());
        // And the neighbouring case does NOT trap: the same extreme
        // magnitudes with a small spread are fine, because what overflows is
        // the spread, never the position.
        let tight: Vec<i32> = (0..64).map(|i| i32::MAX - i).collect();
        assert!(tree_variance_i32(&tight, false).unwrap().is_ok());
    }

    /// `stddev` is the square root of the variance and nothing else, so
    /// `gpu.stddev(v)` and `gpu.variance(v).sqrt()` are the same number.
    #[test]
    fn integer_stddev_is_the_root_of_integer_variance() {
        let xs: Vec<i32> = vec![10, 20, 30, 40, 1 << 28];
        let v = tree_variance_i32(&xs, false).unwrap().unwrap();
        let s = tree_stddev_i32(&xs, false).unwrap().unwrap();
        assert_eq!(s, v.sqrt());
    }

    /// The unsigned path differs only in which exact integer sum feeds the
    /// mean, so a `u32` buffer holding the same values as an `i32` one must
    /// give the same variance.
    #[test]
    fn unsigned_variance_matches_the_signed_path_on_shared_values() {
        let signed: Vec<i32> = vec![1, 5, 9, 1 << 30, 3];
        let unsigned: Vec<u32> = signed.iter().map(|&x| x as u32).collect();
        assert_eq!(
            tree_variance_u32(&unsigned, false).unwrap().unwrap(),
            tree_variance_i32(&signed, false).unwrap().unwrap()
        );
    }

    /// The integer scan must agree with the f32 one on values f32 represents
    /// exactly, and with an ordinary running total — three independent
    /// derivations of the same answer.
    ///
    /// The agreement is over VALUES THAT FIT. It does NOT extend to the trap
    /// set: see
    /// `integer_prefix_sum_can_trap_where_a_running_total_does_not`, which
    /// pins the buffer where the two genuinely part company.
    #[test]
    fn integer_prefix_sum_matches_the_float_twin_and_a_running_total() {
        for n in [1usize, 7, 63, 64, 65, 200, 4096, 4097] {
            let xs: Vec<i32> = (0..n).map(|i| (i % 7) as i32 - 3).collect();
            let got = tree_prefix_sum_i32(&xs).unwrap();

            let mut running = 0i32;
            let want: Vec<i32> = xs
                .iter()
                .map(|&x| {
                    running += x;
                    running
                })
                .collect();
            assert_eq!(got, want, "n={n}: integer scan is not a running total");

            // Small integers are exact in f32, so the two scans must agree
            // element for element despite the different carrier.
            let as_f32: Vec<f32> = xs.iter().map(|&x| x as f32).collect();
            let flt = tree_prefix_sum_f32(&as_f32);
            let flt_i: Vec<i32> = flt.iter().map(|&f| f as i32).collect();
            assert_eq!(got, flt_i, "n={n}: integer and f32 scans disagree");
        }
    }

    /// **THE SPECIFIED ORDER DECIDES WHETHER AN INTEGER SCAN TRAPS, not
    /// merely what it returns** — the prefix sum's version of the fact
    /// [`tree_reduce_i32`] records for the reductions, and the reason
    /// `gpu.prefix_sum` over integers is not interchangeable with a running
    /// total.
    ///
    /// Hillis-Steele forms WINDOW SUMS that a sequential scan never does. With
    /// `MAX = i32::MAX`, the buffer `[-MAX, MAX, MAX]` has running totals
    /// `-MAX`, `0`, `MAX` — every one comfortably in range. The first
    /// Hillis-Steele step computes `prev[2] + prev[1]`, which is `MAX + MAX`,
    /// and traps.
    ///
    /// That is specified behaviour rather than a divergence: all three
    /// surfaces trap on it, because the interpreter reproduces the same step
    /// order the device runs. But it does mean replacing a running total with
    /// `gpu.prefix_sum` is NOT a pure speedup on integer data — it can
    /// introduce a trap that was not there.
    #[test]
    fn integer_prefix_sum_can_trap_where_a_running_total_does_not() {
        const MAX: i32 = i32::MAX;
        let xs = vec![-MAX, MAX, MAX];

        // The sequential running total never leaves the range.
        let mut running = 0i32;
        for &x in &xs {
            running = running
                .checked_add(x)
                .expect("every running total is in range");
        }
        assert_eq!(running, MAX);

        // The specified scan order traps on the very same buffer.
        assert_eq!(tree_prefix_sum_i32(&xs), Err(IntFoldOverflow));
    }

    /// The empty scan is the empty Vec — no `Option`, because the prefix sums
    /// of nothing are nothing rather than a missing answer.
    #[test]
    fn integer_prefix_sum_empty_is_empty() {
        assert_eq!(tree_prefix_sum_i32(&[]), Ok(Vec::new()));
        assert_eq!(tree_prefix_sum_u32(&[]), Ok(Vec::new()));
    }

    /// An overflow anywhere in the scan traps, because every element is an
    /// output — unlike a reduction, where only lane 0 survives and an
    /// intermediate nobody reads may overflow harmlessly.
    #[test]
    fn integer_prefix_sum_traps_on_overflow() {
        // The running total passes i32::MAX at the third element, which is
        // out[2] — a value the caller receives.
        let xs = vec![i32::MAX, 1, 1];
        assert_eq!(tree_prefix_sum_i32(&xs), Err(IntFoldOverflow));
        // Unsigned overflows by carry rather than by sign flip.
        assert_eq!(tree_prefix_sum_u32(&[u32::MAX, 1]), Err(IntFoldOverflow));
        // And a scan that stays in range does NOT trap, at the very edge.
        assert_eq!(
            tree_prefix_sum_i32(&[i32::MAX, -1]),
            Ok(vec![i32::MAX, i32::MAX - 1])
        );
    }

    /// **A PADDED LANE'S OVERFLOW IS REAL AND MUST TRAP.** A lane past the
    /// chunk's length starts at the identity, but the scan sweeps real values
    /// into it, so it ends up holding the CHUNK TOTAL — which feeds phase 2
    /// and every later chunk's offset. Excluding padded lanes from the check
    /// (the obvious "only real elements matter" shortcut) would drop exactly
    /// this case.
    ///
    /// 65 elements: chunk 0 is full and sums to just under i32::MAX, chunk 1
    /// holds one element that pushes the SECOND chunk's offset past the range.
    #[test]
    fn integer_prefix_sum_traps_when_a_chunk_total_overflows() {
        let mut xs = vec![0i32; 65];
        xs[0] = i32::MAX;
        xs[64] = 1;
        // out[64] = i32::MAX + 1, which does not fit.
        assert_eq!(tree_prefix_sum_i32(&xs), Err(IntFoldOverflow));
    }

    /// The scan is self-similar — a long one is a prefix sum OF PREFIX SUMS —
    /// so an overflow at the SECOND level (folding chunk totals) has to
    /// propagate too. 4097 elements need more than one level of chunk totals,
    /// which is the length that separates a correct implementation from one
    /// that only handles a single level.
    #[test]
    fn integer_prefix_sum_traps_at_the_second_level() {
        let mut xs = vec![1i32; 4097];
        xs[0] = i32::MAX - 100;
        assert_eq!(tree_prefix_sum_i32(&xs), Err(IntFoldOverflow));
        // The same shape with room to spare does not trap, and is a running
        // total to the last element.
        let ok = vec![1i32; 4097];
        assert_eq!(tree_prefix_sum_i32(&ok).unwrap()[4096], 4097);
    }
}
