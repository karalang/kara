//! B-2026-09-08-3 — reject the one shape whose user `Drop` bodies neither
//! backend can get right, instead of silently swapping them.
//!
//! THE SHAPE. A field is moved out of an owned, still-live struct, and the
//! same base is then handed to a call that assigns that field:
//!
//! ```text
//! let taken = g.one;      // `g.one` is now a husk; `taken` owns the body
//! g.set(mks(7));          // `set` does `self.one = r`
//! ```
//!
//! WHY IT CANNOT BE COMPILED CORRECTLY TODAY. Both backends decide whether a
//! field assignment owes a `Drop` body for the value it displaces by consulting
//! a PER-FRAME move-out record — codegen's `struct_moved_field_bodies`, the
//! interpreter's `moved_out_struct_field_bodies`. `let taken = g.one` records
//! the move against `g` in the CALLER's frame. Inside `set` the base is the
//! receiver, keyed `self`, and the callee's frame carries no such record (the
//! sets are cleared across a call boundary on purpose), so the gate answers
//! "not moved out" and the displacement fires over a husk `taken` already owns.
//!
//! Measured, before this check existed — the method spelling against the direct
//! one B-2026-09-07-63 fixed, byte-identical on `--interp`, JIT, and AOT at
//! `KARAC_OPT_LEVEL` 0 and 2 with `KARAC_AUTO_PAR` on and off:
//!
//! ```text
//! method   dS1 dS2 t1 dS1     <- spurious dS1, and NO dS7 anywhere
//! direct   dS2 dS7 t1 dS1     <- correct on both counts
//! ```
//!
//! It is a body SWAP, not one body running twice: the displacement runs the
//! HUSK's body, and the value the callee actually stored never runs its own.
//! Memory is balanced either way (12 allocs / 12 frees, zero definitely-lost),
//! so no sanitizer catches it.
//!
//! WHY A REJECTION RATHER THAN A FIX. Making the shape WORK requires husk-ness
//! to be visible at runtime inside the callee. Codegen compiles each function
//! once, so a per-call-site mask cannot reach the callee without either drop
//! flags in the object (a layout/ABI change) or per-call-site specialization (a
//! mangling scheme plus a matching interpreter mirror, and still no answer for
//! a dynamically dispatched call). B-2026-09-08-3 records both, plus two
//! mechanisms that look like they would serve and do not: the `field_view_flags`
//! runtime guard and the use-after-move defensive copy are both maps over
//! allocas in the CURRENT frame, so neither is reachable from the callee
//! either. Until one of those two is built, rejecting is the only option that
//! keeps the backends agreeing — neither compiles the shape, so neither can
//! disagree about it.
//!
//! WHY NOT JUST PROMOTE `UseAfterMove` TO AN ERROR, which is the cheap option
//! B-2026-09-08-3 recorded for the owner. Because the warned set is strictly
//! LARGER than the miscompiling set, in a way that matters: a plain `String`
//! reused in a concat chain draws the same `value 'x' moved here, used again
//! here` and compiles CORRECTLY on every surface. Measured over a 1273-file
//! corpus (`examples/`, `runtime/stdlib/`, `kara-katas/`): 8 files draw the
//! warning, and not one of them is this shape — four of the hits are in
//! karac's OWN `runtime/stdlib/protobuf.kara`, which is correct code. So a
//! blanket promotion would reject the standard library to fix a defect the
//! standard library does not have. This check carries the dominance condition
//! over from the use-after-move witness set (see `uam_bindings`) and adds the
//! two structural requirements the miscompile actually needs.
//!
//! WHAT IS DELIBERATELY NOT REJECTED, each measured correct on every surface:
//!
//!   * a DIFFERENT field — `let taken = g.one; g.settwo(mks(7));`
//!   * a move-out through a BORROW (`let x = self.one` off a `ref` base), which
//!     is a projection COPY, not a move: `warning[borrow_projection_copy]` says
//!     so, and both `Drop` bodies firing there is the documented semantics
//!   * a move-out via a helper or method return rather than a direct field read
//!   * a base whose move and later use are dominance-INCOMPARABLE, which the
//!     ownership pass answers with an RC-fallback promotion instead — the base
//!     retains the field, so there is no husk to run a body over

