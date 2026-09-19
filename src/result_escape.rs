//! Conservative "does a `let` binding escape?" analysis for the
//! `Result[shared]` scope-exit-RC residual of B-2026-07-12-24.
//!
//! ## Why
//!
//! `track_rc_result_var` (codegen) queues a scope-exit `RcDecOption` that
//! releases a `Result[shared T, E]` binding's payload node. That is correct
//! ONLY when the binding is consumed IN PLACE — a binding that is moved OUT
//! (returned, pushed into a collection, passed to a consuming call, stored in
//! a struct/tuple, captured by a closure, reassigned) hands its `+1` to a
//! second owner, so a producer-side dec would double-free (`Result` has no
//! move-out coordination — the `var_option_shared_heap`-keyed inc/suppress
//! machinery is `Option`-only).
//!
//! This module answers, conservatively, "is binding `name` used ONLY as a
//! direct `match` scrutinee (or unused) within the function body?" — the one
//! shape that is provably consume-in-place. Every OTHER position counts as an
//! escape, so an uncertain use is always classified escaping (leak, never a
//! double-free).
//!
//! ## Soundness
//!
//! The per-`ExprKind` / per-`StmtKind` walks below are **exhaustive matches
//! with no `_` wildcard**, so adding an AST node breaks this file's build
//! rather than silently skipping a position where a value could move out. The
//! count rule is: a name is non-escaping iff its TOTAL `Identifier` uses equal
//! its `match`-scrutinee uses (i.e. it appears nowhere else). Shadowing (an
//! inner `let name = …` of the same identifier) merges counts, which only ever
//! makes the result MORE conservative (an inner use inflates the total →
//! treated as escaping → the binding is left leaking, never double-freed).

use crate::ast::{
    Block, CallArg, Expr, ExprKind, Function, MatchArm, ParsedInterpolationPart, Stmt, StmtKind,
};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Per-binding-name use tally: `(total Identifier uses, uses that are a direct
/// `match` scrutinee, uses that are a READ-ONLY position)`.
#[derive(Default)]
struct Acc<'a> {
    /// B-2026-09-14-5 — the PROJECTION POLICY the `payload_escapers_proj` map is
    /// built under, supplied by the caller. `None` is
    /// [`projection_is_read`], i.e. "every projection is a read", which is what
    /// [`optres_payload_escaping_param_variants_ignoring_projections`] wants.
    ///
    /// The point of making it a parameter is that the SOUND answer needs type
    /// information this module does not have and should not acquire: whether a
    /// projection carries a `Drop` body out depends on its LEAF type, which
    /// only a caller holding the payload's `TypeExpr` and the struct table can
    /// resolve. The walk stays here; the policy comes from whoever can answer
    /// it. See [`optres_payload_escaping_param_variants_with`].
    copy_read: Option<&'a dyn Fn(&Expr) -> bool>,
    counts: HashMap<&'a str, (u32, u32, u32)>,
    /// `(binding name, value-span (offset,length))` for every `Binding`-pattern
    /// `let` / `let…else` encountered — filtered against `counts` after the walk.
    lets: Vec<(&'a str, (usize, usize))>,
    /// B-2026-09-06-48 — param names whose boxed `Option`/`Result` payload is
    /// TAKEN by some `Some`/`Ok`/`Err` arm matching on them directly, keyed by
    /// the VARIANT whose payload it is. Read by
    /// [`optres_payload_consuming_param_variants`]; see that function for why the
    /// question has to be answered from the CALLER's side on the generic path.
    payload_consumers: HashMap<&'a str, HashSet<&'a str>>,
    /// B-2026-09-12-15 — the BODIES sibling of `payload_consumers`, and a
    /// separate map because the two questions want opposite conservatism.
    ///
    /// `payload_consumers` answers "does an arm TAKE the payload", where an
    /// unrecognised or NESTED pattern is reported as taken on purpose: for the
    /// memory question that costs a leak, while the other answer would arm a
    /// second owner. This map answers "does the payload OUTLIVE the call", where
    /// reporting a nested destructure as taken costs a MISSING `Drop` body —
    /// measured on `fn show(x: Option[K]) { match x { Option.Some(K.A(r)) => ..`,
    /// whose arm only reads `r.s` and whose payload enum `K` still owes its own
    /// body to the caller. Reusing the other map silenced three `dK` lines that
    /// `e2e_boxed_enum_payload_param_output_is_unchanged` pins.
    ///
    /// So this one asks `binding_only_borrowed` of every name the pattern binds
    /// AT ANY DEPTH, with no destructure shortcut: a name that is merely read
    /// does not escape however deeply it was bound.
    payload_escapers: HashMap<&'a str, HashSet<&'a str>>,
    /// B-2026-09-13-3 — [`Acc::payload_escapers`] recomputed with PROJECTIONS
    /// treated as reads rather than as partial moves.
    ///
    /// Kept as a second map rather than as a knob on the first because the
    /// choice is per-PARAM and cannot be made here: it is sound only when the
    /// payload type carries its own `impl Drop`, and then only because the
    /// typechecker REJECTS a partial move out of such a struct
    /// (`partial_move_of_drop_struct`, design.md § Part 8). Under that rule any
    /// projection off the payload that compiles at all is necessarily a copy
    /// read, so it cannot carry a body out of the callee. The caller knows the
    /// payload type and picks the map; the walk just supplies both answers.
    payload_escapers_proj: HashMap<&'a str, HashSet<&'a str>>,
    /// B-2026-09-14-18 — [`Acc::payload_escapers_proj`] read PER PART, for the
    /// one payload shape that has parts: a TUPLE destructured element-wise by
    /// the arm (`Some((a, b))`).
    ///
    /// The two maps above collapse an arm to one bit per variant, which is the
    /// whole defect this answers. `Some((a, b)) => return b` escapes part 1 and
    /// nothing else, but a bit per variant can only say "Some escapes", so the
    /// caller stands the WHOLE payload down and `a`'s owed `Drop` body is run
    /// by nobody — on every surface, so no A/B comparison can see it.
    ///
    /// ABSENCE IS NOT "nothing escapes": it means the arm is not an
    /// element-wise tuple destructure and has no parts to speak of, so the
    /// caller must fall back to the all-or-nothing maps. A present entry whose
    /// set is EMPTY cannot occur — an arm that escapes no part is never
    /// recorded in `payload_escapers_proj` either, so there is nothing for the
    /// caller to narrow.
    ///
    /// Recorded beside `payload_escapers_proj` and under the same copy-read
    /// policy, at every one of its four sites, so the two cannot disagree about
    /// which names count as escaping.
    payload_escaper_parts: HashMap<&'a str, HashMap<&'a str, BTreeSet<usize>>>,
    /// True while walking inside a closure body. A reference to an OUTER binding
    /// there is a CAPTURE — an escape into an env that can outlive the binding's
    /// scope — so `match`-scrutinee safety is suppressed (even `match d` inside a
    /// closure counts `d` as escaping). Without this a captured `Result[shared]`
    /// would get a producer-side dec that use-after-frees the escaping closure's
    /// env. Closure-local bindings are only ever made MORE conservative by this.
    in_closure: bool,
}

