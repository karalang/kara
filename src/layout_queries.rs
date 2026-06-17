//! Layout-choice queries — P1.5 catalogue entry, phase-8-stdlib-floor.md
//! § Compiler queries channel. Spec at `docs/design.md § Feature 1`
//! (layout blocks) + the P1.5 row of the P1 catalogue table.
//!
//! Surfaces [`QueryKind::LayoutChoice`]: a loop that iterates a
//! collection of a user struct and reads only a *strict subset* of that
//! struct's fields is a candidate for a struct-of-arrays (`layout`
//! block) — grouping the loop-hot fields packs them contiguously so each
//! cache line carries only data the loop touches. The query surfaces the
//! opportunity; the author resolves it by writing the existing `layout`
//! block syntax (no attribute).
//!
//! ### This implements the "`karac explain` suggests a layout" analysis
//!
//! `design.md § Feature 1` advertises that the compiler "statically
//! analyzes field access patterns — identifying which fields of a
//! collection appear together in tight iteration loops — and emits a
//! concrete `layout` block." That analysis did not exist (the
//! `karac explain` command only renders static concept/error-class
//! pages). This module is that analysis, delivered through the query
//! channel: it reads `TypeCheckResult.expr_types` (to type the iterated
//! collection) and `TypeCheckResult.struct_info` (the element struct's
//! fields), then walks the loop body for field reads on the loop binding.
//! Like the P1.1/P1.2/P1.3/P1.6 analyzers it is a plain-data pass run
//! from the CLI's `query_queries` collator, consuming `&Program` +
//! `&TypeCheckResult` and emitting `Vec<CompilerQuery>`.
//!
//! ### v1 scope / limitations
//!
//! - **Access-pattern heuristic, not a SoA-vs-AoS cost model.** The
//!   catalogue's "field grouping alternatives the cost model considered"
//!   framing presumes a layout cost model that does not exist at v1.
//!   This analyzer fires on the clear-cut signal — a *strict subset* of
//!   fields read in a loop over a struct collection — and proposes the
//!   SoA direction. It does not score AoS vs SoA or rank groupings.
//! - **Direct `for <name> in <collection>` only.** The loop binding must
//!   be a single name and the iterated expression must itself type as
//!   `Vec[S]` / `Array[S]` / `Slice[S]` for a struct `S`. Iterator
//!   adaptors (`coll.iter()...`), destructuring loop patterns, and
//!   `Map`/`Set` collections are out of v1 scope (under-report).
//! - **Suppressed once a `layout` block names the struct.** A struct
//!   already referenced by any `layout` block's collection type is
//!   treated as resolved.

use crate::ast::*;
use crate::def_path::{DefPath, QueryId, SubItemHash};
use crate::queries::{CompilerQuery, Confidence, Phase, QueryKind, QueryOption, ResolutionSurface};
use crate::resolver::SpanKey;
use crate::typechecker::{Type, TypeCheckResult};
use std::collections::{BTreeSet, HashSet};

/// A struct must have at least this many fields for a grouping choice to
/// exist — a one-field struct has nothing to split SoA-wise.
const MIN_STRUCT_FIELDS: usize = 2;

/// Entry point. Emits one [`QueryKind::LayoutChoice`] per loop that
/// reads a strict subset of a struct collection's fields, unless the
/// struct is already named by a `layout` block.
pub fn analyze(program: &Program, tc: &TypeCheckResult) -> Vec<CompilerQuery> {
    let covered = collect_layout_covered_structs(program, tc);
    let mut queries = Vec::new();

    for item in &program.items {
        match item {
            Item::Function(f) => {
                let def_path = DefPath::item(f.name.clone());
                walk_block(&f.body, &def_path, tc, &covered, &mut queries);
            }
            Item::ImplBlock(imp) => {
                let TypeKind::Path(p) = &imp.target_type.kind else {
                    continue;
                };
                let Some(target) = p.segments.last() else {
                    continue;
                };
                for impl_item in &imp.items {
                    if let ImplItem::Method(m) = impl_item {
                        let def_path = DefPath::new(vec![target.clone(), m.name.clone()]);
                        walk_block(&m.body, &def_path, tc, &covered, &mut queries);
                    }
                }
            }
            _ => {}
        }
    }

    queries.sort_by_key(|q| q.site.offset);
    queries
}

/// Struct names referenced by any `layout` block's collection type — the
/// decision is already resolved for these, so their loops emit no query.
fn collect_layout_covered_structs(program: &Program, tc: &TypeCheckResult) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in &program.items {
        if let Item::LayoutDef(l) = item {
            collect_type_expr_struct_names(&l.collection_type, tc, &mut out);
        }
    }
    out
}

/// Walk a `TypeExpr`, inserting every single-segment path name that is a
/// known struct. Conservative — a struct mentioned anywhere in a layout
/// block's collection type counts as covered.
fn collect_type_expr_struct_names(ty: &TypeExpr, tc: &TypeCheckResult, out: &mut HashSet<String>) {
    if let TypeKind::Path(p) = &ty.kind {
        if let Some(last) = p.segments.last() {
            if tc.struct_info.contains_key(last) {
                out.insert(last.clone());
            }
        }
        if let Some(args) = &p.generic_args {
            for arg in args {
                if let GenericArg::Type(t) = arg {
                    collect_type_expr_struct_names(t, tc, out);
                }
            }
        }
    }
}

