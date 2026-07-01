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

/// The result of [`reduce_f64`], shaped by the op. The interpreter maps each
/// variant onto its `Value` representation (bare float, `Option[f64]`,
/// `Option[i64]`, `Vec[f64]`, `Vec[i64]`).
#[derive(Debug, Clone, PartialEq)]
pub enum ReduceOutcome {
    /// `Sum`, `Prod`, `Mean`, `Var`, `Std`, `Median`.
    Scalar(f64),
    /// `Min`, `Max` — `None` iff the input was empty.
    OptScalar(Option<f64>),
    /// `Argmin`, `Argmax` — `None` iff the input was empty.
    OptIndex(Option<i64>),
    /// `Sort`.
    F64Vec(Vec<f64>),
    /// `Argsort`.
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
}