//!
//! ## Direction of incompleteness
//!
//! [`walk_expr`] below carries a `_ => {}` catch-all, which is the OPPOSITE
//! discipline to `binding_use.rs`'s deliberately exhaustive match — and it is
//! the right one here because the two verdicts fail in opposite directions.
//! There, an occurrence the walk misses reads as "no mention" and SUPPRESSES
//! drop bookkeeping, so a miss is unsafe. Here, a call the walk misses simply
//! is not rejected: the program keeps compiling exactly as it does today, with
//! the defect this check exists to catch. A miss costs coverage, never
//! soundness, so the catch-all cannot turn a correct program into a rejected
//! one. Every spelling B-2026-09-08-3 measured as miscompiling is covered, and
//! each is pinned by a test in `tests/ownership.rs`.

use crate::ast::{
    Block, Expr, ExprKind, Function, ImplItem, Item, Param, PatternKind, Program, Stmt, StmtKind,
    TypeKind,
};
use crate::token::Span;
use std::collections::{HashMap, HashSet};

/// One rejected site. The diagnostic points at `call_span`, because the call is
/// the statement the author must change.
#[derive(Debug, Clone)]
pub struct MovedFieldRefill {
    pub binding: String,
    pub field: String,
    /// The callee that assigns the field, named in the diagnostic so the author
    /// knows which call refills it.
    pub callee: String,
    pub move_span: Span,
    pub call_span: Span,
}

/// Which `self` fields a method assigns, keyed by `(type name, method name)`,
/// transitively closed over `self.other(..)` calls.
type MethodAssigns = HashMap<(String, String), HashSet<String>>;

/// Which fields a free function assigns through one of its `mut ref`
/// parameters, keyed by `(fn name, parameter index)`.
type FnAssigns = HashMap<(String, usize), HashSet<String>>;

/// Find every occurrence of the shape in `program`.
///
/// `uam_bindings` carries the dominance condition: `(function key, binding)`
/// pairs for which the ownership pass produced a use-after-move witness. A
/// binding absent from it either was never moved or had its move and use
/// dominance-INCOMPARABLE — the RC-fallback shape, where the base retains the
/// field and there is no husk to run a body over. Gating on it keeps this check
/// from re-deriving, and drifting from, the dominance analysis that exists.
pub fn find_moved_field_refills(
    program: &Program,
    uam_bindings: &HashSet<(String, String)>,
) -> Vec<MovedFieldRefill> {
    let drop_types = collect_user_drop_types(program);
    if drop_types.is_empty() {
        return Vec::new();
    }
    let field_types = collect_struct_field_types(program);
    let (method_assigns, fn_assigns) = collect_assignments(program);
    let mut out = Vec::new();
    for_each_function(program, &mut |f, key| {
        scan_function(
            f,
            key,
            &drop_types,
            &field_types,
            &method_assigns,
            &fn_assigns,
            uam_bindings,
            &mut out,
        );
    });
    out.sort_by_key(|r| (r.call_span.offset, r.move_span.offset, r.binding.clone()));
    out
}

/// Visit every function body in the program with the key this check uses to
/// scope a binding name: `Type::method` for an impl method, the bare name for a
/// free function.
fn for_each_function(program: &Program, f: &mut impl FnMut(&Function, &str)) {
    for item in &program.items {
        match item {
            Item::Function(func) => f(func, &func.name),
            Item::ImplBlock(imp) => {
                let Some(ty) = type_name_of(&imp.target_type.kind) else {
                    continue;
                };
                for ii in &imp.items {
                    if let ImplItem::Method(m) = ii {
                        f(m, &format!("{ty}.{}", m.name));
                    }
                }
            }
            _ => {}
        }
    }
}

fn type_name_of(kind: &TypeKind) -> Option<String> {
    match kind {
        TypeKind::Path(p) => p.segments.last().cloned(),
        _ => None,
    }
}

/// Struct names carrying a user `impl Drop`. Only a field of such a type has a
/// body to swap; a field without one has nothing observable to get wrong, which
/// is why the check does not fire on it.
fn collect_user_drop_types(program: &Program) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in &program.items {
        if let Item::ImplBlock(imp) = item {
            if imp
                .trait_name
                .as_ref()
                .and_then(|p| p.segments.last())
                .map(String::as_str)
                == Some("Drop")
            {
                if let Some(t) = type_name_of(&imp.target_type.kind) {
                    out.insert(t);
                }
            }
        }
    }
    out
}

