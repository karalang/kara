//! Maranget-style pattern exhaustiveness for `match` expressions.
//!
//! Tracked under `docs/implementation_checklist/phase-4-interpreter.md` §
//! "TypeChecker: upgrade exhaustiveness to Maranget's algorithm".
//!
//! Slice 1 (landed): pattern matrix data structure + `is_useful` recursion
//! for finite-domain scrutinees only (`bool` and named enums) with opaque
//! payloads.
//!
//! Slice 2 (landed): field-level recursion. Variant payloads, tuple
//! elements, and struct fields are lowered into `Pat::Ctor.args` so
//! `Some(0)` and `Some(1)` are distinct rows. Per-column types are threaded
//! through `is_useful` to drive constructor enumeration at every depth.
//!
//! Slice 3 (landed): top-level scrutinee gate widened from "bool + named
//! enum only" to "everything except function/typeparam/ref/pointer/error/
//! unit". Open-domain types (`i64`, `f64`, `Char`, `Str`, `String`, `Vec`,
//! `Map`, `Slice`, `Array`, etc.) flow through `enumerate_ctors → None`,
//! which lands on the default-matrix path and demands a wildcard arm.
//! `Never` enumerates to an empty constructor list (vacuously exhaustive).
//!
//! Slice 4 (landed): witness construction. The core recursion now
//! returns `Option<Vec<Pat>>` (`None` = covered; `Some(w)` = uncovered
//! witness vector matching the query column count). The witness is
//! repackaged on each recursion level so the caller sees a single root
//! pattern, then rendered as a string for the diagnostic.
//!
//! Slice 5 (landed): reachability pass. `unreachable_arms` walks arms
//! left-to-right, building a covering matrix from prior *unguarded* arms,
//! and reports any arm whose pattern adds no new coverage. Guarded arms
//! don't contribute to the covering matrix.
//!
//! Slice 6 (this file): irrefutability via `U`. `is_pattern_irrefutable`
//! exposes the same recursion to callers that need to know whether a
//! single pattern matches every value of a given type (`let PAT = expr`
//! and function/closure parameters require this; `if let` / `while let`
//! require the inverse). Returns `None` for skipped types so the caller
//! can fall back to the older syntactic check on ref/function/typeparam
//! scrutinees that Maranget doesn't reason about.
//!
//! Slice 6 — range gap analysis (this revision): integer and `char`
//! literals *and* range patterns lower to `PatCtor::IntRange { lo, hi }`
//! (inclusive, in `i128` space) and are reasoned about by **interval
//! splitting** in `int_column_useful` — the type domain (and each query
//! range) is partitioned at the endpoints present in a column into atomic
//! sub-intervals, so a coverage gap yields a missing-value witness and a
//! range tiled by the union of earlier ranges is correctly unreachable.
//! This replaced the prior `RangePattern => Pat::Wildcard` lowering, which
//! was unsound (a lone range arm acted as a catch-all). `i128`/`u128`
//! (domains that don't fit `i128`) keep the open-domain default-matrix
//! behaviour; float literal patterns still lower to `Pat::Wildcard`
//! (awaiting an Eq/Hash story for `f64`).

use crate::ast::{
    BinOp, Expr, ExprKind, LiteralPattern, MatchArm, Pattern, PatternKind, RangeBound, RestPattern,
    UnaryOp,
};
use crate::typechecker::{IntSize, Type, TypeEnv, UIntSize, VariantTypeInfo};

/// Inclusive integer/`char` domain `[min, max]` for a scrutinee type, in
/// the common `i128` space used by `PatCtor::IntRange`. Returns `None` for
/// `i128`/`u128` (their full domains don't fit `i128`, so the split math
/// would be unsound) and for every non-integer/non-`char` type — those
/// route through the open-domain default-matrix path (any non-wildcard arm
/// demands an explicit wildcard), which is sound but imprecise.
fn int_domain(ty: &Type) -> Option<(i128, i128)> {
    match ty {
        Type::Int(s) => match s {
            IntSize::I8 => Some((i8::MIN as i128, i8::MAX as i128)),
            IntSize::I16 => Some((i16::MIN as i128, i16::MAX as i128)),
            IntSize::I32 => Some((i32::MIN as i128, i32::MAX as i128)),
            IntSize::I64 => Some((i64::MIN as i128, i64::MAX as i128)),
            IntSize::I128 => None,
        },
        Type::UInt(s) => match s {
            UIntSize::U8 => Some((0, u8::MAX as i128)),
            UIntSize::U16 => Some((0, u16::MAX as i128)),
            UIntSize::U32 => Some((0, u32::MAX as i128)),
            UIntSize::U64 | UIntSize::Usize => Some((0, u64::MAX as i128)),
            UIntSize::U128 => None,
        },
        // Unicode scalar domain, treated as one contiguous interval. The
        // surrogate gap [0xD800, 0xDFFF] has no inhabitants, so including
        // it here is sound (no real char ever lands there to be a witness).
        Type::Char => Some((0, 0x10FFFF)),
        _ => None,
    }
}

/// Cap on bounded-refinement finite-domain enumeration. A refinement range
/// `[A, B]` with `B − A` greater than this is treated as open-domain (a
/// wildcard arm is required), per design.md § Pattern Exhaustiveness —
/// "When B − A exceeds 1024 the compiler falls back to requiring a wildcard
/// and emits a lint suggesting an enum." The accompanying lint is emitted at
/// the typechecker layer via [`refinement_domain_too_wide`].
pub(crate) const MAX_REFINEMENT_FINITE_DOMAIN: i128 = 1024;

/// The effective integer/`char` domain for exhaustiveness — refinement-aware.
///
/// A refinement type (`type T = Base where …`) or combined distinct type
/// (`distinct type T = Base where …`) over an integer primitive whose
/// predicate is exactly a *bounded* range `self >= A and self <= B` (in any
/// equivalent spelling) defines a **closed finite domain** `[A, B]`; for
/// exhaustiveness the algorithm then treats it like an enum whose variants
/// are the integers `A..=B` (design.md § Pattern Exhaustiveness —
/// "Exception — bounded integer ranges"). Every other case falls back to the
/// base type's full domain via [`int_domain`]: non-refinement types, and
/// refinements that are unbounded (`self > 0`), over-wide (`B − A > 1024`),
/// or over a non-integer base.
fn effective_int_domain(ty: &Type, env: &TypeEnv) -> Option<(i128, i128)> {
    refinement_finite_domain(ty, env).or_else(|| int_domain(ty))
}

/// The closed finite domain `[A, B]` of a bounded-integer refinement, or
/// `None` if `ty` is not such a refinement. Recognizes both `Type::Refinement`
/// (base carried structurally) and the combined `distinct type T = Base where …`
/// form (which flows as a nominal `Type::Named`, base in `env.distinct_bases`,
/// predicate in `env.refinement_predicates`). Requires the base to be a bounded
/// integer primitive (`i8`..`i64` / `u8`..`u64` / `usize` — `int_domain` excludes
/// `i128`/`u128`), the predicate to bound `self` on *both* sides, and the
/// resulting width to be within [`MAX_REFINEMENT_FINITE_DOMAIN`].
fn refinement_finite_domain(ty: &Type, env: &TypeEnv) -> Option<(i128, i128)> {
    let (name, base): (&str, &Type) = match ty {
        Type::Refinement { name, base } => (name.as_str(), base.as_ref()),
        // Combined `distinct type T = Base where self >= A and self <= B`
        // flows as a nominal `Type::Named`; its base lives in `distinct_bases`
        // and its predicate in `refinement_predicates` (phase-9 distinct
        // slice 4 — design.md § Distinct Types). A plain distinct type with
        // no `where` clause has no predicate entry and falls through to None.
        Type::Named { name, .. } => (name.as_str(), env.distinct_bases.get(name)?),
        _ => return None,
    };
    let (dmin, dmax) = int_domain(base)?;
    let pred = env.refinement_predicates.get(name)?;
    let (lo, hi) = refinement_pred_bounds(&pred.expr, env)?;
    // Intersect the predicate range with the base type's representable
    // domain — a defensive clamp; the typechecker rejects out-of-base
    // constants upstream, but clamping keeps the enumeration sound if one
    // slips through.
    let lo = lo.max(dmin);
    let hi = hi.min(dmax);
    if lo > hi || hi - lo > MAX_REFINEMENT_FINITE_DOMAIN {
        return None;
    }
    Some((lo, hi))
}

