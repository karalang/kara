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
}
