//! Auto-par **disjointness proof** for loops over indexed writes — plain
//! data, no LLVM.
//!
//! This is sub-slice 2 of *Auto-parallel loops over provably-disjoint indexed
//! writes* (`docs/implementation_checklist/phase-6-runtime.md`; design in
//! `docs/deferred.md § Auto-Parallel Loops over Provably-Disjoint Indexed
//! Writes`). It answers exactly one question about one loop:
//!
//! > For an outer `for v in lo..hi`, does every iteration of `v` write each
//! > target collection only inside a **contiguous range that depends on `v`
//! > alone**, with a loop-invariant stride — so that two distinct iterations
//! > can never touch the same slot?
//!
//! ## Deliberately not dependence analysis
//!
//! There is no solver, no polyhedral model, no direction vectors. The proof
//! obligation is the one shape the motivating workload needs (`karac.dev/prism`
//! and `karac.dev/veil` hand-roll it in every image kernel):
//!
//! ```text
//! for dy in 0..dh {                       // stride S = dw * 4
//!     while dx < dw {                     // dx in [0, dw)
//!         for c in 0..4 {                 // c  in [0, 4)
//!             out[(dy * dw + dx) * 4 + c] = ...
//! ```
//!
//! Expand the index into a linear form over the induction variables, with
//! **symbolic** coefficients drawn from loop-invariant atoms:
//!
//! ```text
//! index = dy * (4*dw)  +  dx * 4  +  c * 1  +  0
//!         ^^^^^^^^^^^     ^^^^^^^^^^^^^^^^^^^^^^
//!         stride S        residual R
//! ```
//!
//! Then discharge two obligations over the residual's range:
//!
//! 1. `Rmin >= 0`
//! 2. `Rmax <  S`   (checked as `S - Rmax - 1 >= 0`)
//!
//! Together those put every write of iteration `dy` in `[dy*S, (dy+1)*S)`, and
//! half-open ranges at a fixed positive stride are pairwise disjoint. For the
//! kernel above `Rmax = 4*(dw-1) + 3 = 4*dw - 1` and `S - Rmax - 1 = 0`, so the
//! proof discharges with no assumptions about `dw` at all. The same shape
//! covers Game of Life steps (`next[y*w + x]`), row-wise matmul
//! (`out[i*n + j]`), and 3-deep image nests (`out[(z*H + y)*W + x]`, where the
//! *inner* coefficients `W` and `H*W` are themselves symbolic).
//!
//! Anything outside it — indirect indexing `out[idx[i]]`, overlapping windows,
//! shared-slot reductions — **declines**, and the decline carries a machine tag
//! plus prose so `karac query concurrency` can answer "why isn't my loop
//! parallel" with a compiler explanation rather than an override keyword.
//!
//! ## What this proves, and what it does not
//!
//! It proves a **memory-footprint** property and nothing else. Three further
//! obligations belong to callers and to the fan-out lowering slice, and are
//! deliberately *not* folded in here, so that a `true` from this module means
//! exactly one thing:
//!
//! - **Aliasing between distinct target names.** Two different bindings can name
//!   the same buffer through a `ref`. That is the ownership/borrow domain;
//!   `ConcurrencyChecker::loop_body_shares_outer_mut_borrow` is the existing
//!   gate and the caller applies it.
//! - **Mutation performed inside a callee.** A `mut ref` argument or a
//!   `mut ref self` method writes memory this walk never sees. Same gate, same
//!   caller. (This module still declines on any *direct* write it cannot place.)
//! - **Observable effects.** Console output, I/O ordering, and non-atomic
//!   `shared` refcounts are orthogonal to footprint disjointness. The caller
//!   applies `loop_body_types_cross_task_safe`; the rest is the lowering slice's
//!   cost/effect gate, exactly as it is for loop reductions today.
//!
//! ## Overflow is a soundness question, not a nuisance
//!
//! Every coefficient operation is checked. A polynomial that overflows `i64`,
//! grows past [`MAX_TERMS`] terms, or exceeds [`MAX_DEGREE`] fails the proof
//! rather than wrapping — a wrapped coefficient would "prove" disjointness of
//! ranges that in fact overlap, which is the silent-miscompile class this whole
//! slice is gated against.

use crate::ast::{
    assign_target_root, BinOp, Block, CompoundOp, Expr, ExprKind, MatchArm, Pattern, PatternKind,
    Stmt, StmtKind, UnaryOp,
};
use std::collections::{BTreeMap, HashMap, HashSet};

// ── Symbolic polynomials over loop-invariant atoms ──────────────

/// A monomial: the sorted multiset of atom names in one product term. The
/// empty vector is the constant monomial.
type Monomial = Vec<String>;

/// Maximum distinct monomials in one polynomial. A real stride is a product of
/// two or three invariants; anything past this is an expression the proof has
/// no business trusting, so it fails closed.
const MAX_TERMS: usize = 32;

/// Maximum atoms in one monomial (i.e. polynomial degree).
const MAX_DEGREE: usize = 6;

/// A polynomial in loop-invariant symbolic atoms with `i64` coefficients.
///
/// Atoms are opaque names (`dw`, `img.width`, `pixels.len()`); the proof never
/// needs their values, only that they are the *same* value on every iteration
/// of the loop being parallelized. All arithmetic is checked — see the module
/// docs on why overflow must fail the proof.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SymPoly {
    terms: BTreeMap<Monomial, i64>,
}

impl SymPoly {
    fn zero() -> Self {
        Self::default()
    }

    fn constant(c: i64) -> Self {
        let mut terms = BTreeMap::new();
        if c != 0 {
            terms.insert(Vec::new(), c);
        }
        Self { terms }
    }

    fn atom(name: &str) -> Self {
        let mut terms = BTreeMap::new();
        terms.insert(vec![name.to_string()], 1);
        Self { terms }
    }

    fn is_zero(&self) -> bool {
        self.terms.is_empty()
    }

    /// The polynomial as a plain integer, when it has no symbolic term.
    fn as_const(&self) -> Option<i64> {
        match self.terms.len() {
            0 => Some(0),
            1 => self.terms.get::<[String]>(&[]).copied(),
            _ => None,
        }
    }

    /// The single atom this polynomial *is* (coefficient 1, degree 1, no
    /// constant), if any. Used to promote a loop's upper bound into the
    /// "known >= 1 while inside this loop" set.
    fn as_bare_atom(&self) -> Option<&str> {
        if self.terms.len() != 1 {
            return None;
        }
        let (mono, coeff) = self.terms.iter().next()?;
        if *coeff == 1 && mono.len() == 1 {
            Some(mono[0].as_str())
        } else {
            None
        }
    }

    fn insert(&mut self, mono: Monomial, coeff: i64) -> Option<()> {
        if coeff == 0 {
            return Some(());
        }
        if mono.len() > MAX_DEGREE {
            return None;
        }
        match self.terms.get_mut(&mono) {
            Some(existing) => {
                let sum = existing.checked_add(coeff)?;
                if sum == 0 {
                    self.terms.remove(&mono);
                } else {
                    *existing = sum;
                }
            }
            None => {
                if self.terms.len() >= MAX_TERMS {
                    return None;
                }
                self.terms.insert(mono, coeff);
            }
        }
        Some(())
    }

    fn add(&self, other: &Self) -> Option<Self> {
        let mut out = self.clone();
        for (mono, coeff) in &other.terms {
            out.insert(mono.clone(), *coeff)?;
        }
        Some(out)
    }

    fn neg(&self) -> Option<Self> {
        let mut out = Self::zero();
        for (mono, coeff) in &self.terms {
            out.insert(mono.clone(), coeff.checked_neg()?)?;
        }
        Some(out)
    }

    fn sub(&self, other: &Self) -> Option<Self> {
        self.add(&other.neg()?)
    }

    fn mul(&self, other: &Self) -> Option<Self> {
        let mut out = Self::zero();
        for (ma, ca) in &self.terms {
            for (mb, cb) in &other.terms {
                let mut mono = ma.clone();
                mono.extend(mb.iter().cloned());
                mono.sort();
                out.insert(mono, ca.checked_mul(*cb)?)?;
            }
        }
        Some(out)
    }

    /// Is this polynomial provably `>= 0`?
    ///
    /// Sufficient (not necessary) condition, and deliberately so — it fails
    /// closed on anything it cannot establish:
    ///
    /// - No *symbolic* monomial may carry a negative coefficient. (The constant
    ///   term may: `dw - 1` is the whole point of the `positive` credit below.)
    /// - Every atom under a positive coefficient must be known non-negative,
    ///   since a positive coefficient on a possibly-negative atom proves
    ///   nothing.
    /// - The running floor — the constant term, plus `+coeff` credited for each
    ///   monomial whose atoms are *all* known `>= 1` — must not go below zero.
    ///
    /// That credit is what discharges `dw - 1 >= 0` inside a loop whose
    /// execution already implies `dw >= 1`. Without it a footprint like
    /// `out[dy*dw]` written *outside* any `dw`-bounded loop would need the same
    /// assumption on no evidence — and that assumption is false exactly when
    /// `dw == 0`, which is the case where every iteration collides on slot 0.
    fn provably_nonneg(&self, nonneg: &HashSet<String>, positive: &HashSet<String>) -> bool {
        let mut floor: i64 = 0;
        let credit = |v: i64, floor: &mut i64| match floor.checked_add(v) {
            Some(next) => {
                *floor = next;
                true
            }
            None => false,
        };
        for (mono, coeff) in &self.terms {
            if mono.is_empty() {
                if !credit(*coeff, &mut floor) {
                    return false;
                }
                continue;
            }
            if *coeff < 0 || !mono.iter().all(|a| nonneg.contains(a)) {
                return false;
            }
            if mono.iter().all(|a| positive.contains(a)) && !credit(*coeff, &mut floor) {
                return false;
            }
        }
        floor >= 0
    }

