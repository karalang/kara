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
/// B-2026-09-26-46 — see `Acc::lent`: (callee name, argument index) -> lent read.
pub type LentPolicy<'a> = dyn Fn(&str, usize) -> bool + 'a;
/// B-2026-09-28-24 — see `Acc::lent_call`: (call expression, argument index) -> lent read.
pub type LentCallPolicy<'a> = dyn Fn(&Expr, usize) -> bool + 'a;

#[derive(Default)]
struct Acc<'a> {
    /// B-2026-09-27-51 — the unmutated `let mut` rebinds of parameters, read
    /// as immutable aliases (`crate::ast::demoted_param_rebind_names`).
    demoted: HashSet<&'a str>,
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
    /// B-2026-09-28-48 — follow a WHOLE immutable rebind of an arm's payload
    /// binding (`Some(y) => { let z = y; .. }`) and ask the rest of the arm
    /// about `z`. Set only by [`optres_payload_escaping_param_variants_with_rebinds`]
    /// for a payload whose rebind the callee lowers as a view (a named
    /// struct); a `Vec` payload's rebind is its only owner, so for it the
    /// rebind stays an escape.
    follow_rebinds: bool,
    /// B-2026-09-17-23 — the MEMORY sibling of `copy_read`, consulted only by
    /// the `payload_consumers` arms: a projection this answers `true` for is a
    /// copy read that takes nothing out of the payload. `None` keeps the plain
    /// syntactic walk. See [`optres_payload_consuming_param_variants_with`].
    take_copy_read: Option<&'a dyn Fn(&Expr) -> bool>,
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
    /// B-2026-09-26-46 — the LENT-argument policy of
    /// [`by_value_read_or_lent_param_names`]: asked of a bare identifier
    /// handed, unlabelled and unmarked, to a call whose callee is a bare name,
    /// with that name and the argument's index. `true` counts the use as a
    /// read. `None` everywhere else, so every other set is unchanged.
    lent: Option<&'a LentPolicy<'a>>,
    /// B-2026-09-28-24 — the lent policy for a call [`Self::lent`] cannot
    /// name by a bare identifier: a METHOD call (`q.eat(h)`) or an
    /// associated / path call (`K.eat(h)`). Asked with the whole call
    /// expression and the argument's index (the receiver excluded), so the
    /// caller can resolve the callee with its own type information.
    lent_call: Option<&'a LentCallPolicy<'a>>,
    /// B-2026-09-24-20 — by-value `Option`/`Result` PARAMS whose immutable
    /// whole rebinds (`let c = a;`) are read as ALIASES of the param: every
    /// later use of `c` is recorded against `a`, and the rebind itself records
    /// nothing. Empty (the default) leaves the walk exactly as it was, so only
    /// the entry points that seed it see the aliasing. See [`seeded_acc`].
    alias_roots: HashSet<&'a str>,
    /// Rebound name -> the param it aliases. Filled while walking; never
    /// cleared, so a later shadowing `let c = ..` keeps counting against the
    /// param, which only ever makes the param look MORE escaping.
    aliases: HashMap<&'a str, &'a str>,
    /// B-2026-09-28-28 — the statements after the `let .. else` being walked
    /// (and the block's tail), set by [`walk_block`] for a seeded walk. The
    /// binding lives on in exactly that rest, so its payload questions are
    /// asked of it, as an `if let`'s are asked of its body. `None` keeps the
    /// old answer, that such a binding always takes the payload.
    let_else_rest: Option<Block>,
}

/// B-2026-09-24-20 — the param a use of `name` counts against.
fn root<'a>(acc: &Acc<'a>, name: &'a str) -> &'a str {
    acc.aliases.get(name).copied().unwrap_or(name)
}

thread_local! {
    /// B-2026-09-28-13 — the program's identity hand-backs
    /// (`crate::ast::optres_identity_arg`), by function name. Owned data
    /// rather than a borrow of the program, so it cannot dangle; replaced
    /// wholesale by [`set_program`] at the start of every compile and every
    /// interpreter run, so a map left by an earlier program on this thread is
    /// never read against a later one.
    static OPTRES_IDENTITY_FNS: std::cell::RefCell<HashMap<String, usize>> =
        std::cell::RefCell::new(HashMap::new());
}