// ── Body walk ───────────────────────────────────────────────────

fn walk_block(
    block: &Block,
    def_path: &DefPath,
    tc: &TypeCheckResult,
    covered: &HashSet<String>,
    out: &mut Vec<CompilerQuery>,
) {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                walk_expr(value, def_path, tc, covered, out)
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                walk_expr(target, def_path, tc, covered, out);
                walk_expr(value, def_path, tc, covered, out);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                walk_block(body, def_path, tc, covered, out)
            }
            StmtKind::Expr(e) => walk_expr(e, def_path, tc, covered, out),
            StmtKind::LetUninit { .. } => {}
        }
    }
    if let Some(tail) = &block.final_expr {
        walk_expr(tail, def_path, tc, covered, out);
    }
}

fn walk_expr(
    expr: &Expr,
    def_path: &DefPath,
    tc: &TypeCheckResult,
    covered: &HashSet<String>,
    out: &mut Vec<CompilerQuery>,
) {
    if let ExprKind::For {
        pattern,
        iterable,
        body,
        ..
    } = &expr.kind
    {
        if let Some(q) = layout_query_for_loop(expr, pattern, iterable, body, def_path, tc, covered)
        {
            out.push(q);
        }
    }
    // Descend into every child so nested loops are still analyzed.
    for child in child_exprs(expr) {
        walk_expr(child, def_path, tc, covered, out);
    }
    for block in child_blocks(expr) {
        walk_block(block, def_path, tc, covered, out);
    }
}

/// The heart of P1.5: decide whether one `for` loop over a struct
/// collection reads a strict subset of the struct's fields.
fn layout_query_for_loop(
    loop_expr: &Expr,
    pattern: &Pattern,
    iterable: &Expr,
    body: &Block,
    def_path: &DefPath,
    tc: &TypeCheckResult,
    covered: &HashSet<String>,
) -> Option<CompilerQuery> {
    // Single-name binding only (`for e in …`).
    let names = pattern.binding_names();
    let [elem] = names.as_slice() else {
        return None;
    };

    // Type the iterated collection and recover its element struct.
    let coll_ty = tc.expr_types.get(&SpanKey::from_span(&iterable.span))?;
    let struct_name = element_struct_name(coll_ty, tc)?;
    if covered.contains(&struct_name) {
        return None;
    }

    let fields: BTreeSet<&String> = tc
        .struct_info
        .get(&struct_name)?
        .fields
        .iter()
        .map(|(n, _, _)| n)
        .collect();
    if fields.len() < MIN_STRUCT_FIELDS {
        return None;
    }

    // Collect field reads on the loop binding within the body, keeping
    // only names that are real fields of the element struct.
    let mut accessed: BTreeSet<String> = BTreeSet::new();
    collect_field_reads(body, elem, &fields, &mut accessed);

    // Fire only on a strict, non-empty subset — reading every field has
    // no SoA win, reading none is not a field-wise loop.
    if accessed.is_empty() || accessed.len() >= fields.len() {
        return None;
    }

    Some(build_layout_query(
        loop_expr,
        def_path,
        &struct_name,
        &accessed,
        fields.len(),
    ))
}

/// Recover the element struct name of a sequence collection type:
/// `Vec[S]` / `Array[S]` / `Slice[S]` where `S` is a known struct.
fn element_struct_name(ty: &Type, tc: &TypeCheckResult) -> Option<String> {
    let elem = match ty {
        Type::Named { name, args } if name == "Vec" && args.len() == 1 => &args[0],
        Type::Array { element, .. } | Type::Slice { element, .. } => element,
        _ => return None,
    };
    match elem {
        Type::Named { name, .. } if tc.struct_info.contains_key(name) => Some(name.clone()),
        _ => None,
    }
}

/// Walk `block`, recording every `<elem>.<field>` read whose field is in
/// `fields`. Field *writes* (`e.x = …`) count too — a write-only loop
/// over a subset still benefits from SoA packing.
fn collect_field_reads(
    block: &Block,
    elem: &str,
    fields: &BTreeSet<&String>,
    out: &mut BTreeSet<String>,
) {
    fn visit_expr(expr: &Expr, elem: &str, fields: &BTreeSet<&String>, out: &mut BTreeSet<String>) {
        if let ExprKind::FieldAccess { object, field } = &expr.kind {
            if let ExprKind::Identifier(name) = &object.kind {
                if name == elem && fields.contains(field) {
                    out.insert(field.clone());
                }
            }
        }
        for child in child_exprs(expr) {
            visit_expr(child, elem, fields, out);
        }
        for b in child_blocks(expr) {
            visit_block(b, elem, fields, out);
        }
    }
    fn visit_block(
        block: &Block,
        elem: &str,
        fields: &BTreeSet<&String>,
        out: &mut BTreeSet<String>,
    ) {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                    visit_expr(value, elem, fields, out)
                }
                StmtKind::Assign { target, value }
                | StmtKind::CompoundAssign { target, value, .. } => {
                    visit_expr(target, elem, fields, out);
                    visit_expr(value, elem, fields, out);
                }
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    visit_block(body, elem, fields, out)
                }
                StmtKind::Expr(e) => visit_expr(e, elem, fields, out),
                StmtKind::LetUninit { .. } => {}
            }
        }
        if let Some(tail) = &block.final_expr {
            visit_expr(tail, elem, fields, out);
        }
    }
    visit_block(block, elem, fields, out);
}