    /// Human-readable rendering, used in the query `reason` and in the
    /// `--concurrency-report` prose. Deterministic: `BTreeMap` order.
    pub fn render(&self) -> String {
        if self.terms.is_empty() {
            return "0".to_string();
        }
        let mut out = String::new();
        for (mono, coeff) in &self.terms {
            let (sign, mag) = if *coeff < 0 {
                ("-", coeff.unsigned_abs())
            } else {
                ("+", *coeff as u64)
            };
            if out.is_empty() {
                if sign == "-" {
                    out.push('-');
                }
            } else {
                out.push(' ');
                out.push_str(sign);
                out.push(' ');
            }
            if mono.is_empty() {
                out.push_str(&mag.to_string());
            } else if mag == 1 {
                out.push_str(&mono.join(" * "));
            } else {
                out.push_str(&mag.to_string());
                out.push_str(" * ");
                out.push_str(&mono.join(" * "));
            }
        }
        out
    }
}

// ── Linear index forms ──────────────────────────────────────────

/// An index expression expanded as `Σ coeff(v) * v + base`, where `v` ranges
/// over the induction variables in scope and every coefficient is a
/// loop-invariant [`SymPoly`].
///
/// The representation is linear in the induction variables but **non-linear in
/// the invariants** — which is the whole point: `dy * dw` is not affine over
/// `{dy, dw}`, but it is `dy` scaled by the symbolic coefficient `dw`, and that
/// is exactly the form a contiguous-range footprint takes.
#[derive(Clone, Debug, Default)]
struct IndexForm {
    iv: BTreeMap<String, SymPoly>,
    base: SymPoly,
}

impl IndexForm {
    fn constant(c: i64) -> Self {
        Self {
            iv: BTreeMap::new(),
            base: SymPoly::constant(c),
        }
    }

    fn invariant(p: SymPoly) -> Self {
        Self {
            iv: BTreeMap::new(),
            base: p,
        }
    }

    fn induction(name: &str) -> Self {
        let mut iv = BTreeMap::new();
        iv.insert(name.to_string(), SymPoly::constant(1));
        Self {
            iv,
            base: SymPoly::zero(),
        }
    }

    /// `None` when the form mentions an induction variable — i.e. it is not a
    /// loop-invariant quantity.
    fn as_invariant(&self) -> Option<&SymPoly> {
        if self.iv.is_empty() {
            Some(&self.base)
        } else {
            None
        }
    }

    fn add(&self, other: &Self) -> Option<Self> {
        let mut iv = self.iv.clone();
        for (name, coeff) in &other.iv {
            let merged = match iv.get(name) {
                Some(existing) => existing.add(coeff)?,
                None => coeff.clone(),
            };
            if merged.is_zero() {
                iv.remove(name);
            } else {
                iv.insert(name.clone(), merged);
            }
        }
        Some(Self {
            iv,
            base: self.base.add(&other.base)?,
        })
    }

    fn neg(&self) -> Option<Self> {
        let mut iv = BTreeMap::new();
        for (name, coeff) in &self.iv {
            iv.insert(name.clone(), coeff.neg()?);
        }
        Some(Self {
            iv,
            base: self.base.neg()?,
        })
    }

    fn sub(&self, other: &Self) -> Option<Self> {
        self.add(&other.neg()?)
    }

    /// Multiplication is defined only when at least one side is loop-invariant.
    /// `i * i` and `i * j` are rejected — a quadratic index is outside the
    /// contiguous-range shape and must decline, not be approximated.
    fn mul(&self, other: &Self) -> Option<Self> {
        let (scalar, form) = match (self.as_invariant(), other.as_invariant()) {
            (Some(s), _) => (s.clone(), other),
            (None, Some(s)) => (s.clone(), self),
            (None, None) => return None,
        };
        let mut iv = BTreeMap::new();
        for (name, coeff) in &form.iv {
            let scaled = coeff.mul(&scalar)?;
            if !scaled.is_zero() {
                iv.insert(name.clone(), scaled);
            }
        }
        Some(Self {
            iv,
            base: form.base.mul(&scalar)?,
        })
    }
}

/// Half-open bounds `[lo, hi)` on an induction variable, in loop-invariant
/// terms.
#[derive(Clone, Debug)]
struct IvBound {
    lo: SymPoly,
    hi_exclusive: SymPoly,
}

// ── Verdicts ────────────────────────────────────────────────────

/// Why a loop's indexed writes are *not* provably disjoint.
///
/// Every variant carries a stable machine tag and a one-line prose reason, on
/// the same contract as [`crate::par_cost::FanoutVerdict`]: the query surface
/// must be able to say *which* obligation failed, because "the compiler
/// silently didn't parallelize" is the failure mode this whole slice exists to
/// avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisjointDecline {
    /// Not a `for v in lo..hi` over an integer range.
    UnsupportedLoopForm,
    /// The loop variable is assigned inside the body, so iteration `v` does not
    /// identify a footprint.
    LoopVarMutated,
    /// No indexed write to an outside-the-loop collection. Callers use this to
    /// skip the loop entirely rather than report it.
    NoIndexedWrite,
    /// The write target is not a simple `name[...]` (nested index, field path).
    ComplexWriteTarget,
    /// The index reads another collection — `out[idx[i]]`. Explicitly out of
    /// scope per the slice design.
    IndirectIndex,
    /// The index is not linear in the induction variables (products of two
    /// induction variables, `/`, `%`, casts, opaque calls).
    NonAffineIndex,
    /// The index mentions a name that is neither an induction variable nor
    /// provably loop-invariant.
    IndexNotInvariant,
    /// The index does not mention the loop variable, so every iteration writes
    /// the same slot.
    InvariantWriteSlot,
    /// A loop-varying name in the index has no invariant `[lo, hi)` bound: an
    /// inner induction variable whose loop is not counted, or a counter the
    /// body advances more than once per iteration.
    UnboundedInnerLoop,
    /// An induction variable's coefficient could not be shown non-negative, so
    /// the residual's min/max cannot be ordered.
    CoefficientSignUnknown,
    /// Two writes to the same target disagree on the per-iteration stride or on
    /// the invariant base the tiling starts at.
    StrideMismatch,
    /// The residual is not provably inside `[0, stride)`: iterations can
    /// overlap.
    FootprintOverlap,
    /// The body reads a collection it also writes — a possible cross-iteration
    /// dependency.
    ReadsWrittenTarget,
    /// The body writes outside-the-loop state some other way (scalar
    /// assignment, field store, compound assign to a non-target).
    OtherOuterWrite,
    /// The body contains a construct this walk refuses to reason about
    /// (closure, `unsafe`, `par`/`seq`/`lock`, `?`).
    OpaqueBodyConstruct,
    /// `return`, or a `break` that leaves the loop being parallelized.
    EarlyExit,
    /// Coefficient arithmetic overflowed, or the polynomial grew past the term
    /// / degree caps. Fails the proof rather than wrapping.
    SymbolicOverflow,
    /// Set by the caller: the body touches a non-`par` `shared` value, whose
    /// refcount header is non-atomic (B-2026-07-16-6).
    NotCrossTaskSafe,
    /// Set by the caller: the body passes a loop-invariant buffer to a callee
    /// by `mut ref` / `mut Slice` (B-2026-07-23-20).
    SharesOuterMutBorrow,
}