/// Value-spans of every `let <Binding> = <value>` in `func` whose binding name
/// never escapes (see module docs). Keyed by `(value.span.offset,
/// value.span.length)` — the same key codegen's let-statement handler uses.
/// Run on the POST-lowering AST (codegen's view) so the recorded spans match
/// the nodes the handler sees.
pub fn nonescaping_let_value_spans(func: &Function) -> HashSet<(usize, usize)> {
    let mut acc = Acc::default();
    walk_block(&func.body, &mut acc);
    let mut out = HashSet::new();
    for (name, span) in &acc.lets {
        let (total, scrut, _) = acc.counts.get(name).copied().unwrap_or((0, 0, 0));
        if total == scrut {
            out.insert(*span);
        }
    }
    out
}

/// Names of `func`'s PARAMETERS that never escape the body — used only as a
/// direct `match` scrutinee, or unused (same rule as [`nonescaping_let_value_spans`]).
/// An OWNED `Result[shared]` param that is consumed in place owns the caller's
/// transferred `+1` and can safely release it at scope exit; a forwarded param
/// (passed on to another consuming call / returned) escapes → left out → the
/// terminal consumer's dec stays the only one. Borrowed (`ref`) params are the
/// caller's to drop; the codegen param site excludes them by type separately.
pub fn nonescaping_param_names(func: &Function) -> HashSet<String> {
    let mut acc = Acc::default();
    walk_block(&func.body, &mut acc);
    func.params
        .iter()
        .filter_map(|p| {
            let crate::ast::PatternKind::Binding(name) = &p.pattern.kind else {
                return None;
            };
            let (total, scrut, _) = acc.counts.get(name.as_str()).copied().unwrap_or((0, 0, 0));
            (total == scrut).then(|| name.clone())
        })
        .collect()
}

/// Names of `func`'s PARAMETERS that never escape the frame BY VALUE — used
/// only as a direct `match` scrutinee, in a read-only position, or unused.
///
/// The looser sibling of [`nonescaping_param_names`], and the difference is one
/// position: a bare-identifier interpolation hole (`f"{x}"`). That reads the
/// value and cannot move it, but the stricter predicate counts every
/// non-scrutinee use as an escape, so a callee whose whole body is
/// `println(f"{x}")` was classified escaping.
///
/// THAT CLASSIFICATION IS LOAD-BEARING IN TWO PLACES AT ONCE, which is why this
/// exists as its own function rather than as a relaxation of the other one
/// (whose `Result[shared]` RC consumer needs the strict rule — see its doc for
/// the `takeout` shape that fools anything looser). The by-value
/// `Option`/`Result` param entry copy (`compile_function`) is emitted only for
/// a non-escaping param, and the CALLER's ownership of a fresh temp argument
/// (`callee_optres_param_entry_copied`) must answer the same question or the
/// two disagree: the caller believing a copy was made when it was not is a
/// LEAK, and believing one was not made when it was is a DOUBLE FREE. Both were
/// live (B-2026-09-01-29, B-2026-09-01-35). Passing this one set to both sites
/// makes them agree by construction.
///
/// The conservative direction is unchanged and still the safe one: an
/// unrecognised position counts as an escape, which costs a leak and never
/// memory unsafety.
pub fn by_value_nonescaping_param_names(func: &Function) -> HashSet<String> {
    let mut acc = Acc::default();
    walk_block(&func.body, &mut acc);
    func.params
        .iter()
        .filter_map(|p| {
            let crate::ast::PatternKind::Binding(name) = &p.pattern.kind else {
                return None;
            };
            let (total, scrut, ro) = acc.counts.get(name.as_str()).copied().unwrap_or((0, 0, 0));
            (total == scrut + ro).then(|| name.clone())
        })
        .collect()
}

/// Names of `func`'s PARAMETERS that appear NOWHERE in the body.
///
/// Strictly stronger than [`nonescaping_param_names`], which also admits a param
/// used only as a direct `match` scrutinee. That relaxation is right for the RC
/// residual it was written for — a scrutinee is consumed in place, so releasing
/// it is safe — but it is NOT safe for a caller that wants to FREE the argument's
/// buffer after the call, because a match arm can bind the scrutinee and hand it
/// straight back out:
///
/// ```text
/// fn takeout[T](b: T) -> T { match b { v => { return v; } } }
/// ```
///
/// `b` is counted as a scrutinee use and `v` is a different name, so
/// `nonescaping_param_names` calls `b` non-escaping — yet the buffer the caller
/// passed in is exactly what the caller then binds as the RESULT. Freeing the
/// argument on top of that is a double free, not a leak. `total == 0` cannot be
/// fooled that way: a param the body never names cannot reach any position at
/// all. B-2026-08-15-9 uses this one for that reason.
pub fn unused_param_names(func: &Function) -> HashSet<String> {
    let mut acc = Acc::default();
    walk_block(&func.body, &mut acc);
    func.params
        .iter()
        .filter_map(|p| {
            let crate::ast::PatternKind::Binding(name) = &p.pattern.kind else {
                return None;
            };
            let (total, _, _) = acc.counts.get(name.as_str()).copied().unwrap_or((0, 0, 0));
            (total == 0).then(|| name.clone())
        })
        .collect()
}

/// B-2026-09-12-15 — for each PARAM of `func`, the seeded-pair VARIANTS whose
/// payload OUTLIVES the call: bound out by an arm and then moved somewhere the
/// call does not own (pushed into a `mut ref` accumulator, returned, stored).
///
/// The gate in front of the caller-side payload-BODIES registration. A param can
/// be non-escaping while its payload escapes, and then whatever received the
/// payload runs the body — so registering in the caller as well runs it twice.
/// Measured at `-O0`: `match x { Some(r) => acc.push(r) .. }` printed
/// `dR1 / len:1 / dR1` compiled against one body on `--interp`, and
/// `match x { Some(r) => return r .. }` printed `k:1 / dR1 / end / dR1`.
///
/// NOT [`optres_payload_consuming_param_variants`], whose conservatism runs the
/// other way — see `Acc::payload_escapers` for the fixture that separates them.
///
/// [`optres_payload_escaping_param_variants_ignoring_projections`] answers the
/// same question for a payload that cannot be partially moved.
pub fn optres_payload_escaping_param_variants(func: &Function) -> HashMap<String, HashSet<String>> {
    let mut acc = Acc::default();
    walk_block(&func.body, &mut acc);
    func.params
        .iter()
        .filter_map(|p| {
            let crate::ast::PatternKind::Binding(name) = &p.pattern.kind else {
                return None;
            };
            acc.payload_escapers.get(name.as_str()).map(|vs| {
                (
                    name.clone(),
                    vs.iter()
                        .map(|v| (*v).to_string())
                        .collect::<HashSet<String>>(),
                )
            })
        })
        .collect()
}