/// B-2026-09-28-13 — record `program`'s identity hand-backs for the seeded
/// walks. Both backends call this for the program they are about to run, so
/// the escape questions they ask of one function answer alike.
pub fn set_program(program: &crate::ast::Program) {
    let map: HashMap<String, usize> = program
        .items
        .iter()
        .filter_map(|it| match it {
            crate::ast::Item::Function(f) => {
                crate::ast::optres_identity_arg(program, f).map(|k| (f.name.clone(), k))
            }
            _ => None,
        })
        .collect();
    OPTRES_IDENTITY_FNS.with(|m| *m.borrow_mut() = map);
}

/// B-2026-09-28-13 — is `e` an identity hand-back of `param` (`id(a)` for
/// `param == "a"`), under the program [`set_program`] installed? The AST's
/// param-payload channels ask this beside their bare-identifier test, so
/// every one of them reads `id(a)` as `a`.
pub fn is_identity_handback_of(e: &Expr, param: &str) -> bool {
    identity_handback_arg(e)
        .is_some_and(|a| matches!(&a.kind, ExprKind::Identifier(n) if n == param))
}

/// B-2026-09-28-13 — the argument an identity hand-back returns (`a` in
/// `id(a)`), when `e` is one under the program [`set_program`] installed and
/// the argument is a plain unlabelled, unmarked identifier. The result IS that
/// argument's envelope, so a caller can read the call as the identifier.
pub fn identity_handback_arg(e: &Expr) -> Option<&Expr> {
    let ExprKind::Call { callee, args } = &e.kind else {
        return None;
    };
    let ExprKind::Identifier(cn) = &callee.kind else {
        return None;
    };
    if args.len() != 1 {
        return None;
    }
    let k = OPTRES_IDENTITY_FNS.with(|m| m.borrow().get(cn.as_str()).copied())?;
    let a = args.get(k)?;
    (a.label.is_none() && !a.mut_marker && matches!(&a.value.kind, ExprKind::Identifier(_)))
        .then_some(&a.value)
}

/// B-2026-09-28-13 — the by-value `Option`/`Result` PARAM that `e` IS: a bare
/// identifier, or an identity hand-back of one (`id(a)` over `fn id(a:
/// Option[S]) -> Option[S] { a }`), whose result is the argument's own
/// envelope. The hand-back is read through only for a seeded param (an
/// `alias_roots` member) and outside a closure, the terms `let c = a;` is
/// read through on.
fn param_ident<'a>(acc: &Acc<'a>, e: &'a Expr) -> Option<&'a str> {
    match &e.kind {
        ExprKind::Identifier(n) => Some(n.as_str()),
        ExprKind::Call { callee, args } if !acc.in_closure && !acc.alias_roots.is_empty() => {
            let ExprKind::Identifier(cn) = &callee.kind else {
                return None;
            };
            let k = OPTRES_IDENTITY_FNS.with(|m| m.borrow().get(cn.as_str()).copied())?;
            if args.len() != 1 {
                return None;
            }
            let a = args.get(k)?;
            if a.label.is_some() || a.mut_marker {
                return None;
            }
            let ExprKind::Identifier(n) = &a.value.kind else {
                return None;
            };
            acc.alias_roots
                .contains(root(acc, n.as_str()))
                .then_some(n.as_str())
        }
        _ => None,
    }
}

/// B-2026-09-28-13 — a seeded param in a position that drops or probes it and
/// nothing else, for the LENDING walk that answers the bodies question: a
/// discard statement (`a;`, `id(a);`), `let _ = a;`, and a variant probe
/// (`a.is_some()`). None of them hands the payload anywhere, so the caller
/// still owes its bodies; counting them as escapes stood a fresh temp's
/// bodies down with nobody to run them (B-2026-09-28-34).
fn dead_end_param<'a>(acc: &Acc<'a>, e: &'a Expr) -> Option<&'a str> {
    if acc.lent.is_none() || acc.in_closure {
        return None;
    }
    let n = param_ident(acc, e)?;
    acc.alias_roots.contains(root(acc, n)).then_some(n)
}