impl DisjointDecline {
    /// Stable machine-readable tag for `karac query concurrency`.
    pub fn tag(self) -> &'static str {
        match self {
            DisjointDecline::UnsupportedLoopForm => "unsupported_loop_form",
            DisjointDecline::LoopVarMutated => "loop_var_mutated",
            DisjointDecline::NoIndexedWrite => "no_indexed_write",
            DisjointDecline::ComplexWriteTarget => "complex_write_target",
            DisjointDecline::IndirectIndex => "indirect_index",
            DisjointDecline::NonAffineIndex => "non_affine_index",
            DisjointDecline::IndexNotInvariant => "index_not_invariant",
            DisjointDecline::InvariantWriteSlot => "invariant_write_slot",
            DisjointDecline::UnboundedInnerLoop => "unbounded_inner_loop",
            DisjointDecline::CoefficientSignUnknown => "coefficient_sign_unknown",
            DisjointDecline::StrideMismatch => "stride_mismatch",
            DisjointDecline::FootprintOverlap => "footprint_overlap",
            DisjointDecline::ReadsWrittenTarget => "reads_written_target",
            DisjointDecline::OtherOuterWrite => "other_outer_write",
            DisjointDecline::OpaqueBodyConstruct => "opaque_body_construct",
            DisjointDecline::EarlyExit => "early_exit",
            DisjointDecline::SymbolicOverflow => "symbolic_overflow",
            DisjointDecline::NotCrossTaskSafe => "not_cross_task_safe",
            DisjointDecline::SharesOuterMutBorrow => "shares_outer_mut_borrow",
        }
    }

    /// One-line explanation, suitable for a diagnostic or a query `reason`.
    pub fn reason(self) -> &'static str {
        match self {
            DisjointDecline::UnsupportedLoopForm => {
                "only `for v in lo..hi` over an integer range carries a per-iteration footprint"
            }
            DisjointDecline::LoopVarMutated => {
                "the loop variable is assigned in the body, so an iteration does not name one footprint"
            }
            DisjointDecline::NoIndexedWrite => {
                "the body performs no indexed write to a collection declared outside the loop"
            }
            DisjointDecline::ComplexWriteTarget => {
                "the write target is not a plain `name[index]`; nested indexing and field paths are out of scope"
            }
            DisjointDecline::IndirectIndex => {
                "the index reads another collection (`out[idx[i]]`), which no static proof can bound"
            }
            DisjointDecline::NonAffineIndex => {
                "the index is not linear in the loop variables (variable product, `/`, `%`, cast, or opaque call)"
            }
            DisjointDecline::IndexNotInvariant => {
                "the index mentions a name that is neither a loop variable nor provably loop-invariant"
            }
            DisjointDecline::InvariantWriteSlot => {
                "the index does not depend on the loop variable, so every iteration writes the same slot"
            }
            DisjointDecline::UnboundedInnerLoop => {
                "a loop-varying name in the index has no loop-invariant `[lo, hi)` bound, so its contribution has no range"
            }
            DisjointDecline::CoefficientSignUnknown => {
                "an inner loop variable's stride could not be shown non-negative, so the footprint has no ordered range"
            }
            DisjointDecline::StrideMismatch => {
                "two writes to the same collection imply different per-iteration ranges (stride or base)"
            }
            DisjointDecline::FootprintOverlap => {
                "the per-iteration index range is not provably inside `[0, stride)`, so iterations can overlap"
            }
            DisjointDecline::ReadsWrittenTarget => {
                "the body reads a collection it also writes, which may be a cross-iteration dependency"
            }
            DisjointDecline::OtherOuterWrite => {
                "the body writes state declared outside the loop other than through a proven indexed write"
            }
            DisjointDecline::OpaqueBodyConstruct => {
                "the body contains a construct this proof does not model (closure, `unsafe`, `par`/`seq`/`lock`, `?`)"
            }
            DisjointDecline::EarlyExit => {
                "the body can leave the loop early (`return`, or a `break` targeting this loop)"
            }
            DisjointDecline::SymbolicOverflow => {
                "index arithmetic overflowed the symbolic model; the proof fails closed rather than wrapping"
            }
            DisjointDecline::NotCrossTaskSafe => {
                "the body touches a non-`par` `shared` value whose refcount is not atomic"
            }
            DisjointDecline::SharesOuterMutBorrow => {
                "the body passes a loop-invariant buffer to a callee by `mut ref`, which mutates it outside this walk"
            }
        }
    }
}

/// The per-iteration footprint proven for one written collection: iteration
/// `v` writes only `[base + v*stride, base + (v+1)*stride)`.
#[derive(Debug, Clone)]
pub struct TargetFootprint {
    /// The collection written.
    pub target: String,
    /// Rendered per-iteration stride, e.g. `4 * dw`.
    pub stride: String,
    /// Rendered loop-invariant offset the whole tiling sits at — `"0"` for the
    /// usual whole-buffer case, non-zero when the loop fills a sub-region
    /// (`out[offset + dy*w + x]`). It shifts every iteration's range equally, so
    /// it cannot create an overlap; it is reported because it is part of what
    /// was proven.
    pub base: String,
    /// How many distinct indexed-write sites in the body landed in this range.
    pub writes: usize,
}

/// A discharged disjointness proof for one outer loop.
#[derive(Debug, Clone)]
pub struct DisjointWriteProof {
    /// The parallelizable induction variable.
    pub loop_var: String,
    /// One entry per written collection, ordered by name.
    pub targets: Vec<TargetFootprint>,
}

impl DisjointWriteProof {
    /// Prose for the query `reason` field, in the shape the slice design
    /// specifies: `iteration dy writes disjoint contiguous range
    /// [dy*dw*4, (dy+1)*dw*4)`.
    pub fn reason(&self) -> String {
        let v = &self.loop_var;
        let ranges: Vec<String> = self
            .targets
            .iter()
            .map(|t| {
                let base = if t.base == "0" {
                    String::new()
                } else {
                    format!("{} + ", t.base)
                };
                format!(
                    "`{}` only within [{base}{v} * ({}), {base}({v} + 1) * ({}))",
                    t.target, t.stride, t.stride
                )
            })
            .collect();
        format!("iteration `{v}` writes {}", ranges.join(", "))
    }
}

// ── Entry point ─────────────────────────────────────────────────

/// Try to prove that the outer loop `loop_expr` writes a disjoint contiguous
/// range per iteration.
///
/// `Err(DisjointDecline::NoIndexedWrite)` means "this loop is not a candidate
/// at all" — callers should skip it rather than report it, so the query surface
/// carries one entry per *indexed-write* loop instead of one per loop.
pub fn prove_disjoint_indexed_writes(
    loop_expr: &Expr,
) -> Result<DisjointWriteProof, DisjointDecline> {
    let ExprKind::For {
        pattern,
        iterable,
        body,
        ..
    } = &loop_expr.kind
    else {
        return Err(DisjointDecline::UnsupportedLoopForm);
    };
    let PatternKind::Binding(loop_var) = &pattern.kind else {
        return Err(DisjointDecline::UnsupportedLoopForm);
    };
    let ExprKind::Range { start, end, .. } = &iterable.kind else {
        return Err(DisjointDecline::UnsupportedLoopForm);
    };
    // The *bounds* are irrelevant to disjointness — distinct `v` values give
    // disjoint ranges whatever the trip count — but an open-ended range is not
    // a counted loop and has no fan-out lowering, so decline it here rather
    // than let a later slice rediscover it.
    if start.is_none() || end.is_none() {
        return Err(DisjointDecline::UnsupportedLoopForm);
    }

    // Pre-pass 1: every name assigned anywhere in the body, and every name the
    // body introduces. Their complement over the body's free names is the set
    // of usable loop-invariant atoms.
    let mut mutated: HashSet<String> = HashSet::new();
    crate::ast::collect_assigned_roots_block(body, &mut mutated);
    if mutated.contains(loop_var) {
        return Err(DisjointDecline::LoopVarMutated);
    }
    let mut declared: HashSet<String> = HashSet::new();
    declared.insert(loop_var.clone());
    collect_declared_names_block(body, &mut declared);

    // Pre-pass 2: the write targets. A target must be an outside-the-loop
    // binding — writing a body-local `Vec` is per-iteration private and not a
    // fan-out opportunity.
    let mut targets: Vec<String> = Vec::new();
    collect_indexed_write_targets(body, &declared, &mut targets);
    targets.sort();
    targets.dedup();
    if targets.is_empty() {
        return Err(DisjointDecline::NoIndexedWrite);
    }
    let target_set: HashSet<String> = targets.iter().cloned().collect();

    let mut prover = Prover {
        loop_var: loop_var.clone(),
        declared: &declared,
        mutated: &mutated,
        targets: &target_set,
        ivs: HashMap::new(),
        local_forms: HashMap::new(),
        mut_inits: HashMap::new(),
        nonneg: HashSet::new(),
        positive: HashSet::new(),
        strides: BTreeMap::new(),
        write_counts: BTreeMap::new(),
        inner_loop_depth: 0,
    };
    prover.walk_block(body)?;

    let footprints: Vec<TargetFootprint> = prover
        .strides
        .iter()
        .map(|(target, (stride, base))| TargetFootprint {
            target: target.clone(),
            stride: stride.render(),
            base: base.render(),
            writes: prover.write_counts.get(target).copied().unwrap_or(0),
        })
        .collect();
    if footprints.is_empty() {
        return Err(DisjointDecline::NoIndexedWrite);
    }
    Ok(DisjointWriteProof {
        loop_var: loop_var.clone(),
        targets: footprints,
    })
}

// ── The walk ────────────────────────────────────────────────────