/// B-2026-09-14-5 — [`optres_payload_escaping_param_variants`] under a
/// CALLER-SUPPLIED projection policy.
///
/// The two fixed policies this module already exposes are the ends of one
/// axis: `optres_payload_escaping_param_variants` calls every projection off
/// the payload binding an escape, and `..._ignoring_projections` calls none of
/// them one. Both are wrong for a TUPLE payload, and in opposite directions —
/// `t.0` over `Option[(R, i64)]` really does move `R` out and hand its body to
/// the receiver, while `t.1` and `t.0.id` are copy reads that take nothing.
/// Picking either fixed policy therefore loses a body or doubles one; measured
/// as `got end` where `dR5 got end` is due (B-2026-09-14-5), and as
/// `dR1 / len:1 / dR1` in the other direction (B-2026-09-12-15).
///
/// The sound answer depends on the projection's LEAF TYPE, which this module
/// deliberately cannot see: it is a plain-AST analysis with no type table, and
/// giving it one would drag the struct/field environment through every caller.
/// So the WALK stays here and the POLICY is passed in by a caller that can
/// resolve leaves — `codegen` holds the payload `TypeExpr` and the struct field
/// tables and answers "does this projection's leaf carry a `Drop` body".
///
/// `copy_read` is asked of each projection expression and answers `true` when
/// it is a READ that carries nothing away. Returning `false` for everything
/// reproduces `optres_payload_escaping_param_variants`; returning `true` for
/// every projection reproduces `..._ignoring_projections`, so both existing
/// entry points are special cases of this one and are kept as the named,
/// obviously-correct spellings of their ends.
pub fn optres_payload_escaping_param_variants_with(
    func: &Function,
    copy_read: &dyn Fn(&Expr) -> bool,
) -> HashMap<String, HashSet<String>> {
    let mut acc = Acc {
        copy_read: Some(copy_read),
        ..Default::default()
    };
    walk_block(&func.body, &mut acc);
    func.params
        .iter()
        .filter_map(|p| {
            let crate::ast::PatternKind::Binding(name) = &p.pattern.kind else {
                return None;
            };
            acc.payload_escapers_proj.get(name.as_str()).map(|vs| {
                (
                    name.clone(),
                    vs.iter()
                        .map(|v| (*v).to_string())
                        .collect::<HashSet<String>>(),
                )
            })
        })
        .collect()
}

/// B-2026-09-14-18 — [`optres_payload_escaping_param_variants_with`] answered
/// PER PART, for the one payload shape that has parts.
///
/// Keyed param -> variant -> the positional indices of a TUPLE payload that the
/// callee lets outlive the call. A param or variant MISSING from this map has
/// no per-part answer (the arm is not an element-wise tuple destructure), which
/// is a different statement from "escapes nothing" — see
/// `Acc::payload_escaper_parts`. A caller must therefore treat absence as
/// "fall back to the all-or-nothing map", never as an empty escape set.
///
/// Computed under the SAME `copy_read` policy as the map it narrows and
/// recorded at the same four sites, so the two agree by construction about
/// which names escape. The narrowing is only ever a REFINEMENT: this map can
/// say "of the parts Some escapes, only part 1 does", and cannot say that Some
/// escapes when the other map says it does not.
///
/// WHY THIS EXISTS. `Some((a, b)) => return b` over `Option[(R, i64)]` escapes
/// part 1 and nothing else, but a bit per variant can only say "Some escapes",
/// so the caller stood the whole payload down and `a`'s owed `Drop` body was
/// run by nobody. Measured `got:9 end` against a due `dR5 got:9 end`, on all
/// four surfaces — which is why no A/B comparison between the backends could
/// ever see it.
pub fn optres_payload_escaping_param_variant_parts_with(
    func: &Function,
    copy_read: &dyn Fn(&Expr) -> bool,
) -> HashMap<String, HashMap<String, BTreeSet<usize>>> {
    let mut acc = Acc {
        copy_read: Some(copy_read),
        ..Default::default()
    };
    walk_block(&func.body, &mut acc);
    func.params
        .iter()
        .filter_map(|p| {
            let crate::ast::PatternKind::Binding(name) = &p.pattern.kind else {
                return None;
            };
            acc.payload_escaper_parts.get(name.as_str()).map(|vs| {
                (
                    name.clone(),
                    vs.iter()
                        .map(|(v, parts)| ((*v).to_string(), parts.clone()))
                        .collect::<HashMap<String, BTreeSet<usize>>>(),
                )
            })
        })
        .collect()
}

/// B-2026-09-13-3 — [`optres_payload_escaping_param_variants`] for a payload
/// that CANNOT be partially moved, so a projection off it is always a read.
///
/// Ask this one only when the payload type carries its own `impl Drop`. That is
/// what makes it sound, and the guarantee comes from the typechecker rather
/// than from this walk: `partial_move_of_drop_struct` (design.md § Part 8)
/// REJECTS moving a field out of a `Drop`-bearing struct, so every projection
/// that reaches codegen at all is a copy and carries no body away.
///
/// Asking it for any OTHER payload would be wrong in the expensive direction.
/// Measured: `fn eat(o: Option[Holder2]) -> Inner { match o { Some(t) => return
/// t.inner .. } }`, where `Holder2` has no `Drop` of its own and `Inner` does,
/// legally moves the field out — the returned value owns that body, and
/// treating the projection as a read would register a second one in the caller.
/// The tuple-payload spelling (`return t.0`) is the same hazard.
pub fn optres_payload_escaping_param_variants_ignoring_projections(
    func: &Function,
) -> HashMap<String, HashSet<String>> {
    let mut acc = Acc::default();
    walk_block(&func.body, &mut acc);
    func.params
        .iter()
        .filter_map(|p| {
            let crate::ast::PatternKind::Binding(name) = &p.pattern.kind else {
                return None;
            };
            acc.payload_escapers_proj.get(name.as_str()).map(|vs| {
                (
                    name.clone(),
                    vs.iter()
                        .map(|v| (*v).to_string())
                        .collect::<HashSet<String>>(),
                )
            })
        })
        .collect()
}

/// B-2026-09-06-48 — for each PARAM of `func`, the seeded-pair VARIANTS whose
/// boxed payload is TAKEN by an arm matching on that param directly.
///
/// The caller-side twin of codegen's `boxed_tuple_payload_arm_takes_ownership`,
/// and it exists as a separate AST-level predicate because on the GENERIC path
/// the codegen one cannot be reached in time. `compile_generic_call` owns a
/// fresh-temp `Option`/`Result` argument's box (`track_boxed_optres_arg_temp`,
/// B-2026-09-02-46) from the CALLER's frame, while the arm that would retract
/// an inner drop runs inside the monomorph body — and that body is compiled
/// with `scope_cleanup_actions` SWAPPED, so `clear_boxed_enum_inner_drop`
/// cannot see the caller's action at all. There is no retraction path, which
/// is why the caller has to decide BEFORE it arms anything.
///
/// Asking it of the callee's AST makes the answer a property of the callee
/// rather than of the call, so two call sites of one monomorph cannot disagree
/// and nothing has to be cached per instantiation.
///
/// PER VARIANT, not per param, and that is load-bearing rather than tidy: a
/// `Result`'s two variants have different payloads and only one of them is the
/// boxed tuple. `Err(e) => { return e; }` takes the `i64` and says nothing
/// about the `Ok` tuple, so collapsing the two lost the `Ok` interior again —
/// 20 B in 4 blocks, measured, on a fixture whose `Err` arm merely returned
/// its binding.
///
/// Conservative in the safe direction, like every other predicate here: an
/// unrecognised arm shape counts as NOT taking the payload, which leaves
/// today's leak rather than arming a second owner of it.
pub fn optres_payload_consuming_param_variants(
    func: &Function,
) -> HashMap<String, HashSet<String>> {
    let mut acc = Acc::default();
    walk_block(&func.body, &mut acc);
    func.params
        .iter()
        .filter_map(|p| {
            let crate::ast::PatternKind::Binding(name) = &p.pattern.kind else {
                return None;
            };
            acc.payload_consumers.get(name.as_str()).map(|vs| {
                (
                    name.clone(),
                    vs.iter()
                        .map(|v| (*v).to_string())
                        .collect::<HashSet<String>>(),
                )
            })
        })
        .collect()
}