/// B-2026-09-24-20 — an accumulator that reads `let c = a;` over a by-value
/// `Option`/`Result` param as an alias of `a`.
///
/// Why only these params, and only for the entry points that answer the
/// by-value `Option`/`Result` protocol: the caller of such a function retains
/// the payload's `Drop` bodies unless the callee lets the payload outlive the
/// call (B-2026-09-04-29), and codegen gives the callee's rebind a param VIEW
/// for exactly that reason. Counted as an escape, the rebind told the caller
/// the opposite -- so a named argument's body ran in both frames (`d1 k5 d1`)
/// and a temporary's only walker was the callee's. The struct/`shared` protocols
/// that read the other entry points treat a rebind as a real transfer
/// (`callee_rebinds_param_whole`), so they keep the unseeded walk.
fn seeded_acc<'a>(func: &'a Function) -> Acc<'a> {
    let mut acc = Acc {
        demoted: crate::ast::demoted_param_rebind_names(func)
            .into_iter()
            .collect(),
        ..Acc::default()
    };
    for p in &func.params {
        let crate::ast::PatternKind::Binding(name) = &p.pattern.kind else {
            continue;
        };
        let crate::ast::TypeKind::Path(path) = &p.ty.kind else {
            continue;
        };
        if matches!(
            path.segments.first().map(String::as_str),
            Some("Option") | Some("Result")
        ) {
            acc.alias_roots.insert(name.as_str());
        }
    }
    acc
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