struct Prover<'a> {
    loop_var: String,
    /// Names introduced anywhere inside the loop body (plus the loop variable).
    /// Anything else free in the body is declared outside it.
    declared: &'a HashSet<String>,
    /// Names assigned anywhere inside the loop body. A mutated name is not a
    /// usable invariant atom even if it is declared outside.
    mutated: &'a HashSet<String>,
    targets: &'a HashSet<String>,
    /// Induction variables currently in scope, with their invariant bounds.
    /// The outer loop variable is deliberately absent: it is the parallel
    /// dimension, not a residual contributor.
    ivs: HashMap<String, Option<IvBound>>,
    /// Immutable body-locals whose initializer is itself an index form, so an
    /// index that names them can be substituted rather than declined.
    local_forms: HashMap<String, IndexForm>,
    /// Latest known initializer of a mutable body-local, for counted-`while`
    /// recognition (`let mut dx = 0; while dx < dw { ...; dx = dx + 1 }`).
    mut_inits: HashMap<String, SymPoly>,
    /// Atoms known `>= 0` at the current point. Two sources: a `.len()` result
    /// (unconditionally non-negative, and re-minted every time the expression
    /// is walked, so scoping costs it nothing), and an enclosing loop's upper
    /// bound. The second is only true *while inside* that loop — an
    /// unexecuted `for x in 0..w` says nothing about `w`'s sign — so this set
    /// is saved and restored around every nested loop, exactly like
    /// [`Self::positive`]. Letting a bound-derived fact leak to a sibling
    /// statement would let `slack = w` pass on no evidence.
    nonneg: HashSet<String>,
    /// Atoms known `>= 1` at the current point, because an enclosing loop whose
    /// upper bound is that atom must have executed to reach here. Saved and
    /// restored around each nested loop.
    positive: HashSet<String>,
    /// Per-target `(stride, base)`, fixed by the first proven write and required
    /// to match for every later write to the same target. Both must match: two
    /// writes at the same stride but different bases tile the buffer at
    /// different offsets, and `[dy*S + B1, ...)` can overlap
    /// `[(dy+1)*S + B2, ...)` when `B2 < B1`.
    strides: BTreeMap<String, (SymPoly, SymPoly)>,
    write_counts: BTreeMap<String, usize>,
    /// Nesting depth of loops *inside* the loop being parallelized. An
    /// unlabeled `break` at depth 0 exits the outer loop.
    inner_loop_depth: usize,
}