/// `struct name -> [(field name, field type name)]`.
fn collect_struct_field_types(program: &Program) -> HashMap<String, Vec<(String, String)>> {
    let mut out: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for item in &program.items {
        if let Item::StructDef(s) = item {
            out.insert(
                s.name.clone(),
                s.fields
                    .iter()
                    .filter_map(|f| type_name_of(&f.ty.kind).map(|t| (f.name.clone(), t)))
                    .collect(),
            );
        }
    }
    out
}

/// Build the "which fields does this callee assign" maps, then close them over
/// calls so an assignment reached only through a helper still counts.
///
/// The closure is what makes the two-level spelling reachable: `outer` assigns
/// nothing itself but calls `self.set(r)`, so after the fixpoint `outer` counts
/// as assigning whatever `set` assigns. Iterating to a fixpoint rather than
/// recursing keeps mutual recursion from diverging — the sets only grow and are
/// bounded by the field set, so it terminates.
fn collect_assignments(program: &Program) -> (MethodAssigns, FnAssigns) {
    let mut methods: MethodAssigns = HashMap::new();
    let mut frees: FnAssigns = HashMap::new();
    let mut method_edges: HashMap<(String, String), Vec<String>> = HashMap::new();
    let mut fn_edges: HashMap<(String, usize), Vec<(String, usize)>> = HashMap::new();

    for item in &program.items {
        match item {
            Item::Function(f) => {
                for (idx, p) in f.params.iter().enumerate() {
                    if !matches!(p.ty.kind, TypeKind::MutRef(_)) {
                        continue;
                    }
                    let Some(name) = param_binding_name(p) else {
                        continue;
                    };
                    let mut acc = BaseAcc::new(&name);
                    accumulate_base(&f.body, &mut acc);
                    frees.insert((f.name.clone(), idx), acc.assigned);
                    fn_edges.insert((f.name.clone(), idx), acc.fn_forwards);
                }
            }
            Item::ImplBlock(imp) => {
                let Some(ty) = type_name_of(&imp.target_type.kind) else {
                    continue;
                };
                for ii in &imp.items {
                    let ImplItem::Method(m) = ii else { continue };
                    let mut acc = BaseAcc::new("self");
                    accumulate_base(&m.body, &mut acc);
                    methods.insert((ty.clone(), m.name.clone()), acc.assigned);
                    method_edges.insert((ty.clone(), m.name.clone()), acc.method_forwards);
                }
            }
            _ => {}
        }
    }

    loop {
        let mut changed = false;
        for ((ty, m), callees) in &method_edges {
            let add: HashSet<String> = callees
                .iter()
                .filter_map(|c| methods.get(&(ty.clone(), c.clone())))
                .flat_map(|s| s.iter().cloned())
                .collect();
            if let Some(cur) = methods.get_mut(&(ty.clone(), m.clone())) {
                for f in add {
                    changed |= cur.insert(f);
                }
            }
        }
        for (key, callees) in &fn_edges {
            let add: HashSet<String> = callees
                .iter()
                .filter_map(|c| frees.get(c))
                .flat_map(|s| s.iter().cloned())
                .collect();
            if let Some(cur) = frees.get_mut(key) {
                for f in add {
                    changed |= cur.insert(f);
                }
            }
        }
        if !changed {
            break;
        }
    }
    (methods, frees)
}

fn param_binding_name(p: &Param) -> Option<String> {
    match &p.pattern.kind {
        PatternKind::Binding(name) => Some(name.clone()),
        _ => None,
    }
}

/// Accumulator for "what does this body do to one base binding": the fields it
/// assigns directly, and the calls through which it forwards the base.
struct BaseAcc<'a> {
    base: &'a str,
    assigned: HashSet<String>,
    /// `self.other(..)` — method names called on the same base.
    method_forwards: Vec<String>,
    /// `g(.., mut base, ..)` — `(callee name, parameter index)` the base is
    /// forwarded into.
    fn_forwards: Vec<(String, usize)>,
}

impl<'a> BaseAcc<'a> {
    fn new(base: &'a str) -> Self {
        Self {
            base,
            assigned: HashSet::new(),
            method_forwards: Vec::new(),
            fn_forwards: Vec::new(),
        }
    }
}