/// B-2026-09-19-22 — the raw `(total, scrutinee, read-only)` use tallies of
/// `name` over `func`'s body, the unseeded walk [`nonescaping_param_names`]
/// compares. For a caller that needs to account for the non-read uses itself
/// (codegen's generic hand-back check counts the returns).
pub fn param_use_counts(func: &Function, name: &str) -> (u32, u32, u32) {
    let mut acc = Acc::default();
    walk_block(&func.body, &mut acc);
    acc.counts.get(name).copied().unwrap_or((0, 0, 0))
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
    let mut acc = seeded_acc(func);
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

/// B-2026-09-28-10 — [`by_value_nonescaping_param_names`] with a CALLER-SUPPLIED
/// lent policy: a bare identifier handed, unlabelled and unmarked, to a free
/// call counts as a read when `lent(callee, index)` says so. The policy needs
/// type information this module does not have (whether the consumer keeps any
/// part of an `Option`/`Result` payload), so codegen supplies it.
pub fn by_value_nonescaping_param_names_lending<'a>(
    func: &'a Function,
    lent: &'a LentPolicy<'a>,
    lent_call: &'a LentCallPolicy<'a>,
) -> HashSet<String> {
    let mut acc = seeded_acc(func);
    acc.lent = Some(lent);
    acc.lent_call = Some(lent_call);
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

/// B-2026-09-26-46 — [`by_value_nonescaping_param_names`] with one more
/// read-only position: a parameter handed on, bare and by value, to a GENERIC
/// free function whose matching parameter is itself in this set (or is a
/// `ref`). A monomorph's by-value parameter is caller-retained -- the callee
/// deep-copies it only at a site that keeps it -- so a parameter that is only
/// ever read, or lent on to a callee that only reads it, is never kept by
/// anything past the call, and its CALLER still owes every body it carries.
///
/// `fn gw[T](x: T) -> i64 { gn(x) }` over `fn gn[T](x: T) -> i64 { 1 }` is the
/// shape: the stricter set counts `gn(x)` as an escape, so a projection off a
/// fresh temp handed to `gw` (`gw(mkw(7).r)`) was treated as possibly kept and
/// its temp ran no body at all. Only a generic callee is followed: a
/// non-generic one owns a by-value parameter and runs its body itself. The
/// recursion is depth-bounded; running out answers "escapes", the
/// conservative direction.
pub fn by_value_read_or_lent_param_names(
    program: &crate::ast::Program,
    func: &Function,
) -> HashSet<String> {
    read_or_lent_names(program, func, 4)
}

fn read_or_lent_names(
    program: &crate::ast::Program,
    func: &Function,
    depth: u32,
) -> HashSet<String> {
    let lent = |cn: &str, k: usize| -> bool {
        if depth == 0 {
            return false;
        }
        let Some(g) = program.items.iter().find_map(|it| match it {
            crate::ast::Item::Function(f) if f.name == cn => Some(f),
            _ => None,
        }) else {
            return false;
        };
        if g.generic_params.is_none() || g.self_param.is_some() {
            return false;
        }
        let Some(p) = g.params.get(k) else {
            return false;
        };
        match &p.ty.kind {
            crate::ast::TypeKind::Ref(_) => true,
            crate::ast::TypeKind::MutRef(_) => false,
            _ => matches!(&p.pattern.kind, crate::ast::PatternKind::Binding(n)
                if read_or_lent_names(program, g, depth - 1).contains(n)),
        }
    };
    let mut acc = seeded_acc(func);
    acc.lent = Some(&lent);
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

/// B-2026-09-20-38 — [`by_value_nonescaping_param_names`] with one more
/// position: a parameter handed on, bare and by value, to a GENERIC free
/// function whose matching by-value parameter is itself in this set -- that is,
/// to a monomorph whose prologue TAKES a boxed enum payload's box.
///
/// This is the set the two halves of a generic call's box hand-off have to
/// agree on: the caller (`compile_generic_call`) retracts its own box drop for
/// a param in it, and the monomorph's prologue registers the box drop for it.
/// A forwarding middle function was left out of both, so the chain
/// `main` (keeps) -> `gfwd[T](g) { glen(g) }` (declines, escaping) ->
/// `glen[T](g)` (takes) had two owners: `glen` freed the box and `main` freed
/// it again. With the forward counted, `main` stands down, `gfwd` owns the box
/// on entry, and its call to `glen` zeroes that ownership exactly as `main`'s
/// direct call does -- so a forward on only some paths still leaves `gfwd` the
/// owner on the others. Unlike [`by_value_read_or_lent_param_names`] a `ref`
/// callee is NOT followed: lending to it takes nothing, which is the other
/// question. Depth-bounded; running out answers "escapes" (caller-retained),
/// today's route.
pub fn by_value_boxed_param_taken_names(
    program: &crate::ast::Program,
    func: &Function,
) -> HashSet<String> {
    taken_names(program, func, 4)
}

fn taken_names(program: &crate::ast::Program, func: &Function, depth: u32) -> HashSet<String> {
    let lent = |cn: &str, k: usize| -> bool {
        if depth == 0 {
            return false;
        }
        let Some(g) = program.items.iter().find_map(|it| match it {
            crate::ast::Item::Function(f) if f.name == cn => Some(f),
            _ => None,
        }) else {
            return false;
        };
        if g.generic_params.is_none() || g.self_param.is_some() {
            return false;
        }
        let Some(p) = g.params.get(k) else {
            return false;
        };
        // A BARE type param (`fn gany[T](x: T)`) is not followed: its
        // monomorph's prologue registers no box for it (the payload type it
        // would ask is the whole `T`), so counting it a taker strands the box
        // with nobody -- measured as a lost `Drop` body and a 32 B leak.
        let bare_type_param = matches!(&p.ty.kind, crate::ast::TypeKind::Path(path)
        if path.generic_args.is_none()
            && path.segments.len() == 1
            && g.generic_params.as_ref().is_some_and(|gp| {
                gp.params.iter().any(|q| q.name == path.segments[0])
            }));
        !bare_type_param
            && !matches!(
                &p.ty.kind,
                crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
            )
            && matches!(&p.pattern.kind, crate::ast::PatternKind::Binding(n)
                if taken_names(program, g, depth - 1).contains(n))
    };
    let mut acc = seeded_acc(func);
    acc.lent = Some(&lent);
    walk_block(&func.body, &mut acc);
    // B-2026-09-28-8 — a param handed to a generic PASSTHROUGH (a callee that
    // returns that argument on every path) whose result is bound by a
    // top-level `let` that is itself only taken: `let h = gid(g); glen(h)`.
    // The box moves from `g` to `h` and on to `glen`, so `g` is taken exactly
    // as a direct `glen(g)` would take it; codegen's hand-back arm disarms `g`
    // into `h` at the `let`. The binding must be let-bound once, so a shadow
    // cannot lend it its counts.
    // Walked LAST statement first, so a chain (`let h = gid(g); let k =
    // gid(h); glen(k)`) credits `k` before `h` is asked, and `h` then passes
    // its own credit on to `g`.
    let mut passthrough_reads: HashMap<String, u32> = HashMap::new();
    let is_param = |n: &str| {
        func.params
            .iter()
            .any(|p| matches!(&p.pattern.kind, crate::ast::PatternKind::Binding(b) if b == n))
    };
    let let_once = |n: &str| acc.lets.iter().filter(|(l, _)| *l == n).count() == 1;
    for st in func.body.stmts.iter().rev() {
        let StmtKind::Let {
            pattern,
            value,
            is_mut: false,
            ..
        } = &st.kind
        else {
            continue;
        };
        let crate::ast::PatternKind::Binding(h) = &pattern.kind else {
            continue;
        };
        let ExprKind::Call { callee, args } = &value.kind else {
            continue;
        };
        let ExprKind::Identifier(cn) = &callee.kind else {
            continue;
        };
        if depth == 0 || is_param(h) || !let_once(h) {
            continue;
        }
        let (ht, hs, hr) = acc.counts.get(h.as_str()).copied().unwrap_or((0, 0, 0));
        if ht != hs + hr + passthrough_reads.get(h.as_str()).copied().unwrap_or(0) {
            continue;
        }
        let Some(g) = program.items.iter().find_map(|it| match it {
            crate::ast::Item::Function(f) if f.name == *cn => Some(f),
            _ => None,
        }) else {
            continue;
        };
        if g.generic_params.is_none() || g.self_param.is_some() {
            continue;
        }
        for (k, a) in args.iter().enumerate() {
            let ExprKind::Identifier(pn) = &a.value.kind else {
                continue;
            };
            let by_value = g.params.get(k).is_some_and(|p| {
                !matches!(
                    &p.ty.kind,
                    crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
                ) && matches!(p.pattern.kind, crate::ast::PatternKind::Binding(_))
            });
            if a.label.is_none()
                && !a.mut_marker
                && (is_param(pn) || let_once(pn))
                && by_value
                && crate::ast::fn_always_returns_param(Some(program), g, k)
            {
                *passthrough_reads.entry(pn.clone()).or_insert(0) += 1;
            }
        }
    }
    func.params
        .iter()
        .filter_map(|p| {
            let crate::ast::PatternKind::Binding(name) = &p.pattern.kind else {
                return None;
            };
            let (total, scrut, ro) = acc.counts.get(name.as_str()).copied().unwrap_or((0, 0, 0));
            let extra = passthrough_reads.get(name.as_str()).copied().unwrap_or(0);
            (total == scrut + ro + extra).then(|| name.clone())
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
    let mut acc = seeded_acc(func);
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
    optres_payload_escaping_param_variants_with_rebinds(func, copy_read, false)
}

/// B-2026-09-28-48 — [`optres_payload_escaping_param_variants_with`], and
/// with `follow_rebinds` a whole immutable rebind of an arm's payload binding
/// is followed rather than read as an escape (see `Acc::follow_rebinds`).
pub fn optres_payload_escaping_param_variants_with_rebinds(
    func: &Function,
    copy_read: &dyn Fn(&Expr) -> bool,
    follow_rebinds: bool,
) -> HashMap<String, HashSet<String>> {
    let mut acc = Acc {
        copy_read: Some(copy_read),
        follow_rebinds,
        ..seeded_acc(func)
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
        ..seeded_acc(func)
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
    optres_payload_escaping_param_variants_ignoring_projections_with_rebinds(func, false)
}

/// B-2026-09-28-48 follow-up — [`optres_payload_escaping_param_variants_ignoring_projections`],
/// and with `follow_rebinds` a whole immutable rebind of an arm's payload
/// binding is followed rather than read as an escape (see `Acc::follow_rebinds`).
pub fn optres_payload_escaping_param_variants_ignoring_projections_with_rebinds(
    func: &Function,
    follow_rebinds: bool,
) -> HashMap<String, HashSet<String>> {
    let mut acc = Acc {
        follow_rebinds,
        ..seeded_acc(func)
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
    let mut acc = seeded_acc(func);
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

/// B-2026-09-17-23 — [`optres_payload_consuming_param_variants`] with a
/// COPY-READ policy for the arm bindings, supplied by a caller that holds the
/// instantiated payload type.
///
/// The plain walk calls every projection off a whole-payload binding a take,
/// so `Some(t) => { return t.1 }` over `Option[(W, i64)]` read as consuming
/// the payload, `compile_generic_call` left the boxed interior to a callee arm
/// that never frees it, and `W`'s `String` leaked (2 B per call) where the
/// concrete twin is clean. The concrete callee asks
/// `binding_only_borrowed_with` with a scalar copy-read
/// (`arm_binding_scalar_copy_read`, B-2026-09-10-23) before it retracts the
/// interior; this is that same question, asked from the caller's side.
pub fn optres_payload_consuming_param_variants_with(
    func: &Function,
    copy_read: &dyn Fn(&Expr) -> bool,
) -> HashMap<String, HashSet<String>> {
    let mut acc = Acc {
        take_copy_read: Some(copy_read),
        ..seeded_acc(func)
    };
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
    copy_read: Option<&dyn Fn(&Expr) -> bool>,
) -> Option<&'a str> {
    let (variant, binds) = variant_payload_binds(pattern)?;
    let borrowed = |v: &str, e: &Expr| match copy_read {
        Some(cr) => crate::consume_class::binding_only_borrowed_with(v, e, cr),
        None => crate::consume_class::binding_only_borrowed(v, e),
    };
    match binds {
        // A per-element destructure (`Some((a, b))`) gives every leaf its own
        // owner unconditionally.
        None => Some(variant),
        Some(names) => (!names
            .iter()
            .all(|v| borrowed(v, body) && guard.is_none_or(|g| borrowed(v, g))))
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
    follow_rebinds: bool,
) -> Option<&'a str> {
    let variant = optres_variant_of_pattern(pattern)?;
    let names = pattern.binding_names();
    if names.is_empty() {
        return None;
    }
    (!names.iter().all(|v| {
        crate::consume_class::binding_only_borrowed_escape_with(v, body, copy_read, follow_rebinds)
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
    follow_rebinds: bool,
) -> Option<(&'a str, BTreeSet<usize>)> {
    let variant = optres_variant_of_pattern(pattern)?;
    let parts = tuple_payload_binding_parts(pattern)?;
    let escaping: BTreeSet<usize> = parts
        .iter()
        .enumerate()
        .filter_map(|(i, name)| {
            let v = (*name)?;
            let borrowed = crate::consume_class::binding_only_borrowed_escape_with(
                v,
                body,
                copy_read,
                follow_rebinds,
            ) && guard.is_none_or(|g| {
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
    follow_rebinds: bool,
) -> Option<(&'a str, BTreeSet<usize>)> {
    let variant = optres_variant_of_pattern(pattern)?;
    let parts = tuple_payload_binding_parts(pattern)?;
    // `None` for the block is `let`-else outside a seeded walk: the bindings
    // outlive the construct, so every NAMED part escapes. Wildcards still bind nothing.
    let escaping: BTreeSet<usize> = parts
        .iter()
        .enumerate()
        .filter_map(|(i, name)| {
            let v = (*name)?;
            match block {
                None => Some(i),
                Some(b) => (!crate::consume_class::binding_only_borrowed_block_escape_with(
                    v,
                    b,
                    copy_read,
                    follow_rebinds,
                ))
                .then_some(i),
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
    follow_rebinds: bool,
) -> Option<&'a str> {
    let variant = optres_variant_of_pattern(pattern)?;
    let names = pattern.binding_names();
    if names.is_empty() {
        return None;
    }
    match block {
        None => Some(variant),
        Some(b) => (!names.iter().all(|v| {
            crate::consume_class::binding_only_borrowed_block_escape_with(
                v,
                b,
                copy_read,
                follow_rebinds,
            )
        }))
        .then_some(variant),
    }
}

/// Block sibling of [`variant_arm_payload_escapes`]. `None` for the block means
/// the bindings escape the construct entirely (`let`-else outside a seeded
/// walk, which passes the statements after it instead), so they always do.
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
/// construct entirely (`let`-else outside a seeded walk), so they always take.
fn variant_arm_takes_payload_block<'a>(
    pattern: &'a crate::ast::Pattern,
    block: Option<&Block>,
    copy_read: Option<&dyn Fn(&Expr) -> bool>,
) -> Option<&'a str> {
    let (variant, binds) = variant_payload_binds(pattern)?;
    match (binds, block) {
        (None, _) => Some(variant),
        (Some(_), None) => Some(variant),
        (Some(names), Some(b)) => (!names.iter().all(|v| match copy_read {
            Some(cr) => crate::consume_class::binding_only_borrowed_block_with(v, b, cr),
            None => crate::consume_class::binding_only_borrowed_block(v, b),
        }))
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
    let name = root(acc, name);
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
    let name = root(acc, name);
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
    // B-2026-09-28-13 — or an identity hand-back of a seeded param.
    if let Some(n) = param_ident(acc, scrutinee) {
        record_use(acc, n, !acc.in_closure);
    } else {
        walk_expr(scrutinee, acc);
    }
}

fn walk_block<'a>(b: &'a Block, acc: &mut Acc<'a>) {
    for (i, s) in b.stmts.iter().enumerate() {
        // B-2026-09-28-28 — a seeded walk asks a refutable `let .. else`
        // binding's payload questions of the statements it lives on in.
        if !acc.alias_roots.is_empty()
            && matches!(&s.kind, StmtKind::LetElse { pattern, .. }
                if !matches!(pattern.kind, crate::ast::PatternKind::Binding(_)))
        {
            acc.let_else_rest = Some(Block {
                stmts: b.stmts[i + 1..].to_vec(),
                final_expr: b.final_expr.clone(),
                span: b.span,
            });
        }
        walk_stmt(s, acc);
        acc.let_else_rest = None;
    }
    if let Some(fe) = &b.final_expr {
        walk_expr(fe, acc);
    }
}

fn walk_stmt<'a>(s: &'a Stmt, acc: &mut Acc<'a>) {
    match &s.kind {
        StmtKind::Let {
            pattern,
            value,
            is_mut,
            ..
        } => {
            if let crate::ast::PatternKind::Binding(name) = &pattern.kind {
                // B-2026-09-24-20 — `let c = a;` over a seeded param is an
                // alias, not a use: see `Acc::alias_roots`. Immutable only, so
                // `c` cannot be reassigned to something `a` never held.
                // B-2026-09-28-13 — `let c = id(a);` is the same alias.
                if let Some(src) = param_ident(acc, value) {
                    let r = root(acc, src);
                    // B-2026-09-27-51 — or a `let mut` never mutated, which
                    // codegen compiles as the `let` it is.
                    if (!*is_mut || acc.demoted.contains(name.as_str()))
                        && !acc.in_closure
                        && acc.alias_roots.contains(r)
                    {
                        acc.aliases.insert(name.as_str(), r);
                        return;
                    }
                }
                acc.lets
                    .push((name.as_str(), (value.span.offset, value.span.length)));
            } else if matches!(pattern.kind, crate::ast::PatternKind::Wildcard) {
                if let Some(n) = dead_end_param(acc, value) {
                    record_read_only_use(acc, n);
                    return;
                }
            }
            walk_expr(value, acc);
        }
        StmtKind::LetElse {
            pattern,
            value,
            else_block,
            ..
        } => {
            let rest = acc.let_else_rest.take();
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
                if let Some(n) = param_ident(acc, value) {
                    let root_n = root(acc, n);
                    if let Some(v) =
                        variant_arm_takes_payload_block(pattern, rest.as_ref(), acc.take_copy_read)
                    {
                        acc.payload_consumers.entry(root_n).or_default().insert(v);
                    }
                    if let Some(v) = variant_arm_payload_escapes_block(pattern, rest.as_ref()) {
                        acc.payload_escapers.entry(root_n).or_default().insert(v);
                    }
                    let cr: &dyn Fn(&Expr) -> bool = acc.copy_read.unwrap_or(&projection_is_read);
                    let fr = acc.follow_rebinds;
                    if let Some(v) =
                        variant_arm_payload_escapes_proj_block(pattern, rest.as_ref(), cr, fr)
                    {
                        acc.payload_escapers_proj
                            .entry(root_n)
                            .or_default()
                            .insert(v);
                        // B-2026-09-14-18 — see the `Match` site.
                        if let Some((pv, parts)) =
                            variant_arm_payload_escaping_parts_block(pattern, rest.as_ref(), cr, fr)
                        {
                            acc.payload_escaper_parts
                                .entry(root_n)
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
        StmtKind::Expr(e) => {
            if let Some(n) = dead_end_param(acc, e) {
                record_read_only_use(acc, n);
            } else {
                walk_expr(e, acc);
            }
        }
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
            if let Some(n) = param_ident(acc, scrutinee) {
                let root_n = root(acc, n);
                let cr: &dyn Fn(&Expr) -> bool = acc.copy_read.unwrap_or(&projection_is_read);
                let fr = acc.follow_rebinds;
                for a in arms {
                    if let Some(v) = variant_arm_takes_payload(
                        &a.pattern,
                        a.guard.as_ref(),
                        &a.body,
                        acc.take_copy_read,
                    ) {
                        acc.payload_consumers.entry(root_n).or_default().insert(v);
                    }
                    if let Some(v) =
                        variant_arm_payload_escapes(&a.pattern, a.guard.as_ref(), &a.body)
                    {
                        acc.payload_escapers.entry(root_n).or_default().insert(v);
                    }
                    if let Some(v) = variant_arm_payload_escapes_proj(
                        &a.pattern,
                        a.guard.as_ref(),
                        &a.body,
                        cr,
                        fr,
                    ) {
                        acc.payload_escapers_proj
                            .entry(root_n)
                            .or_default()
                            .insert(v);
                        // B-2026-09-14-18 — recorded only alongside the map it
                        // narrows, so a part set can never contradict it.
                        if let Some((pv, parts)) = variant_arm_payload_escaping_parts(
                            &a.pattern,
                            a.guard.as_ref(),
                            &a.body,
                            cr,
                            fr,
                        ) {
                            acc.payload_escaper_parts
                                .entry(root_n)
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
            let lent_to = match (&callee.kind, acc.lent) {
                (ExprKind::Identifier(cn), Some(l)) if !acc.in_closure => Some((cn.as_str(), l)),
                _ => None,
            };
            let lent_call = match (&callee.kind, acc.lent_call) {
                (ExprKind::Identifier(_), _) => None,
                (_, Some(l)) if !acc.in_closure => Some(l),
                _ => None,
            };
            for (k, a) in args.iter().enumerate() {
                // B-2026-09-28-13 — `eat(id(a))` lends `a` as `eat(a)` does.
                let n = param_ident(acc, &a.value);
                if let (Some((cn, l)), Some(n)) = (lent_to, n) {
                    if a.label.is_none() && !a.mut_marker && l(cn, k) {
                        record_read_only_use(acc, n);
                        continue;
                    }
                }
                if let (Some(l), Some(n)) = (lent_call, n) {
                    if a.label.is_none() && !a.mut_marker && l(e, k) {
                        record_read_only_use(acc, n);
                        continue;
                    }
                }
                walk_call_arg(a, acc);
            }
        }
        ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } => {
            // B-2026-09-28-34 — a variant probe reads the tag and nothing else.
            let probed = if args.is_empty()
                && matches!(method.as_str(), "is_some" | "is_none" | "is_ok" | "is_err")
            {
                dead_end_param(acc, object)
            } else {
                None
            };
            if let Some(n) = probed {
                record_read_only_use(acc, n);
            } else {
                walk_expr(object, acc);
            }
            let lent_call = acc.lent_call.filter(|_| !acc.in_closure);
            for (k, a) in args.iter().enumerate() {
                let n = param_ident(acc, &a.value);
                if let (Some(l), Some(n)) = (lent_call, n) {
                    if a.label.is_none() && !a.mut_marker && l(e, k) {
                        record_read_only_use(acc, n);
                        continue;
                    }
                }
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
            if let Some(n) = param_ident(acc, value) {
                let root_n = root(acc, n);
                if let Some(v) =
                    variant_arm_takes_payload_block(pattern, Some(then_block), acc.take_copy_read)
                {
                    acc.payload_consumers.entry(root_n).or_default().insert(v);
                }
                if let Some(v) = variant_arm_payload_escapes_block(pattern, Some(then_block)) {
                    acc.payload_escapers.entry(root_n).or_default().insert(v);
                }
                let cr: &dyn Fn(&Expr) -> bool = acc.copy_read.unwrap_or(&projection_is_read);
                let fr = acc.follow_rebinds;
                if let Some(v) =
                    variant_arm_payload_escapes_proj_block(pattern, Some(then_block), cr, fr)
                {
                    acc.payload_escapers_proj
                        .entry(root_n)
                        .or_default()
                        .insert(v);
                    // B-2026-09-14-18 — see the `Match` site.
                    if let Some((pv, parts)) =
                        variant_arm_payload_escaping_parts_block(pattern, Some(then_block), cr, fr)
                    {
                        acc.payload_escaper_parts
                            .entry(root_n)
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
            if let Some(n) = param_ident(acc, value) {
                let root_n = root(acc, n);
                if let Some(v) =
                    variant_arm_takes_payload_block(pattern, Some(body), acc.take_copy_read)
                {
                    acc.payload_consumers.entry(root_n).or_default().insert(v);
                }
                if let Some(v) = variant_arm_payload_escapes_block(pattern, Some(body)) {
                    acc.payload_escapers.entry(root_n).or_default().insert(v);
                }
                let cr: &dyn Fn(&Expr) -> bool = acc.copy_read.unwrap_or(&projection_is_read);
                let fr = acc.follow_rebinds;
                if let Some(v) = variant_arm_payload_escapes_proj_block(pattern, Some(body), cr, fr)
                {
                    acc.payload_escapers_proj
                        .entry(root_n)
                        .or_default()
                        .insert(v);
                    // B-2026-09-14-18 — see the `Match` site.
                    if let Some((pv, parts)) =
                        variant_arm_payload_escaping_parts_block(pattern, Some(body), cr, fr)
                    {
                        acc.payload_escaper_parts
                            .entry(root_n)
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