/// If `ty` is a *bounded* integer refinement whose domain width exceeds
/// [`MAX_REFINEMENT_FINITE_DOMAIN`], return that width. The typechecker uses
/// this to emit the "use an enum" lint while exhaustiveness falls back to
/// requiring a wildcard (design.md § Pattern Exhaustiveness — bounded
/// integer ranges, the `B − A > 1024` fallback). Returns `None` for every
/// type that is either not a both-sides-bounded integer refinement or whose
/// width is within the cap (those are handled by `refinement_finite_domain`).
pub(crate) fn refinement_domain_too_wide(ty: &Type, env: &TypeEnv) -> Option<i128> {
    let (name, base): (&str, &Type) = match ty {
        Type::Refinement { name, base } => (name.as_str(), base.as_ref()),
        Type::Named { name, .. } => (name.as_str(), env.distinct_bases.get(name)?),
        _ => return None,
    };
    int_domain(base)?;
    let pred = env.refinement_predicates.get(name)?;
    let (lo, hi) = refinement_pred_bounds(&pred.expr, env)?;
    let width = hi.checked_sub(lo)?;
    (width > MAX_REFINEMENT_FINITE_DOMAIN).then_some(width)
}

/// Extract the inclusive integer bounds `(A, B)` from a refinement predicate
/// of the form `self >= A and self <= B` (in any equivalent spelling). The
/// predicate must constrain `self` on *both* sides — a one-sided constraint
/// (`self > 0`) is unbounded and returns `None`, so the type keeps the
/// open-domain (wildcard-required) behaviour. Conjunctions tighten: the
/// lower bound is the max of all lower constraints, the upper the min of all
/// upper constraints. Any leaf that is not a recognized `self`-vs-constant
/// integer comparison (or an `and` of such) makes the whole predicate
/// unrecognized → `None`.
fn refinement_pred_bounds(expr: &Expr, env: &TypeEnv) -> Option<(i128, i128)> {
    let mut lo: Option<i128> = None;
    let mut hi: Option<i128> = None;
    collect_refinement_bounds(expr, env, &mut lo, &mut hi)?;
    Some((lo?, hi?))
}

/// Walk a conjunction of `self`-vs-constant comparisons, tightening `lo`/`hi`.
/// Returns `None` (and leaves `lo`/`hi` partially updated) the moment a leaf
/// is not a recognized comparison or `and`.
fn collect_refinement_bounds(
    expr: &Expr,
    env: &TypeEnv,
    lo: &mut Option<i128>,
    hi: &mut Option<i128>,
) -> Option<()> {
    match &expr.kind {
        ExprKind::Binary {
            op: BinOp::And,
            left,
            right,
        } => {
            collect_refinement_bounds(left, env, lo, hi)?;
            collect_refinement_bounds(right, env, lo, hi)
        }
        ExprKind::Binary { op, left, right } => {
            let (bound, is_lower) = comparison_bound(op, left, right, env)?;
            if is_lower {
                *lo = Some(lo.map_or(bound, |cur| cur.max(bound)));
            } else {
                *hi = Some(hi.map_or(bound, |cur| cur.min(bound)));
            }
            Some(())
        }
        _ => None,
    }
}

/// Interpret a single comparison between `self` and a compile-time integer
/// constant as a (bound, is_lower) pair, normalizing strict bounds to
/// inclusive ones (`self > A` → lower `A+1`, `self < B` → upper `B-1`).
/// Returns `None` for any comparison that is not `self`-vs-constant or whose
/// operator is not an order comparison (`==`, `!=`, etc.).
fn comparison_bound(op: &BinOp, left: &Expr, right: &Expr, env: &TypeEnv) -> Option<(i128, bool)> {
    // Orient to `self <op'> K`. If `self` is on the right, flip the operator.
    let (op, k) = if is_self(left) {
        (op.clone(), refinement_const_int(right, env)?)
    } else if is_self(right) {
        (flip_comparison(op)?, refinement_const_int(left, env)?)
    } else {
        return None;
    };
    match op {
        // lower bounds (is_lower = true)
        BinOp::GtEq => Some((k, true)),
        BinOp::Gt => Some((k.checked_add(1)?, true)),
        // upper bounds (is_lower = false)
        BinOp::LtEq => Some((k, false)),
        BinOp::Lt => Some((k.checked_sub(1)?, false)),
        _ => None,
    }
}

/// Mirror a comparison operator so `K <op> self` becomes `self <flip(op)> K`.
fn flip_comparison(op: &BinOp) -> Option<BinOp> {
    match op {
        BinOp::Lt => Some(BinOp::Gt),
        BinOp::LtEq => Some(BinOp::GtEq),
        BinOp::Gt => Some(BinOp::Lt),
        BinOp::GtEq => Some(BinOp::LtEq),
        _ => None,
    }
}

fn is_self(expr: &Expr) -> bool {
    matches!(expr.kind, ExprKind::SelfValue)
}

/// Compile-time integer value of a refinement-predicate leaf: an integer
/// literal, a negated integer literal, or a single-segment constant
/// reference resolved through `env.const_values` (mirrors
/// `range_bound_to_i128`). Returns `None` for anything else.
fn refinement_const_int(expr: &Expr, env: &TypeEnv) -> Option<i128> {
    match &expr.kind {
        ExprKind::Integer(n, _) => Some(*n as i128),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => match &operand.kind {
            ExprKind::Integer(n, _) => (*n as i128).checked_neg(),
            _ => None,
        },
        ExprKind::Identifier(name) => env
            .const_values
            .get(name)
            .and_then(crate::typechecker::const_value_to_i128),
        ExprKind::Path { segments, .. } if segments.len() == 1 => env
            .const_values
            .get(&segments[0])
            .and_then(crate::typechecker::const_value_to_i128),
        _ => None,
    }
}

/// Integer / `char` value of a literal-pattern bound, in the `i128` space.
fn lit_to_i128(l: &LiteralPattern) -> Option<i128> {
    match l {
        LiteralPattern::Integer(n, _) => Some(*n as i128),
        LiteralPattern::Char(c) => Some(*c as i128),
        _ => None,
    }
}

/// Integer / `char` value of a range-pattern bound, in the `i128` space.
/// A `Path` bound (`MAX_AGE..=…`) resolves through the const it names —
/// single-segment via `env.const_values`, recorded by the typechecker
/// after env build. Returns `None` for an unresolved bound; the caller
/// then falls back to the scrutinee-type domain edge. That fallback is
/// only reachable in already-erroring programs — a valid const bound is
/// resolved and recorded before exhaustiveness runs, so a const range
/// never silently widens to a catch-all here.
fn range_bound_to_i128(b: &RangeBound, env: &TypeEnv) -> Option<i128> {
    match b {
        RangeBound::Literal(l) => lit_to_i128(l),
        RangeBound::Path { segments, .. } if segments.len() == 1 => env
            .const_values
            .get(&segments[0])
            .and_then(crate::typechecker::const_value_to_i128),
        RangeBound::Path { .. } => None,
    }
}