impl Prover<'_> {
    // ── Atoms ───────────────────────────────────────────────────

    /// Is `name` usable as a loop-invariant atom? It must be free in the body
    /// (declared outside the loop) and never assigned inside it.
    fn is_invariant_name(&self, name: &str) -> bool {
        !self.declared.contains(name) && !self.mutated.contains(name)
    }

    // ── Index forms ─────────────────────────────────────────────

    /// Resolve a bare name in an index position to its form.
    ///
    /// The two failure modes are reported apart, because they point the reader
    /// at different fixes: a name the body ASSIGNS is loop-varying with no
    /// bound the proof could recover (an unrecognized counter — a `loop {}`
    /// step, a mid-body extra advance), while any other unusable name is
    /// simply not loop-invariant.
    fn name_form(&self, name: &str) -> Result<IndexForm, DisjointDecline> {
        if name == self.loop_var || self.ivs.contains_key(name) {
            return Ok(IndexForm::induction(name));
        }
        if let Some(form) = self.local_forms.get(name) {
            return Ok(form.clone());
        }
        if self.is_invariant_name(name) {
            return Ok(IndexForm::invariant(SymPoly::atom(name)));
        }
        if self.mutated.contains(name) {
            return Err(DisjointDecline::UnboundedInnerLoop);
        }
        Err(DisjointDecline::IndexNotInvariant)
    }

    fn build_form(&mut self, expr: &Expr) -> Result<IndexForm, DisjointDecline> {
        match &expr.kind {
            ExprKind::Integer(n, _) => Ok(IndexForm::constant(*n)),
            ExprKind::Identifier(name) => self.name_form(name),
            // A module-qualified path in an index position is a constant.
            ExprKind::Path { segments, .. } if segments.len() > 1 => {
                Ok(IndexForm::invariant(SymPoly::atom(&segments.join("."))))
            }
            ExprKind::Path { segments, .. } if segments.len() == 1 => self.name_form(&segments[0]),
            ExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => {
                let inner = self.build_form(operand)?;
                inner.neg().ok_or(DisjointDecline::SymbolicOverflow)
            }
            // A read of an outside-the-loop collection's length is a genuine
            // invariant and is always `>= 0` — which is what lets a footprint
            // whose stride is `v.len()` discharge.
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } if method == "len" && args.is_empty() => {
                let ExprKind::Identifier(recv) = &object.kind else {
                    return Err(DisjointDecline::NonAffineIndex);
                };
                if !self.is_invariant_name(recv) {
                    return Err(DisjointDecline::IndexNotInvariant);
                }
                let atom = format!("{recv}.len()");
                self.nonneg.insert(atom.clone());
                Ok(IndexForm::invariant(SymPoly::atom(&atom)))
            }
            ExprKind::FieldAccess { object, field } => {
                let ExprKind::Identifier(recv) = &object.kind else {
                    return Err(DisjointDecline::NonAffineIndex);
                };
                if !self.is_invariant_name(recv) {
                    return Err(DisjointDecline::IndexNotInvariant);
                }
                Ok(IndexForm::invariant(SymPoly::atom(&format!(
                    "{recv}.{field}"
                ))))
            }
            ExprKind::Index { .. } => Err(DisjointDecline::IndirectIndex),
            _ => {
                if let Some(operand) = as_integer_neg(expr) {
                    let inner = self.build_form(operand)?;
                    return inner.neg().ok_or(DisjointDecline::SymbolicOverflow);
                }
                let Some((op, left, right)) = as_integer_binop(expr) else {
                    return Err(DisjointDecline::NonAffineIndex);
                };
                let l = self.build_form(left)?;
                let r = self.build_form(right)?;
                match op {
                    IntOp::Add => l.add(&r).ok_or(DisjointDecline::SymbolicOverflow),
                    IntOp::Sub => l.sub(&r).ok_or(DisjointDecline::SymbolicOverflow),
                    // `mul` returns `None` for two induction-variable operands
                    // (quadratic) as well as on overflow. Distinguish, so the
                    // decline names the real cause.
                    IntOp::Mul => match (l.as_invariant(), r.as_invariant()) {
                        (None, None) => Err(DisjointDecline::NonAffineIndex),
                        _ => l.mul(&r).ok_or(DisjointDecline::SymbolicOverflow),
                    },
                    IntOp::Lt | IntOp::Le => Err(DisjointDecline::NonAffineIndex),
                }
            }
        }
    }

    /// Build a form that must be loop-invariant (a loop bound, a `let`
    /// initializer used as a counter start).
    fn build_invariant(&mut self, expr: &Expr) -> Option<SymPoly> {
        let form = self.build_form(expr).ok()?;
        form.as_invariant().cloned()
    }

    // ── Statement walk ──────────────────────────────────────────

    fn walk_block(&mut self, block: &Block) -> Result<(), DisjointDecline> {
        for stmt in &block.stmts {
            self.walk_stmt(stmt)?;
        }
        if let Some(final_expr) = &block.final_expr {
            self.walk_expr(final_expr)?;
        }
        Ok(())
    }

    fn walk_stmt(&mut self, stmt: &Stmt) -> Result<(), DisjointDecline> {
        match &stmt.kind {
            StmtKind::Let {
                is_mut,
                pattern,
                value,
                ..
            } => {
                self.walk_expr(value)?;
                let PatternKind::Binding(name) = &pattern.kind else {
                    return Ok(());
                };
                // A rebinding invalidates whatever the name meant before.
                self.local_forms.remove(name);
                self.mut_inits.remove(name);
                if *is_mut {
                    if let Some(init) = self.build_invariant(value) {
                        self.mut_inits.insert(name.clone(), init);
                    }
                } else if let Ok(form) = self.build_form(value) {
                    self.local_forms.insert(name.clone(), form);
                }
                Ok(())
            }
            StmtKind::LetUninit { name, .. } => {
                self.local_forms.remove(name);
                self.mut_inits.remove(name);
                Ok(())
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                self.walk_expr(value)?;
                self.walk_block(else_block)?;
                for n in pattern.binding_names() {
                    self.local_forms.remove(&n);
                    self.mut_inits.remove(&n);
                }
                Ok(())
            }
            // A `defer` body runs at scope exit; modelling its interleaving is
            // out of scope for this proof.
            StmtKind::Defer { .. } | StmtKind::ErrDefer { .. } => {
                Err(DisjointDecline::OpaqueBodyConstruct)
            }
            StmtKind::Assign { target, value } => self.walk_assign(target, value, None),
            StmtKind::CompoundAssign { target, op, value } => {
                self.walk_assign(target, value, Some(op))
            }
            // Desugared away before this phase runs; treat as unmodelled rather
            // than silently ignored.
            StmtKind::MultiAssign { .. } => Err(DisjointDecline::OpaqueBodyConstruct),
            StmtKind::Expr(e) => self.walk_expr(e),
        }
    }

    fn walk_assign(
        &mut self,
        target: &Expr,
        value: &Expr,
        compound: Option<&CompoundOp>,
    ) -> Result<(), DisjointDecline> {
        self.walk_expr(value)?;
        match &target.kind {
            // `x = ...` on a body-local is per-iteration private and fine; on
            // anything declared outside the loop it is a shared scalar write.
            ExprKind::Identifier(name) => {
                if !self.declared.contains(name) {
                    return Err(DisjointDecline::OtherOuterWrite);
                }
                self.local_forms.remove(name);
                // Keep the counted-`while` recogniser's view of the counter's
                // start value current across `dx = 0` re-initialisation.
                match self.build_invariant(value) {
                    Some(init) if compound.is_none() => {
                        self.mut_inits.insert(name.clone(), init);
                    }
                    _ => {
                        self.mut_inits.remove(name);
                    }
                }
                Ok(())
            }
            ExprKind::Index { object, index } => {
                let ExprKind::Identifier(name) = &object.kind else {
                    return Err(DisjointDecline::ComplexWriteTarget);
                };
                if self.declared.contains(name) {
                    // A body-local collection: private per iteration. Still walk
                    // the index for reads of shared targets.
                    return self.walk_expr(index);
                }
                // A compound index-assign (`out[i] += x`) reads the slot it
                // writes. That is still within the iteration's own footprint,
                // so it is sound here — the cross-iteration hazard is a read of
                // a *different* target slot, which `walk_expr` catches.
                let _ = compound;
                self.prove_write(name, index)
            }
            _ => Err(DisjointDecline::ComplexWriteTarget),
        }
    }

    /// Discharge the two range obligations for one indexed write and record the
    /// target's stride.
    fn prove_write(&mut self, target: &str, index: &Expr) -> Result<(), DisjointDecline> {
        let form = self.build_form(index)?;

        let Some(stride) = form.iv.get(&self.loop_var).cloned() else {
            return Err(DisjointDecline::InvariantWriteSlot);
        };
        if stride.is_zero() {
            return Err(DisjointDecline::InvariantWriteSlot);
        }

        // The loop-invariant part is the tiling's BASE, not part of the
        // residual: it shifts every iteration's range by the same amount, so it
        // can never make two iterations collide. Folding it into the residual
        // instead would reject `out[offset + dy*w + x]`, which is disjoint.
        let base = form.base.clone();
        let mut r_min = SymPoly::zero();
        let mut r_max = SymPoly::zero();
        for (name, coeff) in &form.iv {
            if name == &self.loop_var {
                continue;
            }
            let Some(bound) = self.ivs.get(name) else {
                return Err(DisjointDecline::UnboundedInnerLoop);
            };
            let Some(bound) = bound.as_ref() else {
                return Err(DisjointDecline::UnboundedInnerLoop);
            };
            // With `coeff >= 0` the extremes are at the ends of `[lo, hi-1]`.
            // A negative or unknown-sign coefficient would flip them, so it
            // declines instead of guessing.
            if !coeff.provably_nonneg(&self.nonneg, &self.positive) {
                return Err(DisjointDecline::CoefficientSignUnknown);
            }
            let hi_inclusive = bound
                .hi_exclusive
                .sub(&SymPoly::constant(1))
                .ok_or(DisjointDecline::SymbolicOverflow)?;
            let lo_term = coeff
                .mul(&bound.lo)
                .ok_or(DisjointDecline::SymbolicOverflow)?;
            let hi_term = coeff
                .mul(&hi_inclusive)
                .ok_or(DisjointDecline::SymbolicOverflow)?;
            r_min = r_min
                .add(&lo_term)
                .ok_or(DisjointDecline::SymbolicOverflow)?;
            r_max = r_max
                .add(&hi_term)
                .ok_or(DisjointDecline::SymbolicOverflow)?;
        }

        // Obligation 1: the residual never reaches below the range base.
        if !r_min.provably_nonneg(&self.nonneg, &self.positive) {
            return Err(DisjointDecline::FootprintOverlap);
        }
        // Obligation 2: `r_max <= stride - 1`, i.e. the residual never reaches
        // the next iteration's base. Together with (1) this also forces
        // `stride >= 1` whenever a write happens, which is what makes the
        // half-open ranges pairwise disjoint.
        let slack = stride
            .sub(&r_max)
            .and_then(|d| d.sub(&SymPoly::constant(1)))
            .ok_or(DisjointDecline::SymbolicOverflow)?;
        if !slack.provably_nonneg(&self.nonneg, &self.positive) {
            return Err(DisjointDecline::FootprintOverlap);
        }

        // Writes to one target must agree on the stride: two different strides
        // tile the buffer differently and their ranges cross.
        match self.strides.get(target) {
            Some((s, b)) if *s != stride || *b != base => {
                return Err(DisjointDecline::StrideMismatch)
            }
            Some(_) => {}
            None => {
                self.strides.insert(target.to_string(), (stride, base));
            }
        }
        *self.write_counts.entry(target.to_string()).or_insert(0) += 1;
        Ok(())
    }

    // ── Expression walk ─────────────────────────────────────────

    fn walk_expr(&mut self, expr: &Expr) -> Result<(), DisjointDecline> {
        match &expr.kind {
            // Leaves.
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => Ok(()),

            ExprKind::Identifier(name) => self.check_read(name),
            ExprKind::Path { segments, .. } => {
                if segments.len() == 1 {
                    self.check_read(&segments[0])
                } else {
                    Ok(())
                }
            }

            ExprKind::Binary { left, right, .. }
            | ExprKind::NilCoalesce { left, right }
            | ExprKind::Pipe { left, right } => {
                self.walk_expr(left)?;
                self.walk_expr(right)
            }
            ExprKind::Unary { operand, .. } => self.walk_expr(operand),
            ExprKind::Cast { expr, .. } => self.walk_expr(expr),
            ExprKind::Call { callee, args } => {
                self.walk_expr(callee)?;
                for a in args {
                    self.walk_expr(&a.value)?;
                }
                Ok(())
            }
            ExprKind::MethodCall { object, args, .. } => {
                self.walk_expr(object)?;
                for a in args {
                    self.walk_expr(&a.value)?;
                }
                Ok(())
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.walk_expr(object)?;
                if let Some(args) = args {
                    for a in args {
                        self.walk_expr(&a.value)?;
                    }
                }
                Ok(())
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.walk_expr(object)
            }
            ExprKind::Index { object, index } => {
                self.walk_expr(object)?;
                self.walk_expr(index)
            }
            ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
                for e in items {
                    self.walk_expr(e)?;
                }
                Ok(())
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.walk_expr(e)?;
                }
                Ok(())
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.walk_expr(value)?;
                self.walk_expr(count)
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.walk_expr(k)?;
                    self.walk_expr(v)?;
                }
                Ok(())
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.walk_expr(&f.value)?;
                }
                if let Some(s) = spread {
                    self.walk_expr(s)?;
                }
                Ok(())
            }
            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let crate::ast::ParsedInterpolationPart::Expr(e, _) = part {
                        self.walk_expr(e)?;
                    }
                }
                Ok(())
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.walk_expr(s)?;
                }
                if let Some(e) = end {
                    self.walk_expr(e)?;
                }
                Ok(())
            }

            // Control flow: scoped, so save/restore the binding maps.
            ExprKind::Block(b) | ExprKind::LabeledBlock { body: b, .. } => self.scoped_block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.walk_expr(condition)?;
                self.scoped_block(then_block)?;
                if let Some(e) = else_branch {
                    self.walk_expr(e)?;
                }
                Ok(())
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                pattern,
            } => {
                self.walk_expr(value)?;
                self.scoped_pattern_block(pattern, then_block)?;
                if let Some(e) = else_branch {
                    self.walk_expr(e)?;
                }
                Ok(())
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee)?;
                for arm in arms {
                    self.walk_match_arm(arm)?;
                }
                Ok(())
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => self.walk_inner_for(pattern, iterable, body),
            ExprKind::While {
                condition, body, ..
            } => self.walk_inner_while(condition, body),
            ExprKind::WhileLet {
                value,
                body,
                pattern,
                ..
            } => {
                self.walk_expr(value)?;
                self.inner_loop_depth += 1;
                let result = self.scoped_pattern_block(pattern, body);
                self.inner_loop_depth -= 1;
                result
            }
            ExprKind::Loop { body, .. } => {
                self.inner_loop_depth += 1;
                let result = self.scoped_block(body);
                self.inner_loop_depth -= 1;
                result
            }

            ExprKind::Continue { .. } => Ok(()),
            // A `break` inside a nested loop stays inside this iteration; one at
            // depth 0 (or any labeled break, whose target this walk does not
            // resolve) can exit the loop being parallelized.
            ExprKind::Break { label, value } => {
                if label.is_some() || self.inner_loop_depth == 0 {
                    return Err(DisjointDecline::EarlyExit);
                }
                if let Some(v) = value {
                    self.walk_expr(v)?;
                }
                Ok(())
            }
            ExprKind::Return(_) | ExprKind::Question(_) => Err(DisjointDecline::EarlyExit),

            // Constructs this proof deliberately does not model.
            ExprKind::Closure { .. }
            | ExprKind::Unsafe(_)
            | ExprKind::Try(_)
            | ExprKind::Seq(_)
            | ExprKind::Par(_)
            | ExprKind::Lock { .. }
            | ExprKind::Providers { .. }
            | ExprKind::Comptime(_) => Err(DisjointDecline::OpaqueBodyConstruct),
        }
    }

    /// A read of a collection the loop also writes is a possible
    /// cross-iteration dependency (`out[i] = out[i-1] + 1`). Declining every
    /// such read is coarser than tracking which slot is read, and is the right
    /// conservatism for a proof whose failure mode is a silent miscompile —
    /// the motivating kernels all read one buffer and write another.
    fn check_read(&self, name: &str) -> Result<(), DisjointDecline> {
        if self.targets.contains(name) {
            return Err(DisjointDecline::ReadsWrittenTarget);
        }
        Ok(())
    }

    fn scoped_block(&mut self, block: &Block) -> Result<(), DisjointDecline> {
        let saved_forms = self.local_forms.clone();
        let saved_inits = self.mut_inits.clone();
        let result = self.walk_block(block);
        self.local_forms = saved_forms;
        self.mut_inits = saved_inits;
        result
    }

    fn scoped_pattern_block(
        &mut self,
        pattern: &Pattern,
        block: &Block,
    ) -> Result<(), DisjointDecline> {
        let saved_forms = self.local_forms.clone();
        let saved_inits = self.mut_inits.clone();
        for n in pattern.binding_names() {
            self.local_forms.remove(&n);
            self.mut_inits.remove(&n);
        }
        let result = self.walk_block(block);
        self.local_forms = saved_forms;
        self.mut_inits = saved_inits;
        result
    }

    fn walk_match_arm(&mut self, arm: &MatchArm) -> Result<(), DisjointDecline> {
        let saved_forms = self.local_forms.clone();
        let saved_inits = self.mut_inits.clone();
        for n in arm.pattern.binding_names() {
            self.local_forms.remove(&n);
            self.mut_inits.remove(&n);
        }
        let mut result = Ok(());
        if let Some(g) = &arm.guard {
            result = self.walk_expr(g);
        }
        if result.is_ok() {
            result = self.walk_expr(&arm.body);
        }
        self.local_forms = saved_forms;
        self.mut_inits = saved_inits;
        result
    }

    // ── Inner loops (the residual's bounds come from here) ───────

    fn walk_inner_for(
        &mut self,
        pattern: &Pattern,
        iterable: &Expr,
        body: &Block,
    ) -> Result<(), DisjointDecline> {
        self.walk_expr(iterable)?;
        let bound = match (&pattern.kind, &iterable.kind) {
            (
                PatternKind::Binding(_),
                ExprKind::Range {
                    start: Some(start),
                    end: Some(end),
                    inclusive,
                },
            ) => {
                let lo = self.build_invariant(start);
                let hi = self.build_invariant(end);
                match (lo, hi) {
                    (Some(lo), Some(hi)) => {
                        // `a..=b` covers `[a, b+1)`.
                        let hi_exclusive = if *inclusive {
                            match hi.add(&SymPoly::constant(1)) {
                                Some(h) => h,
                                None => return Err(DisjointDecline::SymbolicOverflow),
                            }
                        } else {
                            hi
                        };
                        Some(IvBound { lo, hi_exclusive })
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let name = match &pattern.kind {
            PatternKind::Binding(n) => Some(n.clone()),
            _ => None,
        };
        // A body that reassigns the induction variable breaks the bound, so the
        // variable enters scope unbounded and any index using it declines.
        let usable = name
            .as_ref()
            .map(|n| !block_assigns_name(body, n))
            .unwrap_or(false);
        self.with_inner_loop(name, if usable { bound } else { None }, pattern, body)
    }

    /// Recognize the counted `while` — `let mut dx = 0; while dx < dw { ...;
    /// dx = dx + 1 }` — which is how a nested loop is spelled in the motivating
    /// kernels. The counter is an induction variable over `[init, bound)`
    /// exactly when its only assignment in the body is a single trailing
    /// positive step; any other write (the escape-skip `if c { i = i + 1 }`
    /// shape) leaves it unbounded.
    fn walk_inner_while(&mut self, condition: &Expr, body: &Block) -> Result<(), DisjointDecline> {
        self.walk_expr(condition)?;
        let bound_and_name = self.counted_while_bound(condition, body);
        let (name, bound) = match bound_and_name {
            Some((n, b)) => (Some(n), Some(b)),
            None => (None, None),
        };
        self.inner_loop_depth += 1;
        let result = self.with_inner_loop_named(name, bound, body);
        self.inner_loop_depth -= 1;
        result
    }

    fn counted_while_bound(&mut self, condition: &Expr, body: &Block) -> Option<(String, IvBound)> {
        let (op, left, right) = as_integer_binop(condition)?;
        let counter = match &left.kind {
            ExprKind::Identifier(n) => n.clone(),
            _ => return None,
        };
        let hi = self.build_invariant(right)?;
        let hi_exclusive = match op {
            IntOp::Lt => hi,
            IntOp::Le => hi.add(&SymPoly::constant(1))?,
            _ => return None,
        };
        let lo = self.mut_inits.get(&counter)?.clone();
        if !single_trailing_positive_step(body, &counter) {
            return None;
        }
        Some((counter, IvBound { lo, hi_exclusive }))
    }

    fn with_inner_loop(
        &mut self,
        name: Option<String>,
        bound: Option<IvBound>,
        pattern: &Pattern,
        body: &Block,
    ) -> Result<(), DisjointDecline> {
        let extra = pattern.binding_names();
        self.inner_loop_depth += 1;
        let result = self.with_inner_loop_scoped(name, bound, &extra, body);
        self.inner_loop_depth -= 1;
        result
    }

    fn with_inner_loop_named(
        &mut self,
        name: Option<String>,
        bound: Option<IvBound>,
        body: &Block,
    ) -> Result<(), DisjointDecline> {
        self.with_inner_loop_scoped(name, bound, &[], body)
    }

    fn with_inner_loop_scoped(
        &mut self,
        name: Option<String>,
        bound: Option<IvBound>,
        shadowed: &[String],
        body: &Block,
    ) -> Result<(), DisjointDecline> {
        let saved_forms = self.local_forms.clone();
        let saved_inits = self.mut_inits.clone();
        let saved_positive = self.positive.clone();
        let saved_nonneg = self.nonneg.clone();
        let saved_ivs = self.ivs.clone();

        for n in shadowed {
            self.local_forms.remove(n);
            self.mut_inits.remove(n);
        }
        // Reaching the body means the loop executed, so `lo < hi`. When `lo` is
        // a non-negative constant and `hi` is a bare atom, that atom is `>= 1`
        // for the extent of this body — the fact that discharges `dw - 1 >= 0`
        // for writes nested inside `while dx < dw`.
        if let Some(b) = &bound {
            if let (Some(lo), Some(atom)) = (b.lo.as_const(), b.hi_exclusive.as_bare_atom()) {
                if lo >= 0 {
                    let atom = atom.to_string();
                    self.nonneg.insert(atom.clone());
                    self.positive.insert(atom);
                }
            }
        }
        if let Some(n) = &name {
            self.ivs.insert(n.clone(), bound);
        }

        let result = self.walk_block(body);

        self.ivs = saved_ivs;
        self.positive = saved_positive;
        self.nonneg = saved_nonneg;
        self.local_forms = saved_forms;
        self.mut_inits = saved_inits;
        result
    }
}

// ── Generic traversal ───────────────────────────────────────────

/// One immediate child of an expression or block — an expression or a nested
/// block. A single callback (rather than two) keeps a visitor able to hold one
/// `&mut` accumulator across both arms.
enum Child<'a> {
    Expr(&'a Expr),
    Block(&'a Block),
}

/// Visit every immediate sub-expression and sub-block of `expr`.
///
/// Exhaustive on `ExprKind` **by design** — no wildcard arm. A new expression
/// form must be classified here explicitly, because a silently-unvisited
/// subtree would hide an indexed write from the pre-pass and turn a real
/// overlap into a "proof".
fn for_each_child(expr: &Expr, on: &mut dyn FnMut(Child<'_>)) {
    match &expr.kind {
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(_)
        | ExprKind::Identifier(_)
        | ExprKind::Path { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::PipePlaceholder
        | ExprKind::OffsetOf { .. }
        | ExprKind::Continue { .. }
        | ExprKind::Error => {}
        ExprKind::InterpolatedStringLit(parts) => {
            for part in parts {
                if let crate::ast::ParsedInterpolationPart::Expr(e, _) = part {
                    on(Child::Expr(e));
                }
            }
        }
        ExprKind::Binary { left, right, .. }
        | ExprKind::NilCoalesce { left, right }
        | ExprKind::Pipe { left, right } => {
            on(Child::Expr(left));
            on(Child::Expr(right));
        }
        ExprKind::Unary { operand, .. } => on(Child::Expr(operand)),
        ExprKind::Question(inner) => on(Child::Expr(inner)),
        ExprKind::OptionalChain { object, args, .. } => {
            on(Child::Expr(object));
            for a in args.iter().flatten() {
                on(Child::Expr(&a.value));
            }
        }
        ExprKind::Call { callee, args } => {
            on(Child::Expr(callee));
            for a in args {
                on(Child::Expr(&a.value));
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            on(Child::Expr(object));
            for a in args {
                on(Child::Expr(&a.value));
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            on(Child::Expr(object))
        }
        ExprKind::Index { object, index } => {
            on(Child::Expr(object));
            on(Child::Expr(index));
        }
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. } => on(Child::Block(b)),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            on(Child::Expr(condition));
            on(Child::Block(then_block));
            if let Some(e) = else_branch {
                on(Child::Expr(e));
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            on(Child::Expr(value));
            on(Child::Block(then_block));
            if let Some(e) = else_branch {
                on(Child::Expr(e));
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            on(Child::Expr(scrutinee));
            for arm in arms {
                if let Some(g) = &arm.guard {
                    on(Child::Expr(g));
                }
                on(Child::Expr(&arm.body));
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            on(Child::Expr(condition));
            on(Child::Block(body));
        }
        ExprKind::WhileLet { value, body, .. } => {
            on(Child::Expr(value));
            on(Child::Block(body));
        }
        ExprKind::For { iterable, body, .. } => {
            on(Child::Expr(iterable));
            on(Child::Block(body));
        }
        ExprKind::Closure { body, .. } => on(Child::Expr(body)),
        ExprKind::Return(value) => {
            if let Some(v) = value {
                on(Child::Expr(v));
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(v) = value {
                on(Child::Expr(v));
            }
        }
        ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
            for e in items {
                on(Child::Expr(e));
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for e in items {
                on(Child::Expr(e));
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            on(Child::Expr(value));
            on(Child::Expr(count));
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs {
                on(Child::Expr(k));
                on(Child::Expr(v));
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                on(Child::Expr(&f.value));
            }
            if let Some(s) = spread {
                on(Child::Expr(s));
            }
        }
        ExprKind::Cast { expr: inner, .. } => on(Child::Expr(inner)),
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                on(Child::Expr(s));
            }
            if let Some(e) = end {
                on(Child::Expr(e));
            }
        }
        ExprKind::Lock { mutex, body, .. } => {
            on(Child::Expr(mutex));
            on(Child::Block(body));
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                on(Child::Expr(&b.value));
            }
            on(Child::Block(body));
        }
    }
}

/// Visit every immediate sub-expression and sub-block of every statement in
/// `block`, plus its tail expression.
fn for_each_block_child(block: &Block, on: &mut dyn FnMut(Child<'_>)) {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Let { value, .. } => on(Child::Expr(value)),
            StmtKind::LetUninit { .. } => {}
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                on(Child::Expr(value));
                on(Child::Block(else_block));
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => on(Child::Block(body)),
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                on(Child::Expr(target));
                on(Child::Expr(value));
            }
            StmtKind::MultiAssign { targets, values } => {
                for e in targets.iter().chain(values.iter()) {
                    on(Child::Expr(e));
                }
            }
            StmtKind::Expr(e) => on(Child::Expr(e)),
        }
    }
    if let Some(final_expr) = &block.final_expr {
        on(Child::Expr(final_expr));
    }
}

// ── Pre-pass helpers ────────────────────────────────────────────

/// Every name the block introduces, at any nesting depth: `let` bindings, loop
/// patterns, `if let` / `while let` / `match` arm bindings, closure params.
/// Used only to separate "declared inside the loop" from "loop-invariant", so
/// over-collecting is safe (it shrinks the atom set) and under-collecting is
/// not.
fn collect_declared_names_block(block: &Block, out: &mut HashSet<String>) {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                out.extend(pattern.binding_names());
            }
            StmtKind::LetUninit { name, .. } => {
                out.insert(name.clone());
            }
            _ => {}
        }
    }
    for_each_block_child(block, &mut |c| match c {
        Child::Expr(e) => collect_declared_names_expr(e, out),
        Child::Block(b) => collect_declared_names_block(b, out),
    });
}

fn collect_declared_names_expr(expr: &Expr, out: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::For { pattern, .. }
        | ExprKind::IfLet { pattern, .. }
        | ExprKind::WhileLet { pattern, .. } => out.extend(pattern.binding_names()),
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                out.extend(arm.pattern.binding_names());
            }
        }
        ExprKind::Closure { params, .. } => {
            for p in params {
                out.extend(p.pattern.binding_names());
            }
        }
        ExprKind::Lock {
            alias: Some(alias), ..
        } => {
            out.insert(alias.clone());
        }
        _ => {}
    }
    for_each_child(expr, &mut |c| match c {
        Child::Expr(e) => collect_declared_names_expr(e, out),
        Child::Block(b) => collect_declared_names_block(b, out),
    });
}