/// The VARIANT a `Some`/`Ok`/`Err` arm TAKES the payload of, rather than only
/// reading it. Mirrors codegen's `boxed_tuple_payload_arm_takes_ownership`
/// (a per-element destructure always takes; a whole-payload binding takes
/// unless every use in guard and body only borrows), minus that function's
/// `pattern_binding_types == "Tuple"` test — the caller applies the tuple
/// restriction itself, from the instantiated type, which is where it is known
/// exactly on this path.
fn variant_arm_takes_payload<'a>(
    pattern: &'a crate::ast::Pattern,
    guard: Option<&Expr>,
    body: &Expr,
) -> Option<&'a str> {
    let (variant, binds) = variant_payload_binds(pattern)?;
    match binds {
        // A per-element destructure (`Some((a, b))`) gives every leaf its own
        // owner unconditionally.
        None => Some(variant),
        Some(names) => (!names.iter().all(|v| {
            crate::consume_class::binding_only_borrowed(v, body)
                && guard.is_none_or(|g| crate::consume_class::binding_only_borrowed(v, g))
        }))
        .then_some(variant),
    }
}

/// B-2026-09-12-15 — the VARIANT whose payload an arm lets OUTLIVE the call,
/// feeding `Acc::payload_escapers`.
///
/// Differs from [`variant_arm_takes_payload`] in exactly one way, and that way
/// is the point: the variant is read off the pattern even when its sub-patterns
/// are NESTED, and the borrow test then runs over every name the pattern binds
/// at any depth. A nested destructure whose leaves are only read lets nothing
/// outlive the call.
///
/// A pattern that binds NOTHING (`Some(_)`) escapes nothing. An unrecognised
/// spelling has no names to test and so falls in the same bucket — which is the
/// conservative direction HERE, since declining to register is the status quo
/// rather than a second body.
fn variant_arm_payload_escapes<'a>(
    pattern: &'a crate::ast::Pattern,
    guard: Option<&Expr>,
    body: &Expr,
) -> Option<&'a str> {
    let variant = optres_variant_of_pattern(pattern)?;
    let names = pattern.binding_names();
    if names.is_empty() {
        return None;
    }
    (!names.iter().all(|v| {
        crate::consume_class::binding_only_borrowed(v, body)
            && guard.is_none_or(|g| crate::consume_class::binding_only_borrowed(v, g))
    }))
    .then_some(variant)
}

/// B-2026-09-13-3 — a projection is a READ. See `Acc::payload_escapers_proj`
/// for the rule that makes this sound at its one caller: a payload whose type
/// has its own `impl Drop` cannot be partially moved, so a projection off it
/// that compiles is always a copy.
///
/// A bare identifier is deliberately NOT covered: moving the payload WHOLE
/// (`return r`, `acc.push(r)`) still hands the body onward and still has to
/// stand the caller down.
fn projection_is_read(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. } | ExprKind::Index { .. }
    )
}

/// [`variant_arm_payload_escapes`] with projections treated as reads.
fn variant_arm_payload_escapes_proj<'a>(
    pattern: &'a crate::ast::Pattern,
    guard: Option<&Expr>,
    body: &Expr,
    copy_read: &dyn Fn(&Expr) -> bool,
) -> Option<&'a str> {
    let variant = optres_variant_of_pattern(pattern)?;
    let names = pattern.binding_names();
    if names.is_empty() {
        return None;
    }
    (!names.iter().all(|v| {
        crate::consume_class::binding_only_borrowed_with(v, body, copy_read)
            && guard.is_none_or(|g| {
                crate::consume_class::binding_only_borrowed_with(v, g, &projection_is_read)
            })
    }))
    .then_some(variant)
}

/// B-2026-09-14-18 — [`variant_arm_payload_escapes_proj`] answered PER PART.
///
/// Returns the variant and the positional indices of a TUPLE payload's parts
/// that the arm lets outlive the call, or `None` when the arm is not an
/// element-wise tuple destructure — there being no parts to speak of then, and
/// `None` meaning exactly that rather than "escapes nothing" (see
/// `Acc::payload_escaper_parts`).
///
/// A WILDCARD position binds no name and so can escape nothing, which is the
/// answer that makes `Some((_, b)) => return b` keep part 0's body: the arm
/// never names it, so nothing can carry it out.
///
/// Uses the SAME `binding_only_borrowed_with` test, under the same policy, as
/// the map this narrows — so a part is in this set exactly when its name is
/// one of the names that put the variant in `payload_escapers_proj`.
fn variant_arm_payload_escaping_parts<'a>(
    pattern: &'a crate::ast::Pattern,
    guard: Option<&Expr>,
    body: &Expr,
    copy_read: &dyn Fn(&Expr) -> bool,
) -> Option<(&'a str, BTreeSet<usize>)> {
    let variant = optres_variant_of_pattern(pattern)?;
    let parts = tuple_payload_binding_parts(pattern)?;
    let escaping: BTreeSet<usize> = parts
        .iter()
        .enumerate()
        .filter_map(|(i, name)| {
            let v = (*name)?;
            let borrowed = crate::consume_class::binding_only_borrowed_with(v, body, copy_read)
                && guard.is_none_or(|g| {
                    crate::consume_class::binding_only_borrowed_with(v, g, &projection_is_read)
                });
            (!borrowed).then_some(i)
        })
        .collect();
    Some((variant, escaping))
}

/// Block sibling of [`variant_arm_payload_escaping_parts`].
fn variant_arm_payload_escaping_parts_block<'a>(
    pattern: &'a crate::ast::Pattern,
    block: Option<&Block>,
    copy_read: &dyn Fn(&Expr) -> bool,
) -> Option<(&'a str, BTreeSet<usize>)> {
    let variant = optres_variant_of_pattern(pattern)?;
    let parts = tuple_payload_binding_parts(pattern)?;
    // `None` for the block is `let`-else: the bindings outlive the construct,
    // so every NAMED part escapes. Wildcards still bind nothing.
    let escaping: BTreeSet<usize> = parts
        .iter()
        .enumerate()
        .filter_map(|(i, name)| {
            let v = (*name)?;
            match block {
                None => Some(i),
                Some(b) => {
                    (!crate::consume_class::binding_only_borrowed_block_with(v, b, copy_read))
                        .then_some(i)
                }
            }
        })
        .collect();
    Some((variant, escaping))
}