#[derive(Debug, Clone)]
enum Pat {
    Wildcard,
    Ctor { ctor: PatCtor, args: Vec<Pat> },
    Or(Vec<Pat>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum PatCtor {
    Bool(bool),
    Variant(String),
    Lit(PatLit),
    /// Integer / `char` value or range, modeled as an inclusive interval
    /// `[lo, hi]` in a common `i128` space (chars map to their codepoint).
    /// A single literal is `lo == hi`. Slice 6: integer/char columns are
    /// reasoned about by **interval splitting** (see `int_column_useful`)
    /// rather than per-value enumeration, so a partition of the type's
    /// domain by the range endpoints present in a column drives both
    /// exhaustiveness (uncovered sub-interval → witness) and reachability
    /// (a range fully covered by the union of earlier ranges adds nothing).
    /// `i128` / `u128` are excluded (their domains don't fit `i128` split
    /// math — `int_domain` returns `None`); they keep the open-domain
    /// default-matrix behaviour (any non-wildcard arm demands a wildcard).
    IntRange {
        lo: i128,
        hi: i128,
    },
    Tuple,
    Struct(String),
    /// Fixed-arity array slice pattern — `Array[T, N]` specializes exactly
    /// like a length-`N` tuple. `args.len() == N`. Single constructor per
    /// concrete `Array[T, N]` scrutinee; reachability follows from the
    /// args. Per design.md § Pattern Exhaustiveness > `Array[T, N]`.
    Array(usize),
    /// Vec/Slice slice pattern — a coarse length-class constructor used so
    /// the matrix can distinguish non-wildcard slice patterns from a true
    /// wildcard. `Vec[T]` / `Slice[T]` are open-domain collection types per
    /// design.md § *Vec / Map / String* — they require an explicit wildcard
    /// arm for exhaustiveness regardless of which slice patterns appear.
    /// `fixed` is `prefix.len() + suffix.len()`; `has_rest` discriminates
    /// `[a, b]` (fixed=2, has_rest=false) from `[a, b, ..]` (fixed=2,
    /// has_rest=true). Precise overlap analysis between length classes
    /// (e.g. `[a, ..]` covers `[a, b]`) is deferred — this representation
    /// gives sound exhaustiveness (always requires wildcard) but imprecise
    /// reachability across distinct length classes.
    SliceLen {
        fixed: usize,
        has_rest: bool,
    },
}

/// Open-domain string literals. Integers and `char`s are modeled as
/// `PatCtor::IntRange` instead (slice 6); only `String`/`str` literals
/// remain a per-value point constructor here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum PatLit {
    String(String),
}

#[derive(Debug, Clone)]
struct Row {
    pats: Vec<Pat>,
}

#[derive(Debug, Clone, Default)]
struct Matrix {
    rows: Vec<Row>,
}

pub enum ExhaustiveResult {
    Exhaustive,
    NonExhaustive {
        witness: String,
    },
    /// Scrutinee type is not yet handled by Maranget; the caller must skip
    /// the check (matches the pre-Maranget early-return behaviour).
    Skipped,
}

/// Returns `Some(true)` if `pat` matches every value of `ty` (irrefutable),
/// `Some(false)` if some value is not matched (refutable), or `None` if
/// the type is outside Maranget's handled set (caller should fall back to
/// the legacy syntactic check). Implements `U([PAT], _) == false` —
/// exactly the rule called out in design.md § Pattern Exhaustiveness for
/// slice 6 of the upgrade.
pub fn is_pattern_irrefutable(pat: &Pattern, ty: &Type, env: &TypeEnv) -> Option<bool> {
    if !is_handled_scrutinee(ty) {
        return None;
    }
    let lowered = lower_pattern(pat, ty, env);
    let matrix = Matrix {
        rows: vec![Row {
            pats: vec![lowered],
        }],
    };
    let head_types = vec![ty.clone()];
    Some(usefulness(&matrix, &[Pat::Wildcard], &head_types, env).is_none())
}

/// Walk the arms in source order and return the indices of any arm whose
/// pattern is fully covered by some earlier *unguarded* arm. Guarded arms
/// are themselves checked for reachability (a guarded arm with a duplicate
/// pattern of an earlier unguarded arm is still unreachable) but never
/// contribute to coverage themselves — the guard might fail at runtime,
/// so a following arm with the same pattern can still be reached.
pub fn unreachable_arms(scrutinee_type: &Type, arms: &[MatchArm], env: &TypeEnv) -> Vec<usize> {
    if !is_handled_scrutinee(scrutinee_type) {
        return Vec::new();
    }
    let head_types = vec![scrutinee_type.clone()];
    let mut unreachable = Vec::new();
    let mut covering = Matrix::default();
    for (i, arm) in arms.iter().enumerate() {
        let pat = lower_pattern(&arm.pattern, scrutinee_type, env);
        let q = vec![pat.clone()];
        if usefulness(&covering, &q, &head_types, env).is_none() {
            unreachable.push(i);
        }
        if arm.guard.is_none() {
            covering.rows.push(Row { pats: vec![pat] });
        }
    }
    unreachable
}

pub fn check_match_exhaustive(
    scrutinee_type: &Type,
    arms: &[MatchArm],
    env: &TypeEnv,
) -> ExhaustiveResult {
    if !is_handled_scrutinee(scrutinee_type) {
        return ExhaustiveResult::Skipped;
    }

    let matrix = build_matrix(arms, scrutinee_type, env);
    let head_types = vec![scrutinee_type.clone()];

    // Slice-pattern length-coverage exhaustiveness (B-2026-07-14-14). `Vec[T]` /
    // `Slice[T]` are open-domain, so the general Maranget engine demands an
    // explicit wildcard arm — but a set of IRREFUTABLE slice patterns whose
    // length classes tile `{0, 1, 2, …}` (e.g. `[]` + `[head, ..]`) is already
    // total. Recognise that conservatively BEFORE the open-domain fallthrough.
    if is_open_slice_scrutinee(scrutinee_type) && slice_patterns_cover_all_lengths(&matrix) {
        return ExhaustiveResult::Exhaustive;
    }

    match usefulness(&matrix, &[Pat::Wildcard], &head_types, env) {
        None => ExhaustiveResult::Exhaustive,
        Some(witness_vec) => {
            let pat = witness_vec.into_iter().next().unwrap_or(Pat::Wildcard);
            ExhaustiveResult::NonExhaustive {
                witness: render_witness(&pat, scrutinee_type, env),
            }
        }
    }
}

/// Gate for top-level exhaustiveness. Denylist: skip computed/erroneous/
/// internal types and ref/pointer wrappers (the latter to preserve the
/// pre-Maranget skip behaviour on `match r { ... }` over `ref T`). Every
/// other type — bool, integers, floats, char, str, tuple, array, slice,
/// named, never — flows through. For open-domain types the wildcard
/// recursion in `is_useful` falls to the default-matrix path, demanding a
/// wildcard arm.
/// A scrutinee that admits open-ended slice patterns: an owned `Vec[T]` or a
/// `Slice[T]`. Used to gate the slice-pattern length-coverage exhaustiveness
/// check (B-2026-07-14-14). `Array[T, N]` is fixed-arity (the general engine
/// handles it exactly); `VecDeque` is excluded from slice patterns entirely.
fn is_open_slice_scrutinee(ty: &Type) -> bool {
    match ty {
        Type::Slice { .. } => true,
        Type::Named { name, .. } => is_open_collection(name),
        _ => false,
    }
}

/// Conservative slice-pattern length-coverage exhaustiveness (B-2026-07-14-14).
/// An IRREFUTABLE open-ended arm `[a…, ..]` (`has_rest`, every fixed element a
/// wildcard/binding) covers EVERY length `≥ fixed`; an irrefutable closed arm
/// `[a…]` covers EXACTLY `fixed`. The match tiles all lengths iff some open-ended
/// arm exists — covering `[m, ∞)` for its minimum fixed `m` — AND every length
/// in `0..m` is covered by a closed arm. Only wildcard/binding elements count
/// (a literal or nested-constructor element narrows the class and can't complete
/// the cover), so this is SOUND: it never turns a genuinely non-exhaustive match
/// total. Operates on the already-lowered matrix, so guarded arms (dropped by
/// `build_matrix`) don't contribute and variant-name element bindings (lowered
/// to a `Ctor`, not `Wildcard`) are correctly treated as refutable.
fn slice_patterns_cover_all_lengths(matrix: &Matrix) -> bool {
    use std::collections::HashSet;
    let mut open_min: Option<usize> = None;
    let mut closed_lens: HashSet<usize> = HashSet::new();
    for row in &matrix.rows {
        match row.pats.first() {
            // A catch-all (bare `_` / binding) is already total.
            Some(Pat::Wildcard) => return true,
            Some(Pat::Ctor {
                ctor: PatCtor::SliceLen { fixed, has_rest },
                args,
            }) => {
                if !args.iter().all(|a| matches!(a, Pat::Wildcard)) {
                    continue;
                }
                if *has_rest {
                    open_min = Some(open_min.map_or(*fixed, |m| m.min(*fixed)));
                } else {
                    closed_lens.insert(*fixed);
                }
            }
            _ => {}
        }
    }
    match open_min {
        Some(m) => (0..m).all(|l| closed_lens.contains(&l)),
        None => false,
    }
}

fn is_handled_scrutinee(ty: &Type) -> bool {
    !matches!(
        ty,
        Type::Function { .. }
            | Type::OnceFunction { .. }
            | Type::TypeParam(_)
            | Type::TypeVar(_)
            | Type::AssocProjection { .. }
            | Type::Error
            | Type::Unit
            | Type::Ref(_)
            | Type::MutRef(_)
            | Type::Weak(_)
            | Type::Pointer { .. }
    )
}

fn build_matrix(arms: &[MatchArm], scrut_type: &Type, env: &TypeEnv) -> Matrix {
    let mut rows = Vec::new();
    for arm in arms {
        if arm.guard.is_some() {
            continue;
        }
        let pat = lower_pattern(&arm.pattern, scrut_type, env);
        rows.push(Row { pats: vec![pat] });
    }
    Matrix { rows }
}

fn lower_pattern(p: &Pattern, scrut_type: &Type, env: &TypeEnv) -> Pat {
    match &p.kind {
        PatternKind::Wildcard => Pat::Wildcard,
        PatternKind::Binding(name) => {
            if let Type::Named {
                name: type_name, ..
            } = scrut_type
            {
                if let Some(info) = env.enums.get(type_name) {
                    if info.variants.iter().any(|(v, _)| v == name) {
                        return Pat::Ctor {
                            ctor: PatCtor::Variant(name.clone()),
                            args: vec![],
                        };
                    }
                }
            }
            Pat::Wildcard
        }
        PatternKind::Literal(lit) => match lit {
            LiteralPattern::Bool(b) => Pat::Ctor {
                ctor: PatCtor::Bool(*b),
                args: vec![],
            },
            // Integers and `char`s become singleton `IntRange`s so they
            // share the interval-splitting machinery with range patterns.
            LiteralPattern::Integer(n, _) => Pat::Ctor {
                ctor: PatCtor::IntRange {
                    lo: *n as i128,
                    hi: *n as i128,
                },
                args: vec![],
            },
            LiteralPattern::Char(c) => Pat::Ctor {
                ctor: PatCtor::IntRange {
                    lo: *c as i128,
                    hi: *c as i128,
                },
                args: vec![],
            },
            LiteralPattern::String(s) => Pat::Ctor {
                ctor: PatCtor::Lit(PatLit::String(s.clone())),
                args: vec![],
            },
            // Float patterns lack an Eq/Hash story under f64 — slice 3's
            // type-specific handling models them explicitly. Until then,
            // collapse to wildcard. Float scrutinees are wildcard-required
            // anyway, so this only affects nested float literals (which the
            // is_top_level gate already keeps rare).
            LiteralPattern::Float(_, _) => Pat::Wildcard,
        },
        // Range patterns lower to an inclusive `IntRange` interval. Open
        // ends (`..=hi`, `lo..`) take the scrutinee type's domain bound;
        // for `i128`/`u128` (no `int_domain`) the `i128` extreme is used as
        // a best-effort bound — splitting won't engage there, so only the
        // fact that this is a non-wildcard ctor matters (it correctly
        // refuses to act as a catch-all, unlike the prior wildcard
        // lowering, which made a lone range arm unsoundly "exhaustive").
        PatternKind::RangePattern {
            start,
            end,
            inclusive,
        } => {
            let (dmin, dmax) =
                effective_int_domain(scrut_type, env).unwrap_or((i128::MIN, i128::MAX));
            let lo = start
                .as_ref()
                .and_then(|b| range_bound_to_i128(b, env))
                .unwrap_or(dmin);
            let hi = match end.as_ref() {
                Some(h) => range_bound_to_i128(h, env)
                    .map(|v| if *inclusive { v } else { v - 1 })
                    .unwrap_or(dmax),
                None => dmax,
            };
            Pat::Ctor {
                ctor: PatCtor::IntRange { lo, hi },
                args: vec![],
            }
        }
        PatternKind::TupleVariant { path, patterns } => {
            let name = path.last().cloned().unwrap_or_default();
            let payload_types = variant_payload_types(scrut_type, &name, env);
            let args = patterns
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let ty = payload_types.get(i).cloned().unwrap_or(Type::Unit);
                    lower_pattern(p, &ty, env)
                })
                .collect();
            Pat::Ctor {
                ctor: PatCtor::Variant(name),
                args,
            }
        }
        PatternKind::Struct {
            path,
            fields,
            has_rest: _, // `lower_struct_fields` treats missing fields as
                         // wildcards, so an explicit `..` already collapses
                         // to the same lowered shape; nothing to do here.
        } => {
            let name = path.last().cloned().unwrap_or_default();
            // Two cases: enum struct-variant under an enum scrutinee, or a
            // plain struct.
            if let Type::Named {
                name: type_name, ..
            } = scrut_type
            {
                if let Some(info) = env.enums.get(type_name) {
                    if let Some((_, vinfo)) = info.variants.iter().find(|(v, _)| v == &name) {
                        let field_decls: Vec<(String, Type)> = match vinfo {
                            VariantTypeInfo::Struct(decls) => decls.clone(),
                            _ => vec![],
                        };
                        let args = lower_struct_fields(&field_decls, fields, env);
                        return Pat::Ctor {
                            ctor: PatCtor::Variant(name),
                            args,
                        };
                    }
                }
            }
            let field_decls: Vec<(String, Type)> = env
                .structs
                .get(&name)
                .map(|info| {
                    info.fields
                        .iter()
                        .map(|(n, t, _)| (n.clone(), t.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let args = lower_struct_fields(&field_decls, fields, env);
            Pat::Ctor {
                ctor: PatCtor::Struct(name),
                args,
            }
        }
        PatternKind::Tuple(patterns) => {
            let elem_types: Vec<Type> = match scrut_type {
                Type::Tuple(elems) => elems.clone(),
                _ => vec![],
            };
            let args = patterns
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let ty = elem_types.get(i).cloned().unwrap_or(Type::Unit);
                    lower_pattern(p, &ty, env)
                })
                .collect();
            Pat::Ctor {
                ctor: PatCtor::Tuple,
                args,
            }
        }
        PatternKind::AtBinding { pattern, .. } => lower_pattern(pattern, scrut_type, env),
        PatternKind::Or(alts) => Pat::Or(
            alts.iter()
                .map(|a| lower_pattern(a, scrut_type, env))
                .collect(),
        ),
        PatternKind::Slice {
            prefix,
            rest,
            suffix,
        } => lower_slice_pattern(prefix, rest, suffix, scrut_type, env),
    }
}

/// Lower a slice pattern according to the scrutinee shape. `Array[T, N]`
/// (literal `N`) specializes like a length-`N` tuple — prefix args at the
/// head, wildcards in the rest range, suffix args at the tail. Open-domain
/// collections (`Vec[T]`, `Slice[T]`, `VecDeque[T]`) lower to a coarse
/// `PatCtor::SliceLen` so the matrix can tell them apart from a true
/// wildcard; `enumerate_ctors` returns `None` for these types so any
/// non-wildcard row demands an explicit wildcard arm for exhaustiveness.
/// Non-literal `Array[T, N]` sizes and other type shapes (the typechecker
/// rejects these) collapse to `Pat::Wildcard` so the matrix recursion
/// doesn't spuriously declare under-coverage on a broken arm.
fn lower_slice_pattern(
    prefix: &[Pattern],
    rest: &Option<RestPattern>,
    suffix: &[Pattern],
    scrut_type: &Type,
    env: &TypeEnv,
) -> Pat {
    let underlying = match scrut_type {
        Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
        other => other,
    };
    match underlying {
        Type::Array { element, size } => {
            let Some(n) = size.as_usize() else {
                return Pat::Wildcard;
            };
            let head = prefix.len();
            let tail = suffix.len();
            if head + tail > n {
                return Pat::Wildcard;
            }
            if rest.is_none() && head + tail != n {
                return Pat::Wildcard;
            }
            let mut args: Vec<Pat> = Vec::with_capacity(n);
            for p in prefix {
                args.push(lower_pattern(p, element, env));
            }
            for _ in 0..(n - head - tail) {
                args.push(Pat::Wildcard);
            }
            for p in suffix {
                args.push(lower_pattern(p, element, env));
            }
            Pat::Ctor {
                ctor: PatCtor::Array(n),
                args,
            }
        }
        Type::Slice { element, .. } => lower_open_slice(prefix, rest, suffix, element, env),
        Type::Named { name, args: targs } if is_open_collection(name) => {
            let element = targs.first().cloned().unwrap_or(Type::Unit);
            lower_open_slice(prefix, rest, suffix, &element, env)
        }
        // Other scrutinee shapes are typechecker-rejected; fall back to
        // wildcard so the matrix doesn't see a malformed row.
        _ => Pat::Wildcard,
    }
}

fn lower_open_slice(
    prefix: &[Pattern],
    rest: &Option<RestPattern>,
    suffix: &[Pattern],
    element: &Type,
    env: &TypeEnv,
) -> Pat {
    let mut args: Vec<Pat> = Vec::with_capacity(prefix.len() + suffix.len());
    for p in prefix.iter().chain(suffix.iter()) {
        args.push(lower_pattern(p, element, env));
    }
    Pat::Ctor {
        ctor: PatCtor::SliceLen {
            fixed: prefix.len() + suffix.len(),
            has_rest: rest.is_some(),
        },
        args,
    }
}

/// Built-in collection types that participate in slice patterns and that
/// the exhaustiveness engine treats as open-domain (no finite constructor
/// set). Per design.md § *Slice and array patterns*, slice patterns apply
/// to `Vec[T]` (positional, contiguous); `VecDeque[T]` is excluded because
/// its ring-buffer storage doesn't admit positional patterns.
fn is_open_collection(name: &str) -> bool {
    matches!(name, "Vec")
}

fn lower_struct_fields(
    field_decls: &[(String, Type)],
    pattern_fields: &[crate::ast::FieldPattern],
    env: &TypeEnv,
) -> Vec<Pat> {
    field_decls
        .iter()
        .map(
            |(fname, fty)| match pattern_fields.iter().find(|f| &f.name == fname) {
                Some(fp) => match &fp.pattern {
                    Some(p) => lower_pattern(p, fty, env),
                    None => Pat::Wildcard,
                },
                None => Pat::Wildcard,
            },
        )
        .collect()
}

fn variant_payload_types(parent_ty: &Type, variant_name: &str, env: &TypeEnv) -> Vec<Type> {
    if let Type::Named {
        name: enum_name,
        args,
    } = parent_ty
    {
        if let Some(info) = env.enums.get(enum_name) {
            if let Some((_, vinfo)) = info.variants.iter().find(|(v, _)| v == variant_name) {
                let raw: Vec<Type> = match vinfo {
                    VariantTypeInfo::Unit => vec![],
                    VariantTypeInfo::Tuple(types) => types.clone(),
                    VariantTypeInfo::Struct(fields) => {
                        fields.iter().map(|(_, t)| t.clone()).collect()
                    }
                };
                // Substitute the enum's generic params with the
                // instantiated args so a payload declared in terms of a
                // type parameter (`Result`'s `Ok(T)`) resolves to its
                // concrete type — `Ok(())` over `Result[(), E]` becomes a
                // `Unit` column rather than an open `TypeParam("T")` one.
                // Without this, `enumerate_ctors` treats the payload as an
                // open domain and the match reads as non-exhaustive (the
                // `Ok(())` / unit single-inhabitant case especially), and
                // codegen inherits the same mis-analysis. design.md
                // § Pattern Exhaustiveness.
                if info.generic_params.is_empty() || args.is_empty() {
                    return raw;
                }
                let subs: std::collections::HashMap<String, Type> = info
                    .generic_params
                    .iter()
                    .cloned()
                    .zip(args.iter().cloned())
                    .collect();
                return raw.iter().map(|t| subst_type_params(t, &subs)).collect();
            }
        }
    }
    vec![]
}

/// Substitute `Type::TypeParam(name)` occurrences with their mapped
/// concrete types, recursing into the param-bearing `Type` variants.
/// Param-free / exotic variants pass through unchanged. Local to the
/// exhaustiveness module so it stays free of the typechecker's
/// `SubstValue`-keyed inference substitution.
fn subst_type_params(ty: &Type, subs: &std::collections::HashMap<String, Type>) -> Type {
    match ty {
        Type::TypeParam(name) => subs.get(name).cloned().unwrap_or_else(|| ty.clone()),
        Type::Named { name, args } => Type::Named {
            name: name.clone(),
            args: args.iter().map(|a| subst_type_params(a, subs)).collect(),
        },
        Type::Tuple(elems) => {
            Type::Tuple(elems.iter().map(|e| subst_type_params(e, subs)).collect())
        }
        Type::Rc(inner) => Type::Rc(Box::new(subst_type_params(inner, subs))),
        Type::Arc(inner) => Type::Arc(Box::new(subst_type_params(inner, subs))),
        Type::Ref(inner) => Type::Ref(Box::new(subst_type_params(inner, subs))),
        Type::MutRef(inner) => Type::MutRef(Box::new(subst_type_params(inner, subs))),
        Type::Slice { element, mutable } => Type::Slice {
            element: Box::new(subst_type_params(element, subs)),
            mutable: *mutable,
        },
        Type::Array { element, size } => Type::Array {
            element: Box::new(subst_type_params(element, subs)),
            size: size.clone(),
        },
        Type::Vector { element, lanes } => Type::Vector {
            element: Box::new(subst_type_params(element, subs)),
            lanes: lanes.clone(),
        },
        _ => ty.clone(),
    }
}

/// Maranget's `U(P, q)`, witness-producing variant. Returns `None` if `q`
/// is fully covered by `matrix`; returns `Some(witness)` where `witness`
/// is a vector of patterns (one per column of `q`) describing a value
/// matching `q` but missed by every row of `matrix`.
///
/// `head_types[i]` is the type of column `i` so that wildcard recursion at
/// column 0 enumerates the right constructor set.
fn usefulness(matrix: &Matrix, q: &[Pat], head_types: &[Type], env: &TypeEnv) -> Option<Vec<Pat>> {
    if q.is_empty() {
        return matrix.rows.is_empty().then(Vec::new);
    }

    // Maranget base case `U(∅, q)`: an empty matrix covers nothing, so every
    // value of the remaining columns is uncovered — the query is useful with an
    // all-wildcard witness. Without this, a wildcard column over a *recursive*
    // enum reached after specialization empties the matrix (e.g. the
    // `unreachable_arms` check on the `Add` arm of `match e { Num(n) => …,
    // Add(a, b) => … }` over `shared enum Expr { Add(Expr, Expr), Num(i64) }`,
    // where specializing on `Add` drops the `Num` row) falls into the wildcard
    // arm below, enumerates `Expr`'s constructors, and descends into the
    // recursive `Add` forever — a compiler stack overflow (B-2026-06-13-10).
    // The bug was order-dependent only by accident: a base-case-first enum
    // happened to short-circuit the constructor loop on the terminating
    // constructor before reaching the recursive one.
    if matrix.rows.is_empty() {
        return Some(vec![Pat::Wildcard; q.len()]);
    }

    let q_head = &q[0];
    let q_rest = &q[1..];
    let head_ty = &head_types[0];
    let rest_tys = &head_types[1..];

    match q_head {
        Pat::Or(alts) => {
            for alt in alts {
                let mut new_q = Vec::with_capacity(q.len());
                new_q.push(alt.clone());
                new_q.extend_from_slice(q_rest);
                if let Some(w) = usefulness(matrix, &new_q, head_types, env) {
                    return Some(w);
                }
            }
            None
        }
        // Integer / `char` column (slice 6): reason about it by interval
        // splitting instead of per-value enumeration. A query range
        // narrows the splitting domain to that interval; a wildcard query
        // splits the whole type domain — but only when the column actually
        // contains range/literal patterns (an all-wildcard int column
        // falls through to the generic wildcard fast-path below for a
        // clean `_` witness). `i128`/`u128` (no `int_domain`) skip this and
        // take the generic open-domain path.
        Pat::Ctor {
            ctor: PatCtor::IntRange { lo, hi },
            ..
        } if effective_int_domain(head_ty, env).is_some() => {
            int_column_useful(matrix, (*lo, *hi), q_rest, rest_tys, env)
        }
        Pat::Wildcard
            if effective_int_domain(head_ty, env).is_some() && matrix_has_int_head(matrix) =>
        {
            let (min, max) = effective_int_domain(head_ty, env).unwrap();
            int_column_useful(matrix, (min, max), q_rest, rest_tys, env)
        }
        Pat::Ctor { ctor, args } => {
            let arity = args.len();
            let specialized = specialize(matrix, ctor, arity);
            let mut new_q = args.clone();
            new_q.extend_from_slice(q_rest);
            // When the pattern's arg count and the ctor's field-type list
            // disagree (e.g. malformed match where a variant pattern is used
            // on a scrutinee of an unrelated type — the typechecker emits
            // its own TypeMismatch in that case), pad/truncate to `arity`
            // so the recursion has matching column counts.
            let mut field_tys = ctor_field_types(ctor, head_ty, env);
            field_tys.resize(arity, Type::Unit);
            let mut new_head_types = field_tys;
            new_head_types.extend_from_slice(rest_tys);
            usefulness(&specialized, &new_q, &new_head_types, env)
                .map(|w| repackage_witness(ctor, arity, w))
        }
        Pat::Wildcard => {
            // Fast path: when every matrix row has a wildcard at the head
            // column, that column carries no constraint. Skip it via the
            // default matrix instead of enumerating constructors and
            // recursing per-field. Without this, an `Array[T, N]` scrutinee
            // (single `PatCtor::Array(N)` constructor with N-arity expansion)
            // builds N-length wildcard vectors at each of N recursion levels
            // — O(N²) memory and time. The let-irrefutability check at
            // `typechecker.rs::is_irrefutable_pattern` exercises this path
            // even for trivial `let name: Array[T, N] = …` bindings, where
            // `lower_pattern` lowers `name` to `Pat::Wildcard`. Skipping
            // changes nothing observable for the irrefutability return value
            // (still `None` when the row covers); witness shape becomes
            // `_` instead of `[_, _, …, _]`, which is strictly more readable
            // at scale. Gated on non-empty matrix so the empty-match witness
            // (e.g. `missing: true` on `match b: bool {}`) keeps its
            // ctor-specific shape.
            if !matrix.rows.is_empty()
                && matrix
                    .rows
                    .iter()
                    .all(|r| matches!(r.pats.first(), Some(Pat::Wildcard)))
            {
                let default = default_matrix(matrix);
                return usefulness(&default, q_rest, rest_tys, env).map(|w| {
                    let mut out = Vec::with_capacity(w.len() + 1);
                    out.push(Pat::Wildcard);
                    out.extend(w);
                    out
                });
            }
            if let Some(all) = enumerate_ctors(head_ty, env) {
                for c in &all {
                    let arity = ctor_arity(c, head_ty, env);
                    let spec = specialize(matrix, c, arity);
                    let mut new_q: Vec<Pat> = (0..arity).map(|_| Pat::Wildcard).collect();
                    new_q.extend_from_slice(q_rest);
                    let mut new_head_types = ctor_field_types(c, head_ty, env);
                    new_head_types.extend_from_slice(rest_tys);
                    if let Some(w) = usefulness(&spec, &new_q, &new_head_types, env) {
                        return Some(repackage_witness(c, arity, w));
                    }
                }
                None
            } else {
                let default = default_matrix(matrix);
                usefulness(&default, q_rest, rest_tys, env).map(|w| {
                    let mut out = Vec::with_capacity(w.len() + 1);
                    out.push(Pat::Wildcard);
                    out.extend(w);
                    out
                })
            }
        }
    }
}

/// Take a sub-witness produced by recursing into `ctor`'s `arity` fields and
/// fold it back into a witness for the parent column: the first `arity`
/// elements of `inner` are the ctor's args, the remainder is the tail of
/// the parent query.
fn repackage_witness(ctor: &PatCtor, arity: usize, inner: Vec<Pat>) -> Vec<Pat> {
    let mut iter = inner.into_iter();
    let args: Vec<Pat> = iter.by_ref().take(arity).collect();
    let tail: Vec<Pat> = iter.collect();
    let mut out = Vec::with_capacity(tail.len() + 1);
    out.push(Pat::Ctor {
        ctor: ctor.clone(),
        args,
    });
    out.extend(tail);
    out
}

/// Head of an integer/`char` matrix column after flattening or-patterns:
/// either a wildcard (matches anything) or a concrete inclusive range.
enum IntHead {
    Wild,
    Range(i128, i128),
}

/// Flatten a matrix's head column into `(IntHead, tail)` pairs, expanding
/// or-patterns into one entry per alternative. Non-int heads (which can't
/// occur in a well-typed integer/`char` column) are dropped.
fn int_rows(matrix: &Matrix) -> Vec<(IntHead, Vec<Pat>)> {
    let mut out = Vec::new();
    for row in &matrix.rows {
        if let Some((head, tail)) = row.pats.split_first() {
            push_int_head(head, tail, &mut out);
        }
    }
    out
}

fn push_int_head(head: &Pat, tail: &[Pat], out: &mut Vec<(IntHead, Vec<Pat>)>) {
    match head {
        Pat::Wildcard => out.push((IntHead::Wild, tail.to_vec())),
        Pat::Ctor {
            ctor: PatCtor::IntRange { lo, hi },
            ..
        } => out.push((IntHead::Range(*lo, *hi), tail.to_vec())),
        Pat::Or(alts) => {
            for alt in alts {
                push_int_head(alt, tail, out);
            }
        }
        // A non-int ctor in an int column is ill-typed; ignore it.
        Pat::Ctor { .. } => {}
    }
}

fn matrix_has_int_head(matrix: &Matrix) -> bool {
    int_rows(matrix)
        .iter()
        .any(|(h, _)| matches!(h, IntHead::Range(_, _)))
}

/// Interval-splitting usefulness for an integer/`char` column over the
/// query interval `[qlo, qhi]`. Partitions the interval at every range
/// endpoint present in the column so each sub-interval is *atomic* (each
/// row range either fully contains or is disjoint from it), then checks
/// each sub-interval in turn: a row range contributes iff it contains the
/// sub-interval, and a wildcard row always contributes. The first
/// sub-interval whose specialized matrix still leaves `q_rest` useful is
/// the witness, repackaged as an `IntRange` head (rendered as a
/// representative value of that interval by `render_int_range`).
///
/// This drives both directions soundly: a gap between/around the ranges
/// specializes to a matrix that omits every range row → uncovered → a
/// missing-value witness (exhaustiveness); and a range fully tiled by the
/// union of earlier ranges has every sub-interval covered → no witness
/// (precise reachability, including union coverage like `1..=10 | 11..=20`
/// subsuming `1..=20`).
fn int_column_useful(
    matrix: &Matrix,
    (qlo, qhi): (i128, i128),
    q_rest: &[Pat],
    rest_tys: &[Type],
    env: &TypeEnv,
) -> Option<Vec<Pat>> {
    if qlo > qhi {
        // Empty query interval (e.g. a degenerate exclusive `5..5`):
        // matches no value, so it contributes no uncovered witness.
        return None;
    }
    let rows = int_rows(matrix);

    // Split points: the query lower bound, plus every range start and
    // every range-end+1 that falls strictly inside the query interval.
    // Each adjacent pair then bounds one atomic sub-interval.
    let mut bounds = vec![qlo];
    for (h, _) in &rows {
        if let IntHead::Range(lo, hi) = h {
            if *lo > qlo && *lo <= qhi {
                bounds.push(*lo);
            }
            if let Some(next) = hi.checked_add(1) {
                if next > qlo && next <= qhi {
                    bounds.push(next);
                }
            }
        }
    }
    bounds.sort_unstable();
    bounds.dedup();

    for (i, &start) in bounds.iter().enumerate() {
        let end = bounds.get(i + 1).map(|n| n - 1).unwrap_or(qhi);
        let mut spec = Matrix::default();
        for (h, tail) in &rows {
            let keep = match h {
                IntHead::Wild => true,
                IntHead::Range(lo, hi) => *lo <= start && end <= *hi,
            };
            if keep {
                spec.rows.push(Row { pats: tail.clone() });
            }
        }
        if let Some(w) = usefulness(&spec, q_rest, rest_tys, env) {
            let mut out = Vec::with_capacity(w.len() + 1);
            out.push(Pat::Ctor {
                ctor: PatCtor::IntRange { lo: start, hi: end },
                args: vec![],
            });
            out.extend(w);
            return Some(out);
        }
    }
    None
}

/// `S(c, P)`: rows that begin with `c(...)` or `_`, with the head replaced by
/// `c`'s arguments (a wildcard row contributes `arity` wildcards).
fn specialize(matrix: &Matrix, ctor: &PatCtor, arity: usize) -> Matrix {
    let mut out = Matrix::default();
    for row in &matrix.rows {
        let Some((head, tail)) = row.pats.split_first() else {
            continue;
        };
        match head {
            Pat::Wildcard => {
                let mut new_pats: Vec<Pat> = (0..arity).map(|_| Pat::Wildcard).collect();
                new_pats.extend_from_slice(tail);
                out.rows.push(Row { pats: new_pats });
            }
            Pat::Ctor {
                ctor: row_ctor,
                args,
            } if row_ctor == ctor => {
                let mut new_pats = args.clone();
                new_pats.extend_from_slice(tail);
                out.rows.push(Row { pats: new_pats });
            }
            Pat::Ctor { .. } => {}
            Pat::Or(alts) => {
                for alt in alts {
                    let mut alt_row = vec![alt.clone()];
                    alt_row.extend_from_slice(tail);
                    let mini = Matrix {
                        rows: vec![Row { pats: alt_row }],
                    };
                    out.rows.extend(specialize(&mini, ctor, arity).rows);
                }
            }
        }
    }
    out
}

/// `D(P)`: rows that begin with `_`, with the head dropped.
fn default_matrix(matrix: &Matrix) -> Matrix {
    let mut out = Matrix::default();
    for row in &matrix.rows {
        let Some((head, tail)) = row.pats.split_first() else {
            continue;
        };
        match head {
            Pat::Wildcard => out.rows.push(Row {
                pats: tail.to_vec(),
            }),
            Pat::Ctor { .. } => {}
            Pat::Or(alts) => {
                for alt in alts {
                    let mut alt_row = vec![alt.clone()];
                    alt_row.extend_from_slice(tail);
                    let mini = Matrix {
                        rows: vec![Row { pats: alt_row }],
                    };
                    out.rows.extend(default_matrix(&mini).rows);
                }
            }
        }
    }
    out
}

fn enumerate_ctors(ty: &Type, env: &TypeEnv) -> Option<Vec<PatCtor>> {
    match ty {
        Type::Bool => Some(vec![PatCtor::Bool(true), PatCtor::Bool(false)]),
        Type::Tuple(_) => Some(vec![PatCtor::Tuple]),
        // Unit `()` is a single-inhabitant type — its sole constructor is
        // the empty tuple. Without this arm Unit fell into the open-domain
        // `_ => None` below, so a `()`-typed column carrying the empty-tuple
        // pattern (e.g. the `Ok(())` payload of `Result[(), E]`) was treated
        // as never fully covered: the default matrix drops the `()` ctor row
        // and a wildcard query falsely reports `Ok(_)` uncovered. The
        // `PatCtor::Tuple` ctor is arity-0 against a Unit parent
        // (`ctor_field_types`), so it matches the empty-tuple pattern exactly.
        Type::Unit => Some(vec![PatCtor::Tuple]),
        // Never has no inhabitants — empty constructor list. Any match
        // (including the empty match) is vacuously exhaustive.
        Type::Never => Some(Vec::new()),
        // Array[T, N] (literal N) has a single fixed-arity constructor —
        // specializes exactly like a length-N tuple per design.md § Pattern
        // Exhaustiveness > Array[T, N]. Non-literal N is rejected upstream
        // (typechecker) and routes through the default-matrix path.
        Type::Array { size, .. } => size.as_usize().map(|n| vec![PatCtor::Array(n)]),
        Type::Named { name, .. } => {
            if let Some(info) = env.enums.get(name) {
                Some(
                    info.variants
                        .iter()
                        .map(|(v, _)| PatCtor::Variant(v.clone()))
                        .collect(),
                )
            } else if is_open_collection(name) {
                // Vec / VecDeque are open-domain — no finite constructor
                // set. Match exhaustiveness requires an explicit wildcard
                // arm; routing through the default-matrix path enforces it.
                None
            } else if env.structs.contains_key(name) {
                Some(vec![PatCtor::Struct(name.clone())])
            } else {
                None
            }
        }
        // Integer / float / char / str / slice — open domains that require
        // a wildcard arm. Returning None routes the wildcard branch of
        // `is_useful` through the default matrix.
        _ => None,
    }
}

fn ctor_arity(ctor: &PatCtor, parent_ty: &Type, env: &TypeEnv) -> usize {
    ctor_field_types(ctor, parent_ty, env).len()
}

fn ctor_field_types(ctor: &PatCtor, parent_ty: &Type, env: &TypeEnv) -> Vec<Type> {
    match ctor {
        PatCtor::Bool(_) | PatCtor::Lit(_) | PatCtor::IntRange { .. } => vec![],
        // Route through `variant_payload_types` so the enum's generic
        // params are substituted with `parent_ty`'s instantiated args
        // (`Ok`'s `T` → `()` for `Result[(), E]`). This is the column
        // head-type the usefulness recursion feeds to `enumerate_ctors`;
        // returning the raw `TypeParam` here (the prior duplicate logic)
        // made every generic-payload column an open domain and broke
        // exhaustiveness for `Ok(())` / finite generic payloads.
        PatCtor::Variant(name) => variant_payload_types(parent_ty, name, env),
        PatCtor::Tuple => match parent_ty {
            Type::Tuple(elems) => elems.clone(),
            _ => vec![],
        },
        PatCtor::Struct(name) => env
            .structs
            .get(name)
            .map(|info| info.fields.iter().map(|(_, t, _)| t.clone()).collect())
            .unwrap_or_default(),
        PatCtor::Array(n) => {
            let element = array_or_collection_element(parent_ty);
            vec![element; *n]
        }
        PatCtor::SliceLen { fixed, .. } => {
            let element = array_or_collection_element(parent_ty);
            vec![element; *fixed]
        }
    }
}

/// Element type for `Array[T, N]`, `Slice[T]`, `Vec[T]`, etc. Used by
/// `PatCtor::Array` / `PatCtor::SliceLen` to thread the right per-column
/// type into Maranget recursion.
fn array_or_collection_element(ty: &Type) -> Type {
    let underlying = match ty {
        Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
        other => other,
    };
    match underlying {
        Type::Array { element, .. } | Type::Slice { element, .. } => (**element).clone(),
        Type::Named { args, .. } => args.first().cloned().unwrap_or(Type::Unit),
        _ => Type::Unit,
    }
}

fn render_witness(witness: &Pat, ty: &Type, env: &TypeEnv) -> String {
    match witness {
        Pat::Wildcard => "_".to_string(),
        // Or shouldn't appear in a witness — the recursion picks one
        // alternative — but render defensively just in case.
        Pat::Or(alts) => alts
            .first()
            .map(|a| render_witness(a, ty, env))
            .unwrap_or_else(|| "_".to_string()),
        Pat::Ctor { ctor, args } => render_ctor(ctor, args, ty, env),
    }
}

fn render_ctor(ctor: &PatCtor, args: &[Pat], ty: &Type, env: &TypeEnv) -> String {
    match ctor {
        PatCtor::Bool(b) => b.to_string(),
        PatCtor::IntRange { lo, hi } => render_int_range(*lo, *hi, ty, env),
        PatCtor::Lit(PatLit::String(s)) => format!("{s:?}"),
        PatCtor::Variant(name) => render_variant(name, args, ty, env),
        PatCtor::Tuple => render_tuple(args, ty, env),
        PatCtor::Struct(name) => render_struct(name, args, env),
        PatCtor::Array(_) => render_slice_witness(args, ty, env, false),
        PatCtor::SliceLen { has_rest, .. } => render_slice_witness(args, ty, env, *has_rest),
    }
}

/// Render a witness for an integer/`char` `IntRange`. Three shapes:
/// - a **singleton** (`lo == hi`) renders as the value (`5`);
/// - a **bounded interior gap** (neither bound is a domain extreme — a true
///   hole between two covered ranges) renders as the range itself
///   (`10..=19`), which tells the user exactly which interval to add;
/// - an **extreme-touching gap** (`lo == MIN` or `hi == MAX` — i.e.
///   "everything outside what you matched") renders a single representative
///   value (`0`, `11`) instead of echoing the giant `MIN`/`MAX` bound.
fn render_int_range(lo: i128, hi: i128, ty: &Type, env: &TypeEnv) -> String {
    if lo == hi {
        return render_scalar(lo, ty);
    }
    // Refinement-aware: a bounded refinement's `[A, B]` domain drives the
    // extreme-touching test, so a gap interior to a finite refinement domain
    // renders as the explicit range rather than a lone representative.
    if let Some((dmin, dmax)) = effective_int_domain(ty, env) {
        if lo != dmin && hi != dmax {
            return format!("{}..={}", render_scalar(lo, ty), render_scalar(hi, ty));
        }
    }
    render_scalar(representative_value(lo, hi, ty, env), ty)
}

/// Render a single integer/`char` value as it would appear in source.
fn render_scalar(v: i128, ty: &Type) -> String {
    if matches!(ty, Type::Char) {
        match u32::try_from(v.clamp(0, 0x10FFFF))
            .ok()
            .and_then(char::from_u32)
        {
            Some(c) if !c.is_control() => format!("'{c}'"),
            _ => format!("'\\u{{{:X}}}'", v.max(0)),
        }
    } else {
        v.to_string()
    }
}

/// Pick a concrete value inside `[lo, hi]` for an extreme-touching gap:
/// prefer `0` when it is in range; otherwise the bound adjacent to a
/// covered region (the non-domain-extreme end), so a gap renders as e.g.
/// `11` (just past a covered range) rather than the domain extreme.
fn representative_value(lo: i128, hi: i128, ty: &Type, env: &TypeEnv) -> i128 {
    if lo <= 0 && 0 <= hi {
        return 0;
    }
    match effective_int_domain(ty, env) {
        Some((dmin, _)) if lo != dmin => lo,
        Some(_) => hi,
        None => lo,
    }
}

fn render_slice_witness(args: &[Pat], ty: &Type, env: &TypeEnv, has_rest: bool) -> String {
    let element = array_or_collection_element(ty);
    let parts: Vec<String> = args
        .iter()
        .map(|a| render_witness(a, &element, env))
        .collect();
    if has_rest {
        if parts.is_empty() {
            "[..]".to_string()
        } else {
            format!("[{}, ..]", parts.join(", "))
        }
    } else {
        format!("[{}]", parts.join(", "))
    }
}

fn render_variant(name: &str, args: &[Pat], ty: &Type, env: &TypeEnv) -> String {
    if args.is_empty() {
        return name.to_string();
    }
    if let Type::Named {
        name: enum_name, ..
    } = ty
    {
        if let Some(info) = env.enums.get(enum_name) {
            if let Some((_, vinfo)) = info.variants.iter().find(|(v, _)| v == name) {
                match vinfo {
                    VariantTypeInfo::Tuple(types) => {
                        let parts: Vec<String> = args
                            .iter()
                            .zip(types.iter().chain(std::iter::repeat(&Type::Unit)))
                            .map(|(a, t)| render_witness(a, t, env))
                            .collect();
                        return format!("{name}({})", parts.join(", "));
                    }
                    VariantTypeInfo::Struct(fields) => {
                        let parts: Vec<String> = args
                            .iter()
                            .zip(fields.iter())
                            .map(|(a, (fname, fty))| {
                                format!("{fname}: {}", render_witness(a, fty, env))
                            })
                            .collect();
                        return format!("{name} {{ {} }}", parts.join(", "));
                    }
                    VariantTypeInfo::Unit => return name.to_string(),
                }
            }
        }
    }
    let parts: Vec<String> = args
        .iter()
        .map(|a| render_witness(a, &Type::Unit, env))
        .collect();
    format!("{name}({})", parts.join(", "))
}

fn render_tuple(args: &[Pat], ty: &Type, env: &TypeEnv) -> String {
    let elem_types: Vec<Type> = match ty {
        Type::Tuple(elems) => elems.clone(),
        _ => Vec::new(),
    };
    let parts: Vec<String> = args
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let t = elem_types.get(i).cloned().unwrap_or(Type::Unit);
            render_witness(a, &t, env)
        })
        .collect();
    format!("({})", parts.join(", "))
}

fn render_struct(name: &str, args: &[Pat], env: &TypeEnv) -> String {
    if let Some(info) = env.structs.get(name) {
        let parts: Vec<String> = args
            .iter()
            .zip(info.fields.iter())
            .map(|(a, (fname, fty, _))| format!("{fname}: {}", render_witness(a, fty, env)))
            .collect();
        return format!("{name} {{ {} }}", parts.join(", "));
    }
    format!("{name} {{ .. }}")
}