/// Roots of every `name[...] = ...` / `name[...] op= ...` in the block whose
/// root is declared outside the loop.
fn collect_indexed_write_targets(block: &Block, declared: &HashSet<String>, out: &mut Vec<String>) {
    for stmt in &block.stmts {
        let (StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. }) =
            &stmt.kind
        else {
            continue;
        };
        if let ExprKind::Index { object, .. } = &target.kind {
            if let ExprKind::Identifier(name) = &object.kind {
                if !declared.contains(name) {
                    out.push(name.clone());
                }
            }
        }
    }
    for_each_block_child(block, &mut |c| match c {
        Child::Expr(e) => collect_indexed_write_targets_expr(e, declared, out),
        Child::Block(b) => collect_indexed_write_targets(b, declared, out),
    });
}

fn collect_indexed_write_targets_expr(
    expr: &Expr,
    declared: &HashSet<String>,
    out: &mut Vec<String>,
) {
    for_each_child(expr, &mut |c| match c {
        Child::Expr(e) => collect_indexed_write_targets_expr(e, declared, out),
        Child::Block(b) => collect_indexed_write_targets(b, declared, out),
    });
}

/// Does `block` assign `name` anywhere (at any nesting depth)?
fn block_assigns_name(block: &Block, name: &str) -> bool {
    let mut roots = HashSet::new();
    crate::ast::collect_assigned_roots_block(block, &mut roots);
    roots.contains(name)
}