/// The positional binding names of a payload destructured ELEMENT-WISE as a
/// tuple (`Some((a, _, c))`), one entry per tuple position and `None` where the
/// position is a wildcard.
///
/// `None` for anything else, and the exclusions are what keep the per-part
/// answer honest rather than merely available:
///
///   - `Some(t)` binds the payload WHOLE; it has one part, itself, and the
///     all-or-nothing maps already say the right thing about it.
///   - a nested sub-pattern at any position (`Some((K.A(r), b))`) binds names
///     this cannot map back to a single tuple index, so there is no per-part
///     answer to give and the caller must keep its old behaviour.
///
/// Both fall out of requiring every inner pattern to be a plain `Binding` or
/// `Wildcard`, which is the same bar [`variant_payload_binds`] sets one level
/// up for the same reason.
fn tuple_payload_binding_parts(pattern: &crate::ast::Pattern) -> Option<Vec<Option<&str>>> {
    let crate::ast::PatternKind::TupleVariant { patterns, .. } = &pattern.kind else {
        return None;
    };
    let [only] = patterns.as_slice() else {
        return None;
    };
    let crate::ast::PatternKind::Tuple(elems) = &only.kind else {
        return None;
    };
    elems
        .iter()
        .map(|p| match &p.kind {
            crate::ast::PatternKind::Binding(n) => Some(Some(n.as_str())),
            crate::ast::PatternKind::Wildcard => Some(None),
            _ => None,
        })
        .collect()
}

/// Block sibling of [`variant_arm_payload_escapes_proj`].
fn variant_arm_payload_escapes_proj_block<'a>(
    pattern: &'a crate::ast::Pattern,
    block: Option<&Block>,
    copy_read: &dyn Fn(&Expr) -> bool,
) -> Option<&'a str> {
    let variant = optres_variant_of_pattern(pattern)?;
    let names = pattern.binding_names();
    if names.is_empty() {
        return None;
    }
    match block {
        None => Some(variant),
        Some(b) => (!names
            .iter()
            .all(|v| crate::consume_class::binding_only_borrowed_block_with(v, b, copy_read)))
        .then_some(variant),
    }
}

/// Block sibling of [`variant_arm_payload_escapes`]. `None` for the block means
/// the bindings escape the construct entirely (`let`-else), so they always do.
fn variant_arm_payload_escapes_block<'a>(
    pattern: &'a crate::ast::Pattern,
    block: Option<&Block>,
) -> Option<&'a str> {
    let variant = optres_variant_of_pattern(pattern)?;
    let names = pattern.binding_names();
    if names.is_empty() {
        return None;
    }
    match block {
        None => Some(variant),
        Some(b) => (!names
            .iter()
            .all(|v| crate::consume_class::binding_only_borrowed_block(v, b)))
        .then_some(variant),
    }
}

/// The seeded-pair variant a pattern matches, nested sub-patterns included.
/// [`variant_payload_binds`] answers this too but folds it together with its
/// take/not-take verdict, which the escape question needs to reach separately.
fn optres_variant_of_pattern(pattern: &crate::ast::Pattern) -> Option<&str> {
    let crate::ast::PatternKind::TupleVariant { path, .. } = &pattern.kind else {
        return None;
    };
    match path.last().map(|s| s.as_str()) {
        Some(v @ ("Some" | "Ok" | "Err")) => Some(v),
        _ => None,
    }
}

/// Block-scoped sibling of [`variant_arm_takes_payload`], for `if let` /
/// `while let` bodies. `None` for the block means the bindings escape the
/// construct entirely (`let`-else), so they always take.
fn variant_arm_takes_payload_block<'a>(
    pattern: &'a crate::ast::Pattern,
    block: Option<&Block>,
) -> Option<&'a str> {
    let (variant, binds) = variant_payload_binds(pattern)?;
    match (binds, block) {
        (None, _) => Some(variant),
        (Some(_), None) => Some(variant),
        (Some(names), Some(b)) => (!names
            .iter()
            .all(|v| crate::consume_class::binding_only_borrowed_block(v, b)))
        .then_some(variant),
    }
}

/// The variant name and payload bindings of a `Some`/`Ok`/`Err` pattern: the
/// bindings are `None` when the pattern DESTRUCTURES the payload into elements
/// (which always takes it), `Some(names)` for whole-payload bindings, and the
/// whole result is `None` for a pattern this predicate does not recognise (a
/// wildcard `Some(_)` included, which binds nothing and therefore takes
/// nothing).
fn variant_payload_binds(pattern: &crate::ast::Pattern) -> Option<(&str, Option<Vec<&str>>)> {
    let crate::ast::PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
        return None;
    };
    let variant = match path.last().map(|s| s.as_str()) {
        Some(v @ ("Some" | "Ok" | "Err")) => v,
        _ => return None,
    };
    if patterns
        .iter()
        .any(|p| matches!(&p.kind, crate::ast::PatternKind::Tuple(_)))
    {
        return Some((variant, None));
    }
    // Anything that is neither a plain `Binding` nor a `Wildcard` binds the
    // payload by a spelling this predicate cannot follow — `Some(t @ ..)`,
    // an or-pattern, a struct/slice sub-pattern. Report it as TAKEN rather
    // than as unrecognised: the two answers differ in which direction the
    // uncertainty falls, and "taken" costs the leak this row is fixing while
    // "not taken" would arm a second owner of a payload the arm may consume.
    if patterns.iter().any(|p| {
        !matches!(
            &p.kind,
            crate::ast::PatternKind::Binding(_) | crate::ast::PatternKind::Wildcard
        )
    }) {
        return Some((variant, None));
    }
    let names: Vec<&str> = patterns
        .iter()
        .filter_map(|p| match &p.kind {
            crate::ast::PatternKind::Binding(n) => Some(n.as_str()),
            _ => None,
        })
        .collect();
    // All wildcards: binds nothing, so takes nothing.
    if names.is_empty() {
        return None;
    }
    Some((variant, Some(names)))
}

fn record_use<'a>(acc: &mut Acc<'a>, name: &'a str, scrutinee: bool) {
    let e = acc.counts.entry(name).or_insert((0, 0, 0));
    e.0 += 1;
    if scrutinee {
        e.1 += 1;
    }
}

/// Record a use in a position that reads the value and cannot move it out —
/// today, a bare-identifier interpolation hole (`f"{x}"`). Counted in the total
/// like every other use, and additionally in the read-only slot, so
/// [`by_value_nonescaping_param_names`] can subtract it while
/// [`nonescaping_param_names`] (which never looks at that slot) keeps its
/// stricter answer unchanged.
fn record_read_only_use<'a>(acc: &mut Acc<'a>, name: &'a str) {
    let e = acc.counts.entry(name).or_insert((0, 0, 0));
    e.0 += 1;
    e.2 += 1;
}

