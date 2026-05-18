//! Codegen queries — P1.3 catalogue entry, phase-7-codegen.md line 25.
//!
//! Surfaces two query kinds derived from a pre-codegen AST walk:
//!
//!   1. [`QueryKind::InliningDecision`] — `pub fn`s called from
//!      three-or-more sites that lie inside loops, where the
//!      definition has no `#[inline]` / `#[inline(never)]` annotation
//!      and is small enough that the inline decision is plausibly
//!      load-bearing.
//!
//!   2. [`QueryKind::BranchHint`] — `match` expressions and `if`/`else`
//!      pairs whose arm/branch body sizes are asymmetric enough that a
//!      `#[likely]` / `#[unlikely]` annotation would change branch
//!      layout, but no such annotation is present today.
//!
//! ### Why this lives outside `src/codegen.rs`
//!
//! Codegen-side hooks per the tracker entry, but the heuristics are
//! plain-data walks over the AST + a handful of attribute presence
//! checks. The codegen-containment invariant (`CLAUDE.md § Codegen
//! containment`) is that `inkwell`-typed values stay inside
//! `src/codegen*`. This module needs none of that: it consumes
//! `&Program` and emits `Vec<CompilerQuery>`. Keeping it outside the
//! `--features llvm` gate means `karac query queries` works regardless
//! of whether the LLVM backend is built — the tooling surface for
//! query consumers is uniform across feature configurations.
//!
//! ### v1 limitations
//!
//! - **No arm-level / branch-level attribute suppression yet.** The
//!   parser does not attach `#[likely]` / `#[unlikely]` to `MatchArm`
//!   bodies or `If` branches today; the `resolution_surface` field on
//!   each emitted query still names those attributes so downstream
//!   tools render the correct guidance, and a future parser update
//!   wires the suppression check at that point. Inlining-site
//!   suppression *is* honored because `Function.attributes` is
//!   well-populated.
//!
//! - **SubItemHash is a per-compile site index, not a structural
//!   hash.** The `def_path.rs` module documents `SubItemHash::of_expr`
//!   as a stub returning `ROOT`; this analyzer uses the byte offset of
//!   the decision site's span as the disambiguator within an item so
//!   multiple sites in one function get distinct query IDs without a
//!   full structural hash. Edit stability across runs is best-effort
//!   for v1; the cross-compile-stable hash lands when tools persist
//!   resolved answers (P1.x maturation).
//!
//! - **No type-checker integration.** Call-site discovery is by
//!   plain identifier match against pub-fn names; method calls, trait
//!   dispatch, and re-exported names are out of v1 scope. The
//!   conservative shape is fine for v1: we under-report rather than
//!   over-report, and the catalogue is informational.

use crate::ast::*;
use crate::def_path::{DefPath, QueryId, SubItemHash};
use crate::queries::{CompilerQuery, Confidence, Phase, QueryKind, QueryOption, ResolutionSurface};
use crate::token::Span;
use std::collections::HashMap;

/// Minimum number of inside-loop call sites a pub fn needs before
/// inlining becomes a worth-asking question. Three is the
/// tracker-entry's stated threshold ("`f` is called from 3 hot-looking
/// sites; inline?").
const INLINE_QUERY_LOOP_SITE_THRESHOLD: usize = 3;

/// Function bodies larger than this (in statement count) are large
/// enough that an inline decision is unlikely to be the load-bearing
/// factor — LLVM's pipeline will inline-or-not based on size heuristics
/// before any query confirmation would matter. Suppresses queries on
/// big functions so the report focuses on small hot helpers where
/// inlining moves the needle.
const INLINE_QUERY_BODY_SIZE_CAP: usize = 20;

/// Branch-hint queries fire when one arm/branch body has at least this
/// many statements *and* outweighs the lightest peer by at least
/// `BRANCH_HINT_RATIO`. The lower bound suppresses noise from short
/// arms; the ratio captures the "cost model considers the choice
/// notable" wording on the tracker entry without needing real cost
/// modelling.
const BRANCH_HINT_MIN_HEAVY_STMTS: usize = 3;
const BRANCH_HINT_RATIO: usize = 3;