/// Is the block's *only* write to `counter` a single trailing `counter =
/// counter + K` / `counter += K` with `K` a positive integer literal?
///
/// That is what makes `counter` range over `[init, bound)` at the top of every
/// body execution. An extra mid-body increment (the self-hosted lexer's
/// `if escaped { i = i + 1 }` skip-advance) breaks the invariant for every
/// statement after it, so it must not qualify.
fn single_trailing_positive_step(block: &Block, counter: &str) -> bool {
    let Some(last) = block.stmts.last() else {
        return false;
    };
    let step_ok = match &last.kind {
        StmtKind::Assign { target, value } => {
            matches!(&target.kind, ExprKind::Identifier(n) if n == counter)
                && increment_of(value, counter).is_some_and(|k| k > 0)
        }
        StmtKind::CompoundAssign { target, op, value } => {
            matches!(&target.kind, ExprKind::Identifier(n) if n == counter)
                && matches!(op, CompoundOp::Add)
                && matches!(&value.kind, ExprKind::Integer(k, _) if *k > 0)
        }
        _ => false,
    };
    if !step_ok {
        return false;
    }
    // No other write to the counter anywhere else in the body.
    let mut earlier = HashSet::new();
    for stmt in &block.stmts[..block.stmts.len() - 1] {
        collect_assigned_roots_stmt(stmt, &mut earlier);
    }
    if let Some(final_expr) = &block.final_expr {
        crate::ast::collect_assigned_roots_expr(final_expr, &mut earlier);
    }
    !earlier.contains(counter)
}

fn collect_assigned_roots_stmt(stmt: &Stmt, out: &mut HashSet<String>) {
    match &stmt.kind {
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            if let Some(root) = assign_target_root(target) {
                out.insert(root);
            }
            crate::ast::collect_assigned_roots_expr(target, out);
            crate::ast::collect_assigned_roots_expr(value, out);
        }
        StmtKind::Let { value, .. } => crate::ast::collect_assigned_roots_expr(value, out),
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            crate::ast::collect_assigned_roots_expr(value, out);
            crate::ast::collect_assigned_roots_block(else_block, out);
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            crate::ast::collect_assigned_roots_block(body, out)
        }
        StmtKind::Expr(e) => crate::ast::collect_assigned_roots_expr(e, out),
        StmtKind::LetUninit { .. } | StmtKind::MultiAssign { .. } => {}
    }
}

/// `counter + K` / `K + counter` → `Some(K)`.
fn increment_of(value: &Expr, counter: &str) -> Option<i64> {
    let (IntOp::Add, left, right) = as_integer_binop(value)? else {
        return None;
    };
    match (&left.kind, &right.kind) {
        (ExprKind::Identifier(n), ExprKind::Integer(k, _)) if n == counter => Some(*k),
        (ExprKind::Integer(k, _), ExprKind::Identifier(n)) if n == counter => Some(*k),
        _ => None,
    }
}