/// Walk a pattern-matching CONSTRUCT's scrutinee (`match` / `if let` / `while
/// let` / `let…else` value). A bare `Identifier(n)` scrutinee is a
/// consume-in-place use (counted as a scrutinee use — safe), UNLESS inside a
/// closure where referencing an outer binding is a capture (escape). The bare
/// identifier is counted directly (NOT recursed into, which would double-count
/// it as a plain use); any other scrutinee shape recurses normally. All four
/// pattern-match forms are match-sugar, so they share this consume semantics.
fn walk_scrutinee<'a>(acc: &mut Acc<'a>, scrutinee: &'a Expr) {
    if let ExprKind::Identifier(n) = &scrutinee.kind {
        record_use(acc, n.as_str(), !acc.in_closure);
    } else {
        walk_expr(scrutinee, acc);
    }
}

fn walk_block<'a>(b: &'a Block, acc: &mut Acc<'a>) {
    for s in &b.stmts {
        walk_stmt(s, acc);
    }
    if let Some(fe) = &b.final_expr {
        walk_expr(fe, acc);
    }
}

fn walk_stmt<'a>(s: &'a Stmt, acc: &mut Acc<'a>) {
    match &s.kind {
        StmtKind::Let { pattern, value, .. } => {
            if let crate::ast::PatternKind::Binding(name) = &pattern.kind {
                acc.lets
                    .push((name.as_str(), (value.span.offset, value.span.length)));
            }
            walk_expr(value, acc);
        }
        StmtKind::LetElse {
            pattern,
            value,
            else_block,
            ..
        } => {
            if let crate::ast::PatternKind::Binding(name) = &pattern.kind {
                // Irrefutable-binding let-else (`let x = v else`, rare) —
                // introduces `x`, so record it and treat `v` as its RHS.
                acc.lets
                    .push((name.as_str(), (value.span.offset, value.span.length)));
                walk_expr(value, acc);
            } else {
                // Refutable `let Pat = <scrutinee> else { … }` — match-sugar
                // over `value`, so `value` is a consume-in-place scrutinee.
                walk_scrutinee(acc, value);
                // B-2026-09-06-48 — a `let`-else binding ESCAPES into the
                // enclosing scope, so it always takes the payload. Same rule
                // `retract_boxed_tuple_inner_drop_for_block` states by passing
                // `None` for its block.
                if let ExprKind::Identifier(n) = &value.kind {
                    if let Some(v) = variant_arm_takes_payload_block(pattern, None) {
                        acc.payload_consumers
                            .entry(n.as_str())
                            .or_default()
                            .insert(v);
                    }
                    if let Some(v) = variant_arm_payload_escapes_block(pattern, None) {
                        acc.payload_escapers
                            .entry(n.as_str())
                            .or_default()
                            .insert(v);
                    }
                    let cr: &dyn Fn(&Expr) -> bool = acc.copy_read.unwrap_or(&projection_is_read);
                    if let Some(v) = variant_arm_payload_escapes_proj_block(pattern, None, cr) {
                        acc.payload_escapers_proj
                            .entry(n.as_str())
                            .or_default()
                            .insert(v);
                        // B-2026-09-14-18 — see the `Match` site.
                        if let Some((pv, parts)) =
                            variant_arm_payload_escaping_parts_block(pattern, None, cr)
                        {
                            acc.payload_escaper_parts
                                .entry(n.as_str())
                                .or_default()
                                .entry(pv)
                                .or_default()
                                .extend(parts);
                        }
                    }
                }
            }
            walk_block(else_block, acc);
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => walk_block(body, acc),
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            walk_expr(target, acc);
            walk_expr(value, acc);
        }
        StmtKind::MultiAssign { targets, values } => {
            for t in targets {
                walk_expr(t, acc);
            }
            for v in values {
                walk_expr(v, acc);
            }
        }
        StmtKind::Expr(e) => walk_expr(e, acc),
    }
}

fn walk_call_arg<'a>(a: &'a CallArg, acc: &mut Acc<'a>) {
    walk_expr(&a.value, acc);
}

fn walk_match_arm<'a>(a: &'a MatchArm, acc: &mut Acc<'a>) {
    // Patterns bind NEW names; they are not uses of an outer binding. Guard and
    // body ARE ordinary use positions (any binding referenced there escapes).
    if let Some(g) = &a.guard {
        walk_expr(g, acc);
    }
    walk_expr(&a.body, acc);
}