/// Entry point. Walks `program` and emits the union of P1.3 codegen
/// queries (inlining + branch-hint).
pub fn analyze(program: &Program) -> Vec<CompilerQuery> {
    let mut queries = Vec::new();

    let pub_fns = collect_pub_fns(program);
    let call_counts = collect_call_sites_in_loops(program, &pub_fns);

    for (name, loop_call_count) in &call_counts {
        if *loop_call_count < INLINE_QUERY_LOOP_SITE_THRESHOLD {
            continue;
        }
        let info = match pub_fns.get(name) {
            Some(info) => info,
            None => continue,
        };
        if info.has_inline_attr {
            continue;
        }
        if info.body_stmt_count > INLINE_QUERY_BODY_SIZE_CAP {
            continue;
        }
        queries.push(build_inlining_query(name, info, *loop_call_count));
    }

    // Stable order — the inlining loop iterates a HashMap so sort by
    // span before emitting to keep tooling output deterministic.
    queries.sort_by_key(|q| q.site.offset);

    let mut branch_hint_queries = collect_branch_hint_queries(program);
    queries.append(&mut branch_hint_queries);

    queries
}

#[derive(Debug, Clone)]
struct PubFnInfo {
    def_path: DefPath,
    span: Span,
    body_stmt_count: usize,
    has_inline_attr: bool,
}

fn collect_pub_fns(program: &Program) -> HashMap<String, PubFnInfo> {
    let mut out = HashMap::new();
    for item in &program.items {
        if let Item::Function(f) = item {
            if !f.is_pub {
                continue;
            }
            let info = PubFnInfo {
                def_path: DefPath::item(f.name.clone()),
                span: f.span.clone(),
                body_stmt_count: count_block_stmts(&f.body),
                has_inline_attr: has_inline_resolution_attr(&f.attributes),
            };
            out.insert(f.name.clone(), info);
        }
    }
    out
}

fn has_inline_resolution_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        let name = attribute_simple_name(a);
        matches!(name.as_deref(), Some("inline" | "inline(never)"))
    })
}

fn attribute_simple_name(attr: &Attribute) -> Option<String> {
    // Single-segment paths render as their last segment; that matches
    // how `inline` / `inline(never)` are written in source. Multi-segment
    // / tool-namespaced paths are ignored — they don't resolve a P1.3
    // query.
    if attr.path.len() != 1 {
        return None;
    }
    let base = attr.path[0].clone();
    // `#[inline(never)]` arrives as `path = ["inline"]` with one
    // positional `AttrArg { name: None, value: Some(Identifier("never")) }`
    // (see `parse_attribute` — bare identifiers parse as the positional
    // form because `never` is followed by `)`, not `=` / `:`).
    if base == "inline" && attr.args.len() == 1 {
        let arg = &attr.args[0];
        if arg.name.is_none() {
            if let Some(value) = &arg.value {
                if let ExprKind::Identifier(s) = &value.kind {
                    if s == "never" {
                        return Some("inline(never)".to_string());
                    }
                }
            }
        }
    }
    Some(base)
}

fn build_inlining_query(name: &str, info: &PubFnInfo, loop_call_count: usize) -> CompilerQuery {
    CompilerQuery {
        id: QueryId {
            def_path: info.def_path.clone(),
            sub_item_hash: SubItemHash::ROOT,
        },
        site: info.span.clone(),
        kind: QueryKind::InliningDecision,
        options: vec![
            QueryOption {
                label: "inline".to_string(),
                note: Some(format!(
                    "called from {} hot-looking sites; inline `{}` at all callers",
                    loop_call_count, name
                )),
            },
            QueryOption {
                label: "keep_call".to_string(),
                note: Some("preserve call instructions; rely on LLVM's heuristic".to_string()),
            },
        ],
        default: 1, // keep_call — matches what LLVM does without a hint
        default_confidence: Confidence::Low,
        resolution_surface: ResolutionSurface {
            attributes: vec!["inline".to_string(), "inline(never)".to_string()],
        },
        cross_phase_origin: Some(Phase::Codegen),
    }
}