// ── Operator shape normalisation ────────────────────────────────

/// The integer operators the proof understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntOp {
    Add,
    Sub,
    Mul,
    Lt,
    Le,
}

/// Integer primitives whose `add`/`sub`/`mul` are exact machine arithmetic.
/// A user type's `add` must never fold into the symbolic model — it can mean
/// anything — and neither may a float's, whose rounding breaks the exact
/// coefficient reasoning the proof relies on.
fn is_integer_primitive(name: &str) -> bool {
    matches!(
        name,
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize"
    )
}

/// Normalise `a OP b` across the two shapes this AST takes at different points
/// in the pipeline: the parser's `Binary { op, left, right }`, and
/// `src/lowering.rs`'s post-lowering `Call(Path([int_type, method]), [a, b])`.
///
/// Both must be handled. The CLI lowers before running concurrency analysis, so
/// a `Binary`-only matcher fires exclusively in test pipelines that skip
/// lowering — the trap `concurrency.rs`'s `induction_step_via_assign` documents
/// for the reduction recognizer.
fn as_integer_binop(expr: &Expr) -> Option<(IntOp, &Expr, &Expr)> {
    match &expr.kind {
        ExprKind::Binary { op, left, right } => {
            let op = match op {
                BinOp::Add => IntOp::Add,
                BinOp::Sub => IntOp::Sub,
                BinOp::Mul => IntOp::Mul,
                BinOp::Lt => IntOp::Lt,
                BinOp::LtEq => IntOp::Le,
                _ => return None,
            };
            Some((op, left, right))
        }
        ExprKind::Call { callee, args } => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() != 2 || args.len() != 2 || !is_integer_primitive(&segments[0]) {
                return None;
            }
            let op = match segments[1].as_str() {
                "add" => IntOp::Add,
                "sub" => IntOp::Sub,
                "mul" => IntOp::Mul,
                "lt" => IntOp::Lt,
                "le" => IntOp::Le,
                _ => return None,
            };
            Some((op, &args[0].value, &args[1].value))
        }
        _ => None,
    }
}

/// Sibling of [`as_integer_binop`] for unary negation (`-x` /
/// `i64.neg(x)`).
fn as_integer_neg(expr: &Expr) -> Option<&Expr> {
    match &expr.kind {
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => Some(operand),
        ExprKind::Call { callee, args } => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() != 2
                || args.len() != 1
                || segments[1] != "neg"
                || !is_integer_primitive(&segments[0])
            {
                return None;
            }
            Some(&args[0].value)
        }
        _ => None,
    }
}

// ── Unit tests for the symbolic layer ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn atoms(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn poly_renders_deterministically() {
        let p = SymPoly::constant(4).mul(&SymPoly::atom("dw")).unwrap();
        assert_eq!(p.render(), "4 * dw");
        assert_eq!(SymPoly::zero().render(), "0");
        assert_eq!(SymPoly::constant(-3).render(), "-3");
        let q = SymPoly::atom("h")
            .mul(&SymPoly::atom("w"))
            .unwrap()
            .add(&SymPoly::constant(-1))
            .unwrap();
        assert_eq!(q.render(), "-1 + h * w");
    }

    #[test]
    fn prism_slack_polynomial_is_exactly_zero() {
        // stride = 4*dw, residual max = 4*(dw-1) + 3 = 4*dw - 1.
        // slack = stride - r_max - 1 must be the ZERO polynomial, which is what
        // lets the kernel prove without any assumption about `dw`.
        let stride = SymPoly::constant(4).mul(&SymPoly::atom("dw")).unwrap();
        let r_max = SymPoly::constant(4)
            .mul(&SymPoly::atom("dw").sub(&SymPoly::constant(1)).unwrap())
            .unwrap()
            .add(&SymPoly::constant(3))
            .unwrap();
        let slack = stride
            .sub(&r_max)
            .unwrap()
            .sub(&SymPoly::constant(1))
            .unwrap();
        assert!(slack.is_zero(), "slack was {}", slack.render());
        assert!(slack.provably_nonneg(&HashSet::new(), &HashSet::new()));
    }

    #[test]
    fn nonneg_requires_atom_knowledge_it_does_not_assume() {
        let empty = HashSet::new();
        // `dw - 1` is NOT provably >= 0 with nothing known about `dw`. This is
        // the `out[dy*dw]` case: when `dw == 0` every iteration hits slot 0.
        let p = SymPoly::atom("dw").sub(&SymPoly::constant(1)).unwrap();
        assert!(!p.provably_nonneg(&atoms(&["dw"]), &empty));
        // Knowing `dw >= 1` — from an enclosing `for x in 0..dw` that must have
        // executed to reach the write — discharges it.
        assert!(p.provably_nonneg(&atoms(&["dw"]), &atoms(&["dw"])));
        // A negative coefficient is never non-negative, whatever is known.
        let q = SymPoly::constant(-1).mul(&SymPoly::atom("dw")).unwrap();
        assert!(!q.provably_nonneg(&atoms(&["dw"]), &atoms(&["dw"])));
    }

    #[test]
    fn nonneg_rejects_a_positive_coefficient_on_an_unknown_atom() {
        // `n` could be negative; a "positive coefficient means positive term"
        // shortcut would wrongly accept this.
        let p = SymPoly::atom("n");
        assert!(!p.provably_nonneg(&HashSet::new(), &HashSet::new()));
        assert!(p.provably_nonneg(&atoms(&["n"]), &HashSet::new()));
    }

    #[test]
    fn coefficient_overflow_fails_the_proof_instead_of_wrapping() {
        // A wrapped coefficient would "prove" disjointness of ranges that in
        // fact overlap — the silent-miscompile class this slice is gated
        // against, so every operation is checked.
        let big = SymPoly::constant(i64::MAX);
        assert!(big.add(&SymPoly::constant(1)).is_none());
        assert!(big.mul(&SymPoly::constant(2)).is_none());
        assert!(SymPoly::constant(i64::MIN).neg().is_none());
    }

    #[test]
    fn degree_and_term_caps_fail_closed() {
        // Degree: multiply past MAX_DEGREE atoms into one monomial.
        let mut deep = SymPoly::constant(1);
        let mut overflowed = false;
        for i in 0..(MAX_DEGREE + 2) {
            match deep.mul(&SymPoly::atom(&format!("a{i}"))) {
                Some(next) => deep = next,
                None => {
                    overflowed = true;
                    break;
                }
            }
        }
        assert!(overflowed, "degree cap never fired");

        // Terms: sum more distinct atoms than MAX_TERMS.
        let mut wide = SymPoly::zero();
        let mut overflowed = false;
        for i in 0..(MAX_TERMS + 2) {
            match wide.add(&SymPoly::atom(&format!("b{i}"))) {
                Some(next) => wide = next,
                None => {
                    overflowed = true;
                    break;
                }
            }
        }
        assert!(overflowed, "term cap never fired");
    }

    #[test]
    fn index_form_multiplication_rejects_two_induction_operands() {
        // `i * j` and `i * i` are quadratic — outside the contiguous-range
        // shape, and must decline rather than be approximated.
        let i = IndexForm::induction("i");
        let j = IndexForm::induction("j");
        assert!(i.mul(&j).is_none());
        assert!(i.mul(&i).is_none());
        // `i * w` (one side invariant) is exactly the shape that must work.
        let w = IndexForm::invariant(SymPoly::atom("w"));
        let scaled = i.mul(&w).expect("invariant scaling must fold");
        assert_eq!(scaled.iv.get("i").unwrap().render(), "w");
    }

    #[test]
    fn every_decline_tag_is_distinct() {
        // The tags are a machine surface; two variants sharing one would make
        // a query answer ambiguous.
        let all = [
            DisjointDecline::UnsupportedLoopForm,
            DisjointDecline::LoopVarMutated,
            DisjointDecline::NoIndexedWrite,
            DisjointDecline::ComplexWriteTarget,
            DisjointDecline::IndirectIndex,
            DisjointDecline::NonAffineIndex,
            DisjointDecline::IndexNotInvariant,
            DisjointDecline::InvariantWriteSlot,
            DisjointDecline::UnboundedInnerLoop,
            DisjointDecline::CoefficientSignUnknown,
            DisjointDecline::StrideMismatch,
            DisjointDecline::FootprintOverlap,
            DisjointDecline::ReadsWrittenTarget,
            DisjointDecline::OtherOuterWrite,
            DisjointDecline::OpaqueBodyConstruct,
            DisjointDecline::EarlyExit,
            DisjointDecline::SymbolicOverflow,
            DisjointDecline::NotCrossTaskSafe,
            DisjointDecline::SharesOuterMutBorrow,
        ];
        let tags: HashSet<&str> = all.iter().map(|d| d.tag()).collect();
        assert_eq!(tags.len(), all.len(), "duplicate decline tag");
        assert!(all.iter().all(|d| !d.reason().is_empty()));
    }
}