fn walk_expr<'a>(e: &'a Expr, acc: &mut Acc<'a>) {
    match &e.kind {
        // A bare identifier reached HERE is a use in a non-`match`-scrutinee
        // position (the scrutinee case is intercepted in the `Match` arm below
        // and never recurses here), so it is an escape.
        ExprKind::Identifier(n) => record_use(acc, n.as_str(), false),
        ExprKind::Match { scrutinee, arms } => {
            walk_scrutinee(acc, scrutinee);
            if let ExprKind::Identifier(n) = &scrutinee.kind {
                let cr: &dyn Fn(&Expr) -> bool = acc.copy_read.unwrap_or(&projection_is_read);
                for a in arms {
                    if let Some(v) =
                        variant_arm_takes_payload(&a.pattern, a.guard.as_ref(), &a.body)
                    {
                        acc.payload_consumers
                            .entry(n.as_str())
                            .or_default()
                            .insert(v);
                    }
                    if let Some(v) =
                        variant_arm_payload_escapes(&a.pattern, a.guard.as_ref(), &a.body)
                    {
                        acc.payload_escapers
                            .entry(n.as_str())
                            .or_default()
                            .insert(v);
                    }
                    if let Some(v) =
                        variant_arm_payload_escapes_proj(&a.pattern, a.guard.as_ref(), &a.body, cr)
                    {
                        acc.payload_escapers_proj
                            .entry(n.as_str())
                            .or_default()
                            .insert(v);
                        // B-2026-09-14-18 — recorded only alongside the map it
                        // narrows, so a part set can never contradict it.
                        if let Some((pv, parts)) = variant_arm_payload_escaping_parts(
                            &a.pattern,
                            a.guard.as_ref(),
                            &a.body,
                            cr,
                        ) {
                            acc.payload_escaper_parts
                                .entry(n.as_str())
                                .or_default()
                                .entry(pv)
                                .or_default()
                                .extend(parts);
                        }
                    }
                }
            }
            for arm in arms {
                walk_match_arm(arm, acc);
            }
        }
        // Leaves with no sub-expressions.
        ExprKind::Integer(_, _)
        | ExprKind::Float(_, _)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::ByteStringLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(_)
        | ExprKind::Path { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::PipePlaceholder
        | ExprKind::Continue { .. }
        | ExprKind::Error => {}
        ExprKind::InterpolatedStringLit(parts) => {
            for p in parts {
                if let ParsedInterpolationPart::Expr(inner, _) = p {
                    // A BARE identifier hole is a READ: the Display path loads
                    // from the binding's slot and formats it, and there is no
                    // spelling of `f"{x}"` that moves `x` anywhere. Intercepted
                    // here rather than recursed, exactly as the `Match` arm
                    // intercepts its scrutinee, so the recursion below never
                    // sees it and never counts it as an escape.
                    //
                    // Suppressed inside a closure for the same reason the
                    // scrutinee case is: referencing an outer binding there is
                    // a CAPTURE into an env that can outlive the binding.
                    match &inner.kind {
                        ExprKind::Identifier(n) if !acc.in_closure => {
                            record_read_only_use(acc, n.as_str())
                        }
                        _ => walk_expr(inner, acc),
                    }
                }
            }
        }
        ExprKind::Binary { left, right, .. } => {
            walk_expr(left, acc);
            walk_expr(right, acc);
        }
        ExprKind::Unary { operand, .. } => walk_expr(operand, acc),
        ExprKind::Question(inner) => walk_expr(inner, acc),
        ExprKind::OptionalChain { object, args, .. } => {
            walk_expr(object, acc);
            if let Some(a) = args {
                for arg in a {
                    walk_call_arg(arg, acc);
                }
            }
        }
        ExprKind::NilCoalesce { left, right } => {
            walk_expr(left, acc);
            walk_expr(right, acc);
        }
        ExprKind::Call { callee, args } => {
            walk_expr(callee, acc);
            for a in args {
                walk_call_arg(a, acc);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            walk_expr(object, acc);
            for a in args {
                walk_call_arg(a, acc);
            }
        }
        ExprKind::FieldAccess { object, .. } => walk_expr(object, acc),
        ExprKind::TupleIndex { object, .. } => walk_expr(object, acc),
        ExprKind::Index { object, index } => {
            walk_expr(object, acc);
            walk_expr(index, acc);
        }
        ExprKind::Block(b) | ExprKind::Comptime(b) => walk_block(b, acc),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            walk_expr(condition, acc);
            walk_block(then_block, acc);
            if let Some(e) = else_branch {
                walk_expr(e, acc);
            }
        }
        ExprKind::IfLet {
            pattern,
            value,
            then_block,
            else_branch,
        } => {
            // `if let Pat = <scrutinee>` is match-sugar — consume-in-place.
            walk_scrutinee(acc, value);
            if let ExprKind::Identifier(n) = &value.kind {
                if let Some(v) = variant_arm_takes_payload_block(pattern, Some(then_block)) {
                    acc.payload_consumers
                        .entry(n.as_str())
                        .or_default()
                        .insert(v);
                }
                if let Some(v) = variant_arm_payload_escapes_block(pattern, Some(then_block)) {
                    acc.payload_escapers
                        .entry(n.as_str())
                        .or_default()
                        .insert(v);
                }
                let cr: &dyn Fn(&Expr) -> bool = acc.copy_read.unwrap_or(&projection_is_read);
                if let Some(v) =
                    variant_arm_payload_escapes_proj_block(pattern, Some(then_block), cr)
                {
                    acc.payload_escapers_proj
                        .entry(n.as_str())
                        .or_default()
                        .insert(v);
                    // B-2026-09-14-18 — see the `Match` site.
                    if let Some((pv, parts)) =
                        variant_arm_payload_escaping_parts_block(pattern, Some(then_block), cr)
                    {
                        acc.payload_escaper_parts
                            .entry(n.as_str())
                            .or_default()
                            .entry(pv)
                            .or_default()
                            .extend(parts);
                    }
                }
            }
            walk_block(then_block, acc);
            if let Some(e) = else_branch {
                walk_expr(e, acc);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            walk_expr(condition, acc);
            walk_block(body, acc);
        }
        ExprKind::WhileLet {
            pattern,
            value,
            body,
            ..
        } => {
            // `while let Pat = <scrutinee>` is match-sugar — consume-in-place.
            walk_scrutinee(acc, value);
            if let ExprKind::Identifier(n) = &value.kind {
                if let Some(v) = variant_arm_takes_payload_block(pattern, Some(body)) {
                    acc.payload_consumers
                        .entry(n.as_str())
                        .or_default()
                        .insert(v);
                }
                if let Some(v) = variant_arm_payload_escapes_block(pattern, Some(body)) {
                    acc.payload_escapers
                        .entry(n.as_str())
                        .or_default()
                        .insert(v);
                }
                let cr: &dyn Fn(&Expr) -> bool = acc.copy_read.unwrap_or(&projection_is_read);
                if let Some(v) = variant_arm_payload_escapes_proj_block(pattern, Some(body), cr) {
                    acc.payload_escapers_proj
                        .entry(n.as_str())
                        .or_default()
                        .insert(v);
                    // B-2026-09-14-18 — see the `Match` site.
                    if let Some((pv, parts)) =
                        variant_arm_payload_escaping_parts_block(pattern, Some(body), cr)
                    {
                        acc.payload_escaper_parts
                            .entry(n.as_str())
                            .or_default()
                            .entry(pv)
                            .or_default()
                            .extend(parts);
                    }
                }
            }
            walk_block(body, acc);
        }
        ExprKind::For { iterable, body, .. } => {
            walk_expr(iterable, acc);
            walk_block(body, acc);
        }
        ExprKind::Loop { body, .. } => walk_block(body, acc),
        ExprKind::LabeledBlock { body, .. } => walk_block(body, acc),
        ExprKind::Closure { body, .. } => {
            let prev = acc.in_closure;
            acc.in_closure = true;
            walk_expr(body, acc);
            acc.in_closure = prev;
        }
        ExprKind::Return(opt) => {
            if let Some(inner) = opt {
                walk_expr(inner, acc);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(v) = value {
                walk_expr(v, acc);
            }
        }
        ExprKind::Tuple(exprs) | ExprKind::ArrayLiteral(exprs) => {
            for x in exprs {
                walk_expr(x, acc);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for x in items {
                walk_expr(x, acc);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            walk_expr(value, acc);
            walk_expr(count, acc);
        }
        ExprKind::MapLiteral { entries: pairs, .. } => {
            for (k, v) in pairs {
                walk_expr(k, acc);
                walk_expr(v, acc);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                walk_expr(&f.value, acc);
            }
            if let Some(sp) = spread {
                walk_expr(sp, acc);
            }
        }
        ExprKind::Pipe { left, right } => {
            walk_expr(left, acc);
            walk_expr(right, acc);
        }
        ExprKind::Cast { expr, .. } => walk_expr(expr, acc),
        ExprKind::OffsetOf { .. } => {}
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                walk_expr(s, acc);
            }
            if let Some(e) = end {
                walk_expr(e, acc);
            }
        }
        ExprKind::Unsafe(b) | ExprKind::Try(b) | ExprKind::Seq(b) | ExprKind::Par(b) => {
            walk_block(b, acc)
        }
        ExprKind::Lock { body, .. } => walk_block(body, acc),
        ExprKind::Providers { bindings, body } => {
            for pb in bindings {
                walk_expr(&pb.value, acc);
            }
            walk_block(body, acc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Item;
    use std::collections::HashSet;

    /// Non-escaping binding NAMES in the first function of `src` — maps the
    /// span-keyed production result back to names for readable assertions.
    fn nonescaping_names(src: &str) -> HashSet<String> {
        let parsed = crate::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let func = parsed
            .program
            .items
            .iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("no function");
        let spans = nonescaping_let_value_spans(func);
        // Second walk: collect (name, value-span) for every Binding let, keep
        // names whose span is in the non-escaping set.
        let mut acc = Acc::default();
        walk_block(&func.body, &mut acc);
        acc.lets
            .iter()
            .filter(|(_, sp)| spans.contains(sp))
            .map(|(n, _)| n.to_string())
            .collect()
    }

    #[test]
    fn consume_in_place_and_discard_are_nonescaping() {
        // matched-in-place and unused bindings are safe to release.
        let names = nonescaping_names(
            "fn f() -> i64 { let d = g(); let u = g(); match d { A(n) => n, B => 0 } }",
        );
        assert!(
            names.contains("d"),
            "matched-in-place `d` should be non-escaping"
        );
        assert!(names.contains("u"), "unused `u` should be non-escaping");
    }

    #[test]
    fn if_let_scrutinee_is_nonescaping() {
        // `if let Pat = d` is match-sugar → consume-in-place.
        let names =
            nonescaping_names("fn f() -> i64 { let d = g(); if let A(n) = d { n } else { 0 } }");
        assert!(
            names.contains("d"),
            "if-let scrutinee `d` should be non-escaping"
        );
    }

    #[test]
    fn if_let_capture_into_closure_escapes() {
        // if-let inside a closure body is still a capture (escape).
        let names = nonescaping_names(
            "fn f() -> i64 { let d = g(); let c = || { if let A(n) = d { n } else { 0 } }; c() }",
        );
        assert!(
            !names.contains("d"),
            "if-let inside a closure captures `d` → escape"
        );
    }

    #[test]
    fn multiple_matches_of_same_binding_are_nonescaping() {
        let names =
            nonescaping_names("fn f() { let d = g(); match d { _ => {} } match d { _ => {} } }");
        assert!(names.contains("d"));
    }

    #[test]
    fn returned_binding_escapes() {
        let names = nonescaping_names("fn f() -> R { let d = g(); d }");
        assert!(!names.contains("d"), "returned `d` must escape");
    }

    #[test]
    fn call_arg_use_escapes() {
        // `d` used as a call argument (and as an alias RHS) is a non-scrutinee
        // use → escapes. (`e`, matched only, satisfies THIS module's contract —
        // "used only as a match scrutinee"; the alias-RHS hazard for `e` is
        // handled separately by codegen's `owned_rhs` gate, which refuses an
        // identifier-alias RHS. This module answers escape, not alias-ownership.)
        let names = nonescaping_names(
            "fn f() -> i64 { let d = g(); let e = d; eat(d); match e { _ => 0 } }",
        );
        assert!(
            !names.contains("d"),
            "`d` passed to a call / aliased must escape"
        );
    }

    #[test]
    fn struct_tuple_field_positions_escape() {
        let s = nonescaping_names(
            "fn f() -> i64 { let d = g(); let b = S { r: d }; match b.r { _ => 0 } }",
        );
        assert!(!s.contains("d"), "struct-field init must escape");
        let t = nonescaping_names(
            "fn f() -> i64 { let d = g(); let p = (d, 1); match p.0 { _ => 0 } }",
        );
        assert!(!t.contains("d"), "tuple element must escape");
    }

    #[test]
    fn reassigned_binding_escapes() {
        // `d = g()` reads `d` as an assign target → a non-scrutinee use.
        let names =
            nonescaping_names("fn f() -> i64 { let mut d = g(); d = g(); match d { _ => 0 } }");
        assert!(
            !names.contains("d"),
            "reassigned `d` must escape (conservative)"
        );
    }

    /// Non-escaping PARAM names of the first function in `src`.
    fn nonescaping_params(src: &str) -> HashSet<String> {
        let parsed = crate::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let func = parsed
            .program
            .items
            .iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("no function");
        nonescaping_param_names(func)
    }

    #[test]
    fn param_consumed_in_place_is_nonescaping() {
        // A param used only as a `match` scrutinee owns the caller's transferred
        // +1 and can be released.
        let names = nonescaping_params("fn eat(r: R) -> i64 { match r { A(n) => n, B => 0 } }");
        assert!(
            names.contains("r"),
            "in-place-consumed param `r` should be non-escaping"
        );
    }

    #[test]
    fn forwarded_param_escapes() {
        // A param passed on to another consuming call escapes → the intermediate
        // must not release it (the terminal consumer does).
        let names = nonescaping_params("fn eat(r: R) -> i64 { eat2(r) }");
        assert!(!names.contains("r"), "forwarded param `r` must escape");
    }

    #[test]
    fn returned_param_escapes() {
        let names = nonescaping_params("fn id(r: R) -> R { r }");
        assert!(!names.contains("r"), "returned param `r` must escape");
    }

    /// Same helper, the strict predicate.
    fn unused_params(src: &str) -> HashSet<String> {
        let parsed = crate::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let func = parsed
            .program
            .items
            .iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("no function");
        unused_param_names(func)
    }

    /// THE POINT OF HAVING TWO PREDICATES, pinned as a pair so neither drifts
    /// into the other. `b` is used only as a `match` scrutinee, which
    /// `nonescaping_param_names` admits — correct for the in-place RC release it
    /// serves, and wrong for a caller that wants to FREE `b`'s buffer, because
    /// the arm binds the scrutinee and returns it. Measured under ASAN
    /// (B-2026-08-15-9): swapping the strict predicate for the loose one at the
    /// monomorph call site turns this exact shape into a double-free abort.
    #[test]
    fn match_scrutinee_param_is_nonescaping_but_not_unused() {
        let src = "fn takeout(b: T) -> T { match b { v => { return v; } } }";
        assert!(
            nonescaping_params(src).contains("b"),
            "a match-scrutinee-only param is non-escaping by the loose rule"
        );
        assert!(
            !unused_params(src).contains("b"),
            "...but it IS used, so the strict rule must reject it — the arm hands \
             the caller's own buffer back out as the result"
        );
    }

    /// The shape B-2026-08-15-9 fixes: a param that shares its type parameter
    /// with a RETURNED sibling but is itself never named. The return-type test
    /// in `mono.rs` cannot tell the two apart — both are `T` — so it declined
    /// both; this is what tells them apart.
    #[test]
    fn sibling_of_a_returned_param_is_unused() {
        let names = unused_params("fn pick(a: T, b: T) -> T { return id(a); }");
        assert!(names.contains("b"), "never-named `b` must be unused");
        assert!(
            !names.contains("a"),
            "`a` reaches the return through a forwarding call and must not be unused"
        );
    }

    #[test]
    fn capture_into_closure_escapes() {
        // `match d` INSIDE a closure body is a capture — must NOT be treated as a
        // safe scrutinee use (would use-after-free an escaping closure's env).
        let names = nonescaping_names(
            "fn f() -> i64 { let d = g(); let c = || { match d { A(n) => n, B => 0 } }; c() }",
        );
        assert!(
            !names.contains("d"),
            "binding captured by a closure must escape even if only matched inside"
        );
    }
}