/// For every pub-fn name in `targets`, count how many call sites
/// appear inside loop bodies anywhere in the program. The walker
/// tracks loop nesting depth; calls at depth > 0 contribute.
fn collect_call_sites_in_loops(
    program: &Program,
    targets: &HashMap<String, PubFnInfo>,
) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for item in &program.items {
        match item {
            Item::Function(f) => {
                walk_block_for_calls(&f.body, 0, targets, &mut counts);
            }
            Item::ImplBlock(imp) => {
                for impl_item in &imp.items {
                    if let ImplItem::Method(m) = impl_item {
                        walk_block_for_calls(&m.body, 0, targets, &mut counts);
                    }
                }
            }
            _ => {}
        }
    }
    counts
}

fn walk_block_for_calls(
    block: &Block,
    loop_depth: u32,
    targets: &HashMap<String, PubFnInfo>,
    counts: &mut HashMap<String, usize>,
) {
    for stmt in &block.stmts {
        walk_stmt_for_calls(stmt, loop_depth, targets, counts);
    }
    if let Some(tail) = &block.final_expr {
        walk_expr_for_calls(tail, loop_depth, targets, counts);
    }
}

fn walk_stmt_for_calls(
    stmt: &Stmt,
    loop_depth: u32,
    targets: &HashMap<String, PubFnInfo>,
    counts: &mut HashMap<String, usize>,
) {
    match &stmt.kind {
        StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
            walk_expr_for_calls(value, loop_depth, targets, counts);
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            walk_block_for_calls(body, loop_depth, targets, counts);
        }
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            walk_expr_for_calls(target, loop_depth, targets, counts);
            walk_expr_for_calls(value, loop_depth, targets, counts);
        }
        StmtKind::Expr(e) => walk_expr_for_calls(e, loop_depth, targets, counts),
    }
}

fn walk_expr_for_calls(
    expr: &Expr,
    loop_depth: u32,
    targets: &HashMap<String, PubFnInfo>,
    counts: &mut HashMap<String, usize>,
) {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            if loop_depth > 0 {
                if let Some(name) = callee_simple_name(callee) {
                    if targets.contains_key(&name) {
                        *counts.entry(name).or_insert(0) += 1;
                    }
                }
            }
            walk_expr_for_calls(callee, loop_depth, targets, counts);
            for arg in args {
                walk_expr_for_calls(&arg.value, loop_depth, targets, counts);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            walk_expr_for_calls(object, loop_depth, targets, counts);
            for arg in args {
                walk_expr_for_calls(&arg.value, loop_depth, targets, counts);
            }
        }
        ExprKind::Binary { left, right, .. } => {
            walk_expr_for_calls(left, loop_depth, targets, counts);
            walk_expr_for_calls(right, loop_depth, targets, counts);
        }
        ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
            walk_expr_for_calls(operand, loop_depth, targets, counts);
        }
        ExprKind::NilCoalesce { left, right } => {
            walk_expr_for_calls(left, loop_depth, targets, counts);
            walk_expr_for_calls(right, loop_depth, targets, counts);
        }
        ExprKind::OptionalChain { object, args, .. } => {
            walk_expr_for_calls(object, loop_depth, targets, counts);
            if let Some(args) = args {
                for arg in args {
                    walk_expr_for_calls(&arg.value, loop_depth, targets, counts);
                }
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            walk_expr_for_calls(object, loop_depth, targets, counts);
        }
        ExprKind::Index { object, index } => {
            walk_expr_for_calls(object, loop_depth, targets, counts);
            walk_expr_for_calls(index, loop_depth, targets, counts);
        }
        ExprKind::Block(b) => walk_block_for_calls(b, loop_depth, targets, counts),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            walk_expr_for_calls(condition, loop_depth, targets, counts);
            walk_block_for_calls(then_block, loop_depth, targets, counts);
            if let Some(eb) = else_branch {
                walk_expr_for_calls(eb, loop_depth, targets, counts);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            walk_expr_for_calls(value, loop_depth, targets, counts);
            walk_block_for_calls(then_block, loop_depth, targets, counts);
            if let Some(eb) = else_branch {
                walk_expr_for_calls(eb, loop_depth, targets, counts);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_expr_for_calls(scrutinee, loop_depth, targets, counts);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    walk_expr_for_calls(g, loop_depth, targets, counts);
                }
                walk_expr_for_calls(&arm.body, loop_depth, targets, counts);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            walk_expr_for_calls(condition, loop_depth, targets, counts);
            walk_block_for_calls(body, loop_depth + 1, targets, counts);
        }
        ExprKind::WhileLet { value, body, .. } => {
            walk_expr_for_calls(value, loop_depth, targets, counts);
            walk_block_for_calls(body, loop_depth + 1, targets, counts);
        }
        ExprKind::For { iterable, body, .. } => {
            walk_expr_for_calls(iterable, loop_depth, targets, counts);
            walk_block_for_calls(body, loop_depth + 1, targets, counts);
        }
        ExprKind::Loop { body, .. } => {
            walk_block_for_calls(body, loop_depth + 1, targets, counts);
        }
        ExprKind::LabeledBlock { body, .. } => {
            walk_block_for_calls(body, loop_depth, targets, counts);
        }
        ExprKind::Closure { body, .. } => {
            walk_expr_for_calls(body, loop_depth, targets, counts);
        }
        ExprKind::Return(Some(e)) => walk_expr_for_calls(e, loop_depth, targets, counts),
        ExprKind::Break { value: Some(v), .. } => {
            walk_expr_for_calls(v, loop_depth, targets, counts)
        }
        ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
            for it in items {
                walk_expr_for_calls(it, loop_depth, targets, counts);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for it in items {
                walk_expr_for_calls(it, loop_depth, targets, counts);
            }
        }
        _ => {}
    }
}