fn build_layout_query(
    loop_expr: &Expr,
    def_path: &DefPath,
    struct_name: &str,
    accessed: &BTreeSet<String>,
    total_fields: usize,
) -> CompilerQuery {
    let hot: Vec<&str> = accessed.iter().map(|s| s.as_str()).collect();
    let hot_list = hot.join(", ");
    CompilerQuery {
        id: QueryId {
            def_path: def_path.clone(),
            sub_item_hash: SubItemHash(loop_expr.span.offset as u64),
        },
        site: loop_expr.span.clone(),
        kind: QueryKind::LayoutChoice,
        options: vec![
            QueryOption {
                label: "keep_aos".to_string(),
                note: Some(format!(
                    "`Vec[{}]` uses the default array-of-structs layout; this loop reads {} of {} fields ({})",
                    struct_name,
                    accessed.len(),
                    total_fields,
                    hot_list,
                )),
            },
            QueryOption {
                label: "group_hot_fields".to_string(),
                note: Some(format!(
                    "write a `layout` block for `Vec[{}]` grouping the loop-hot fields ({}) so each \
                     cache line carries only data this loop touches (struct-of-arrays)",
                    struct_name, hot_list,
                )),
            },
        ],
        // Default: keep AoS — no layout block is the current state.
        default: 0,
        // `Low`: this is an access-pattern heuristic with no backing
        // SoA-vs-AoS cost model, so it is genuinely worth confirming.
        default_confidence: Confidence::Low,
        // The resolution is the `layout` block *syntax*, not an
        // attribute, so the attribute surface is empty — the query
        // self-resolves once a layout block names the struct.
        resolution_surface: ResolutionSurface {
            attributes: Vec::new(),
        },
        cross_phase_origin: Some(Phase::Codegen),
    }
}

// ── Generic child-expr / child-block enumeration ────────────────

/// Every sub-expression of `expr` (one level down). Used by the walker
/// and the field-read collector so both descend uniformly.
fn child_exprs(expr: &Expr) -> Vec<&Expr> {
    let mut v: Vec<&Expr> = Vec::new();
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            v.push(callee);
            v.extend(args.iter().map(|a| &a.value));
        }
        ExprKind::MethodCall { object, args, .. } => {
            v.push(object);
            v.extend(args.iter().map(|a| &a.value));
        }
        ExprKind::Binary { left, right, .. } => {
            v.push(left);
            v.push(right);
        }
        ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => v.push(operand),
        ExprKind::NilCoalesce { left, right } => {
            v.push(left);
            v.push(right);
        }
        ExprKind::OptionalChain { object, args, .. } => {
            v.push(object);
            if let Some(args) = args {
                v.extend(args.iter().map(|a| &a.value));
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            v.push(object)
        }
        ExprKind::Index { object, index } => {
            v.push(object);
            v.push(index);
        }
        ExprKind::If {
            condition,
            else_branch,
            ..
        } => {
            v.push(condition);
            if let Some(eb) = else_branch {
                v.push(eb);
            }
        }
        ExprKind::IfLet {
            value, else_branch, ..
        } => {
            v.push(value);
            if let Some(eb) = else_branch {
                v.push(eb);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            v.push(scrutinee);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    v.push(g);
                }
                v.push(&arm.body);
            }
        }
        ExprKind::While { condition, .. } => v.push(condition),
        ExprKind::WhileLet { value, .. } => v.push(value),
        ExprKind::For { iterable, .. } => v.push(iterable),
        ExprKind::Closure { body, .. } => v.push(body),
        ExprKind::Return(Some(e)) => v.push(e),
        ExprKind::Break { value: Some(e), .. } => v.push(e),
        ExprKind::Tuple(items)
        | ExprKind::ArrayLiteral(items)
        | ExprKind::PrefixCollectionLiteral { items, .. } => v.extend(items.iter()),
        _ => {}
    }
    v
}

/// Every sub-block of `expr` (one level down) — the block-valued arms a
/// `child_exprs` walk does not reach.
fn child_blocks(expr: &Expr) -> Vec<&Block> {
    let mut v: Vec<&Block> = Vec::new();
    match &expr.kind {
        ExprKind::Block(b) | ExprKind::LabeledBlock { body: b, .. } => v.push(b),
        ExprKind::If { then_block, .. } => v.push(then_block),
        ExprKind::IfLet { then_block, .. } => v.push(then_block),
        ExprKind::While { body, .. }
        | ExprKind::WhileLet { body, .. }
        | ExprKind::For { body, .. }
        | ExprKind::Loop { body, .. } => v.push(body),
        _ => {}
    }
    v
}