fn is_identifier(e: &Expr, name: &str) -> bool {
    match &e.kind {
        ExprKind::Identifier(n) => n == name,
        ExprKind::SelfValue => name == "self",
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Generic traversal
//
// Two walkers, both recursing through every nested block: one over statements
// (the assignment shape is statement-level) and one over expressions (the call
// shape is not). See the module docs' "Direction of incompleteness" for why the
// `_ => {}` arms here cost coverage and never soundness.
// ---------------------------------------------------------------------------

fn for_each_stmt(b: &Block, f: &mut impl FnMut(&Stmt)) {
    for s in &b.stmts {
        f(s);
        each_block_in_stmt(s, &mut |inner| for_each_stmt(inner, f));
    }
    if let Some(e) = &b.final_expr {
        each_block_in_expr(e, &mut |inner| for_each_stmt(inner, f));
    }
}

fn for_each_expr(b: &Block, f: &mut impl FnMut(&Expr)) {
    for s in &b.stmts {
        each_expr_in_stmt(s, f);
    }
    if let Some(e) = &b.final_expr {
        expr_rec(e, f);
    }
}

fn each_expr_in_stmt(s: &Stmt, f: &mut impl FnMut(&Expr)) {
    match &s.kind {
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            expr_rec(target, f);
            expr_rec(value, f);
        }
        StmtKind::Let { value, .. } => expr_rec(value, f),
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            expr_rec(value, f);
            for_each_expr(else_block, f);
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => for_each_expr(body, f),
        StmtKind::Expr(e) => expr_rec(e, f),
        StmtKind::LetUninit { .. } | StmtKind::MultiAssign { .. } => {}
    }
}

fn expr_rec(e: &Expr, f: &mut impl FnMut(&Expr)) {
    f(e);
    match &e.kind {
        ExprKind::MethodCall { object, args, .. } => {
            expr_rec(object, f);
            for a in args {
                expr_rec(&a.value, f);
            }
        }
        ExprKind::Call { callee, args } => {
            expr_rec(callee, f);
            for a in args {
                expr_rec(&a.value, f);
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            expr_rec(object, f)
        }
        ExprKind::Index { object, index } => {
            expr_rec(object, f);
            expr_rec(index, f);
        }
        ExprKind::Binary { left, right, .. } => {
            expr_rec(left, f);
            expr_rec(right, f);
        }
        ExprKind::Unary { operand, .. } => expr_rec(operand, f),
        ExprKind::Cast { expr, .. } => expr_rec(expr, f),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            expr_rec(condition, f);
            for_each_expr(then_block, f);
            if let Some(eb) = else_branch {
                expr_rec(eb, f);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            expr_rec(value, f);
            for_each_expr(then_block, f);
            if let Some(eb) = else_branch {
                expr_rec(eb, f);
            }
        }
        ExprKind::WhileLet { value, body, .. } => {
            expr_rec(value, f);
            for_each_expr(body, f);
        }
        ExprKind::Block(b) => for_each_expr(b, f),
        ExprKind::While {
            condition, body, ..
        } => {
            expr_rec(condition, f);
            for_each_expr(body, f);
        }
        ExprKind::Loop { body, .. } => for_each_expr(body, f),
        ExprKind::For { iterable, body, .. } => {
            expr_rec(iterable, f);
            for_each_expr(body, f);
        }
        ExprKind::Match { scrutinee, arms } => {
            expr_rec(scrutinee, f);
            for arm in arms {
                expr_rec(&arm.body, f);
            }
        }
        ExprKind::Tuple(xs) | ExprKind::ArrayLiteral(xs) => {
            for x in xs {
                expr_rec(x, f);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for fi in fields {
                expr_rec(&fi.value, f);
            }
            if let Some(sp) = spread {
                expr_rec(sp, f);
            }
        }
        ExprKind::Return(Some(x)) => expr_rec(x, f),
        _ => {}
    }
}

fn each_block_in_stmt(s: &Stmt, f: &mut impl FnMut(&Block)) {
    match &s.kind {
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            f(else_block);
            each_block_in_expr(value, f);
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => f(body),
        StmtKind::Expr(e) => each_block_in_expr(e, f),
        StmtKind::Let { value, .. } => each_block_in_expr(value, f),
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            each_block_in_expr(target, f);
            each_block_in_expr(value, f);
        }
        StmtKind::LetUninit { .. } | StmtKind::MultiAssign { .. } => {}
    }
}

fn each_block_in_expr(e: &Expr, f: &mut impl FnMut(&Block)) {
    expr_rec(e, &mut |x| match &x.kind {
        ExprKind::Block(b) => f(b),
        ExprKind::If { then_block, .. } => f(then_block),
        ExprKind::While { body, .. } | ExprKind::Loop { body, .. } | ExprKind::For { body, .. } => {
            f(body)
        }
        _ => {}
    });
}

// ---------------------------------------------------------------------------
// Collectors
// ---------------------------------------------------------------------------

/// Fill a [`BaseAcc`] for one base binding over a whole body.
fn accumulate_base(b: &Block, acc: &mut BaseAcc<'_>) {
    let base = acc.base.to_string();
    let mut assigned = HashSet::new();
    for_each_stmt(b, &mut |s| {
        if let StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } = &s.kind {
            if let ExprKind::FieldAccess { object, field } = &target.kind {
                if is_identifier(object, &base) {
                    assigned.insert(field.clone());
                }
            }
        }
    });
    acc.assigned.extend(assigned);
    for_each_expr(b, &mut |e| match &e.kind {
        ExprKind::MethodCall { object, method, .. } => {
            if is_identifier(object, &base) {
                acc.method_forwards.push(method.clone());
            }
        }
        ExprKind::Call { callee, args } => {
            if let ExprKind::Identifier(name) = &callee.kind {
                for (i, a) in args.iter().enumerate() {
                    if is_identifier(&a.value, &base) {
                        acc.fn_forwards.push((name.clone(), i));
                    }
                }
            }
        }
        _ => {}
    });
}

/// Record `let g = Bs { .. }` / `let g: Bs = ..` root bindings.
fn collect_root_types(b: &Block, roots: &mut HashMap<String, String>) {
    for_each_stmt(b, &mut |s| {
        let StmtKind::Let {
            pattern, ty, value, ..
        } = &s.kind
        else {
            return;
        };
        let PatternKind::Binding(name) = &pattern.kind else {
            return;
        };
        let resolved =
            ty.as_ref()
                .and_then(|t| type_name_of(&t.kind))
                .or_else(|| match &value.kind {
                    ExprKind::StructLiteral { path, .. } => path.last().cloned(),
                    _ => None,
                });
        if let Some(t) = resolved {
            roots.insert(name.clone(), t);
        }
    });
}

/// Record every `let X = <place>.<field>;` whose field type carries a user
/// `Drop` — the move that creates the husk.
fn collect_field_moves(
    b: &Block,
    roots: &HashMap<String, String>,
    field_types: &HashMap<String, Vec<(String, String)>>,
    drop_types: &HashSet<String>,
    out: &mut Vec<(Vec<String>, String, Span)>,
) {
    for_each_stmt(b, &mut |s| {
        let StmtKind::Let { value, .. } = &s.kind else {
            return;
        };
        let ExprKind::FieldAccess { object, field } = &value.kind else {
            return;
        };
        let Some(base_path) = place_path(object) else {
            return;
        };
        let Some(base_ty) = resolve_place_type(&base_path, roots, field_types) else {
            return;
        };
        let is_drop_field = field_types
            .get(&base_ty)
            .map(|fs| fs.iter().any(|(n, t)| n == field && drop_types.contains(t)))
            .unwrap_or(false);
        if is_drop_field {
            out.push((base_path, field.clone(), value.span));
        }
    });
}

/// Find calls that assign `field` on `base_path` — a method on the base whose
/// `self`-assignment set contains it, or a free function the base is forwarded
/// into at a parameter whose assignment set contains it.
#[allow(clippy::too_many_arguments)]
fn collect_refilling_calls(
    b: &Block,
    base_path: &[String],
    base_ty: &str,
    field: &str,
    method_assigns: &MethodAssigns,
    fn_assigns: &FnAssigns,
    out: &mut Vec<(String, Span)>,
) {
    for_each_expr(b, &mut |e| match &e.kind {
        ExprKind::MethodCall { object, method, .. } => {
            if place_path(object).as_deref() == Some(base_path)
                && method_assigns
                    .get(&(base_ty.to_string(), method.clone()))
                    .map(|s| s.contains(field))
                    .unwrap_or(false)
            {
                out.push((method.clone(), e.span));
            }
        }
        ExprKind::Call { callee, args } => {
            if let ExprKind::Identifier(name) = &callee.kind {
                for (i, a) in args.iter().enumerate() {
                    if place_path(&a.value).as_deref() == Some(base_path)
                        && fn_assigns
                            .get(&(name.clone(), i))
                            .map(|s| s.contains(field))
                            .unwrap_or(false)
                    {
                        out.push((name.clone(), e.span));
                    }
                }
            }
        }
        _ => {}
    });
}

// ---------------------------------------------------------------------------
// Place resolution and the scan
// ---------------------------------------------------------------------------

/// The dotted path of a place expression rooted at a binding: `g` -> `["g"]`,
/// `o.b` -> `["o", "b"]`. `None` for anything rooted at a call, an index or a
/// literal — not a base this check can track.
fn place_path(e: &Expr) -> Option<Vec<String>> {
    match &e.kind {
        ExprKind::Identifier(n) => Some(vec![n.clone()]),
        ExprKind::SelfValue => Some(vec!["self".to_string()]),
        ExprKind::FieldAccess { object, field } => {
            let mut p = place_path(object)?;
            p.push(field.clone());
            Some(p)
        }
        _ => None,
    }
}

/// Resolve the struct type of a place path from the root bindings this scan
/// pinned down.
///
/// Deliberately partial: a base whose type cannot be pinned is not checked.
/// That is the safe direction, and it avoids threading `TypeCheckResult`
/// through for this one lookup — the shape needs a struct literal, a type
/// annotation or `self`, and every spelling B-2026-09-08-3 measured has one.
fn resolve_place_type(
    path: &[String],
    roots: &HashMap<String, String>,
    field_types: &HashMap<String, Vec<(String, String)>>,
) -> Option<String> {
    let mut ty = roots.get(path.first()?)?.clone();
    for seg in &path[1..] {
        let fields = field_types.get(&ty)?;
        ty = fields.iter().find(|(n, _)| n == seg)?.1.clone();
    }
    Some(ty)
}

#[allow(clippy::too_many_arguments)]
fn scan_function(
    f: &Function,
    fn_key: &str,
    drop_types: &HashSet<String>,
    field_types: &HashMap<String, Vec<(String, String)>>,
    method_assigns: &MethodAssigns,
    fn_assigns: &FnAssigns,
    uam_bindings: &HashSet<(String, String)>,
    out: &mut Vec<MovedFieldRefill>,
) {
    let mut roots: HashMap<String, String> = HashMap::new();
    // `fn_key` is `Type.method` for an impl method (the ownership pass's own
    // spelling, shared so the `uam_bindings` lookup needs no translation) and a
    // bare name for a free function.
    if let Some((ty, _)) = fn_key.split_once('.') {
        roots.insert("self".to_string(), ty.to_string());
    }
    collect_root_types(&f.body, &mut roots);

    let mut moves: Vec<(Vec<String>, String, Span)> = Vec::new();
    collect_field_moves(&f.body, &roots, field_types, drop_types, &mut moves);
    if moves.is_empty() {
        return;
    }

    for (base_path, field, move_span) in &moves {
        // The dominance gate: only a base the ownership pass already reported
        // as used-after-move can be sitting on a husk at the call.
        if !uam_bindings.contains(&(fn_key.to_string(), base_path[0].clone())) {
            continue;
        }
        let Some(base_ty) = resolve_place_type(base_path, &roots, field_types) else {
            continue;
        };
        let mut hits = Vec::new();
        collect_refilling_calls(
            &f.body,
            base_path,
            &base_ty,
            field,
            method_assigns,
            fn_assigns,
            &mut hits,
        );
        for (callee, call_span) in hits {
            // Only a call AFTER the move can refill the husk it created.
            if call_span.offset <= move_span.offset {
                continue;
            }
            out.push(MovedFieldRefill {
                binding: base_path.join("."),
                field: field.clone(),
                callee,
                move_span: *move_span,
                call_span,
            });
        }
    }
}