fn callee_simple_name(callee: &Expr) -> Option<String> {
    match &callee.kind {
        ExprKind::Identifier(n) => Some(n.clone()),
        ExprKind::Path { segments, .. } if segments.len() == 1 => Some(segments[0].clone()),
        _ => None,
    }
}

fn count_block_stmts(block: &Block) -> usize {
    block.stmts.len() + usize::from(block.final_expr.is_some())
}

// ── Branch-hint walk ──────────────────────────────────────────────

fn collect_branch_hint_queries(program: &Program) -> Vec<CompilerQuery> {
    let mut queries = Vec::new();
    for item in &program.items {
        match item {
            Item::Function(f) => {
                let def_path = DefPath::item(f.name.clone());
                walk_block_for_branch_hints(&f.body, &def_path, &mut queries);
            }
            Item::ImplBlock(imp) => {
                if let Some(target) = impl_target_name(&imp.target_type) {
                    for impl_item in &imp.items {
                        if let ImplItem::Method(m) = impl_item {
                            let def_path = DefPath::new(vec![target.clone(), m.name.clone()]);
                            walk_block_for_branch_hints(&m.body, &def_path, &mut queries);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    queries.sort_by_key(|q| q.site.offset);
    queries
}

fn impl_target_name(target: &TypeExpr) -> Option<String> {
    match &target.kind {
        TypeKind::Path(path) => path.segments.last().cloned(),
        _ => None,
    }
}

fn walk_block_for_branch_hints(block: &Block, def_path: &DefPath, out: &mut Vec<CompilerQuery>) {
    for stmt in &block.stmts {
        walk_stmt_for_branch_hints(stmt, def_path, out);
    }
    if let Some(tail) = &block.final_expr {
        walk_expr_for_branch_hints(tail, def_path, out);
    }
}

fn walk_stmt_for_branch_hints(stmt: &Stmt, def_path: &DefPath, out: &mut Vec<CompilerQuery>) {
    match &stmt.kind {
        StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
            walk_expr_for_branch_hints(value, def_path, out);
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            walk_block_for_branch_hints(body, def_path, out);
        }
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            walk_expr_for_branch_hints(target, def_path, out);
            walk_expr_for_branch_hints(value, def_path, out);
        }
        StmtKind::Expr(e) => walk_expr_for_branch_hints(e, def_path, out),
    }
}

fn walk_expr_for_branch_hints(expr: &Expr, def_path: &DefPath, out: &mut Vec<CompilerQuery>) {
    match &expr.kind {
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            walk_expr_for_branch_hints(condition, def_path, out);
            walk_block_for_branch_hints(then_block, def_path, out);
            if let Some(eb) = else_branch {
                walk_expr_for_branch_hints(eb, def_path, out);
                if let Some(q) = build_if_branch_hint_query(expr, then_block, eb, def_path) {
                    out.push(q);
                }
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            walk_expr_for_branch_hints(value, def_path, out);
            walk_block_for_branch_hints(then_block, def_path, out);
            if let Some(eb) = else_branch {
                walk_expr_for_branch_hints(eb, def_path, out);
                if let Some(q) = build_if_branch_hint_query(expr, then_block, eb, def_path) {
                    out.push(q);
                }
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_expr_for_branch_hints(scrutinee, def_path, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    walk_expr_for_branch_hints(g, def_path, out);
                }
                walk_expr_for_branch_hints(&arm.body, def_path, out);
            }
            if let Some(q) = build_match_branch_hint_query(expr, arms, def_path) {
                out.push(q);
            }
        }
        ExprKind::Block(b) => walk_block_for_branch_hints(b, def_path, out),
        ExprKind::Binary { left, right, .. } => {
            walk_expr_for_branch_hints(left, def_path, out);
            walk_expr_for_branch_hints(right, def_path, out);
        }
        ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
            walk_expr_for_branch_hints(operand, def_path, out);
        }
        ExprKind::NilCoalesce { left, right } => {
            walk_expr_for_branch_hints(left, def_path, out);
            walk_expr_for_branch_hints(right, def_path, out);
        }
        ExprKind::Call { callee, args } => {
            walk_expr_for_branch_hints(callee, def_path, out);
            for arg in args {
                walk_expr_for_branch_hints(&arg.value, def_path, out);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            walk_expr_for_branch_hints(object, def_path, out);
            for arg in args {
                walk_expr_for_branch_hints(&arg.value, def_path, out);
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            walk_expr_for_branch_hints(object, def_path, out);
        }
        ExprKind::Index { object, index } => {
            walk_expr_for_branch_hints(object, def_path, out);
            walk_expr_for_branch_hints(index, def_path, out);
        }
        ExprKind::OptionalChain { object, args, .. } => {
            walk_expr_for_branch_hints(object, def_path, out);
            if let Some(args) = args {
                for arg in args {
                    walk_expr_for_branch_hints(&arg.value, def_path, out);
                }
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            walk_expr_for_branch_hints(condition, def_path, out);
            walk_block_for_branch_hints(body, def_path, out);
        }
        ExprKind::WhileLet { value, body, .. } => {
            walk_expr_for_branch_hints(value, def_path, out);
            walk_block_for_branch_hints(body, def_path, out);
        }
        ExprKind::For { iterable, body, .. } => {
            walk_expr_for_branch_hints(iterable, def_path, out);
            walk_block_for_branch_hints(body, def_path, out);
        }
        ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => {
            walk_block_for_branch_hints(body, def_path, out);
        }
        ExprKind::Closure { body, .. } => {
            walk_expr_for_branch_hints(body, def_path, out);
        }
        ExprKind::Return(Some(e)) => walk_expr_for_branch_hints(e, def_path, out),
        ExprKind::Break { value: Some(v), .. } => walk_expr_for_branch_hints(v, def_path, out),
        ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
            for it in items {
                walk_expr_for_branch_hints(it, def_path, out);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for it in items {
                walk_expr_for_branch_hints(it, def_path, out);
            }
        }
        _ => {}
    }
}

fn build_if_branch_hint_query(
    expr: &Expr,
    then_block: &Block,
    else_branch: &Expr,
    def_path: &DefPath,
) -> Option<CompilerQuery> {
    let then_count = count_block_stmts(then_block);
    let else_count = count_expr_stmts(else_branch);
    if !is_notable_skew(then_count, else_count) {
        return None;
    }
    let (heavy_label, light_label) = if then_count > else_count {
        ("then", "else")
    } else {
        ("else", "then")
    };
    Some(CompilerQuery {
        id: QueryId {
            def_path: def_path.clone(),
            sub_item_hash: SubItemHash(expr.span.offset as u64),
        },
        site: expr.span.clone(),
        kind: QueryKind::BranchHint,
        options: vec![
            QueryOption {
                label: format!("hint_{}_likely", heavy_label),
                note: Some(format!(
                    "mark `{}` branch ({} stmts) as the likely path",
                    heavy_label,
                    heavier_count(then_count, else_count),
                )),
            },
            QueryOption {
                label: format!("hint_{}_unlikely", light_label),
                note: Some(format!(
                    "mark `{}` branch ({} stmts) as the unlikely path",
                    light_label,
                    lighter_count(then_count, else_count),
                )),
            },
            QueryOption {
                label: "no_hint".to_string(),
                note: Some("leave branch layout to LLVM's heuristic".to_string()),
            },
        ],
        default: 2, // no_hint
        default_confidence: Confidence::Low,
        resolution_surface: ResolutionSurface {
            attributes: vec!["likely".to_string(), "unlikely".to_string()],
        },
        cross_phase_origin: Some(Phase::Codegen),
    })
}

fn build_match_branch_hint_query(
    expr: &Expr,
    arms: &[MatchArm],
    def_path: &DefPath,
) -> Option<CompilerQuery> {
    if arms.len() < 2 {
        return None;
    }
    let sizes: Vec<usize> = arms.iter().map(|a| count_expr_stmts(&a.body)).collect();
    let heaviest = *sizes.iter().max().unwrap_or(&0);
    let lightest = *sizes.iter().min().unwrap_or(&0);
    if !is_notable_skew(heaviest, lightest) {
        return None;
    }
    // Identify the heaviest arm — the candidate `#[likely]` site.
    let heavy_idx = sizes
        .iter()
        .enumerate()
        .max_by_key(|(_, s)| **s)
        .map(|(i, _)| i)
        .unwrap_or(0);
    Some(CompilerQuery {
        id: QueryId {
            def_path: def_path.clone(),
            sub_item_hash: SubItemHash(expr.span.offset as u64),
        },
        site: expr.span.clone(),
        kind: QueryKind::BranchHint,
        options: vec![
            QueryOption {
                label: format!("hint_arm_{}_likely", heavy_idx),
                note: Some(format!(
                    "mark arm {} ({} stmts) as the likely path",
                    heavy_idx, heaviest
                )),
            },
            QueryOption {
                label: "hint_lightest_unlikely".to_string(),
                note: Some(format!(
                    "mark the lightest arm ({} stmts) as the unlikely path",
                    lightest
                )),
            },
            QueryOption {
                label: "no_hint".to_string(),
                note: Some("leave arm layout to LLVM's heuristic".to_string()),
            },
        ],
        default: 2, // no_hint
        default_confidence: Confidence::Low,
        resolution_surface: ResolutionSurface {
            attributes: vec!["likely".to_string(), "unlikely".to_string()],
        },
        cross_phase_origin: Some(Phase::Codegen),
    })
}

fn is_notable_skew(a: usize, b: usize) -> bool {
    let heavy = a.max(b);
    let light = a.min(b);
    if heavy < BRANCH_HINT_MIN_HEAVY_STMTS {
        return false;
    }
    if light == 0 {
        return true;
    }
    heavy >= light * BRANCH_HINT_RATIO
}

fn heavier_count(then_count: usize, else_count: usize) -> usize {
    then_count.max(else_count)
}

fn lighter_count(then_count: usize, else_count: usize) -> usize {
    then_count.min(else_count)
}

fn count_expr_stmts(expr: &Expr) -> usize {
    match &expr.kind {
        ExprKind::Block(b) => count_block_stmts(b),
        ExprKind::If { .. }
        | ExprKind::IfLet { .. }
        | ExprKind::Match { .. }
        | ExprKind::While { .. }
        | ExprKind::WhileLet { .. }
        | ExprKind::For { .. }
        | ExprKind::Loop { .. } => 3,
        _ => 1,
    }
}
