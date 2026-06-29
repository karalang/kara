//! FE-3 — `#[gpu]` call-graph validation (recursion rejection).
//!
//! GPU kernels run with no call stack, so **recursion is forbidden** in the
//! transitive call graph rooted at any `#[gpu]` function (design.md § GPU
//! Subset Constraints — "Recursion" is in the *Not Allowed* column). This
//! pass builds a *precise* call graph and, from each `#[gpu]` root, reports
//! the first reachable cycle with the full call chain from the root.
//!
//! **Precision matters here because the diagnostic is a hard reject** — a
//! false-positive cycle would wrongly reject valid GPU code. So unlike the
//! conservative `crate::call_graph` (which links a `MethodCall` to *every*
//! same-named method for the affected-by query), this pass adds an edge
//! only when the callee resolves *exactly* to a known function node:
//!
//! - **Direct calls** (`f(...)`, `Type.assoc(...)`) — keyed by the callee
//!   path; free-function names are unique in the single-program v1 scope.
//! - **Method calls** (`obj.m(...)`) — resolved through the typechecker's
//!   own `method_callee_types` side-table (`"Type.method"`), populated
//!   during inference. Unresolved / builtin / indirect callees add no edge
//!   (a false *negative* — safe: it under-rejects, never over-rejects).
//!
//! Scope of this slice: **recursion only**. The sibling `#[gpu]` call-graph
//! checks — a call to a *generic* function lacking `#[gpu]` (FE-3b) and
//! host-capturing closures (FE-3c) — are tracked follow-ups. `dyn Trait` is
//! already globally rejected at type lowering (`E_DYN_TRAIT_NOT_IMPLEMENTED_YET`),
//! so no `#[gpu]`-specific dyn check is needed.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;
use crate::typechecker::TypeErrorKind;

/// One call-graph node reachable for the recursion analysis.
struct GpuNode {
    /// `fn`-keyword span, for anchoring the root's diagnostic.
    span: Span,
    is_gpu: bool,
    /// Precise callee keys (free-fn names / `Type.method`).
    callees: Vec<String>,
}

impl<'a> super::TypeChecker<'a> {
    /// FE-3 entry point — invoked from `check()` after inference, so the
    /// `method_callee_types` side-table is fully populated. Walks the
    /// precise call graph from every `#[gpu]` root and emits an `E0801`
    /// `GpuNotSafe` diagnostic naming the cycle chain for each root that
    /// can reach recursion.
    pub(super) fn check_gpu_call_graph(&mut self) {
        let program = self.program; // `&'a Program` — Copy, detaches the borrow
        let method_callee = &self.method_callee_types;

        // 1. Collect every user function node (free fn, impl method, trait
        //    default-body method) keyed identically to `method_callee_types`
        //    (`"Type.method"`) so resolved method edges join cleanly.
        let mut nodes: HashMap<String, (Span, bool, &Block)> = HashMap::new();
        for item in &program.items {
            match item {
                Item::Function(f) => {
                    nodes.insert(f.name.clone(), (f.span.clone(), f.is_gpu, &f.body));
                }
                Item::ImplBlock(b) => {
                    let recv = render_target_base(&b.target_type);
                    for inner in &b.items {
                        if let ImplItem::Method(m) = inner {
                            let key = format!("{recv}.{}", m.name);
                            nodes.insert(key, (m.span.clone(), m.is_gpu, &m.body));
                        }
                    }
                }
                Item::TraitDef(t) => {
                    for inner in &t.items {
                        if let TraitItem::Method(m) = inner {
                            if let Some(body) = &m.body {
                                let key = format!("{}.{}", t.name, m.name);
                                nodes.insert(key, (m.span.clone(), m.is_gpu, body));
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        let known: HashSet<String> = nodes.keys().cloned().collect();

        // 2. Build the precise forward graph.
        let graph: HashMap<String, GpuNode> = nodes
            .iter()
            .map(|(key, (span, is_gpu, body))| {
                let mut callees: Vec<String> = Vec::new();
                collect_edges_block(body, &known, method_callee, &mut callees);
                callees.sort();
                callees.dedup();
                (
                    key.clone(),
                    GpuNode {
                        span: span.clone(),
                        is_gpu: *is_gpu,
                        callees,
                    },
                )
            })
            .collect();

        // 3. From each `#[gpu]` root, find the first reachable cycle.
        let mut roots: Vec<&String> = graph
            .iter()
            .filter(|(_, n)| n.is_gpu)
            .map(|(k, _)| k)
            .collect();
        roots.sort(); // deterministic diagnostic order

        let mut violations: Vec<(Span, String)> = Vec::new();
        for root in roots {
            let mut path: Vec<String> = Vec::new();
            let mut on_path: HashSet<String> = HashSet::new();
            let mut done: HashSet<String> = HashSet::new();
            if let Some(chain) = find_cycle(root, &graph, &mut path, &mut on_path, &mut done) {
                let root_span = graph[root].span.clone();
                let message = format!(
                    "recursion is not allowed in a `#[gpu]` call graph: `{}` reaches a cycle \
                     `{}`. GPU kernels run with no call stack, so a `#[gpu]` function and \
                     everything it transitively calls must be non-recursive — restructure the \
                     algorithm to an iterative form (a bounded `for`/`while` loop). See \
                     design.md § GPU Subset Constraints.",
                    root,
                    chain.join(" → "),
                );
                violations.push((root_span, message));
            }
        }

        for (span, message) in violations {
            self.type_error(message, span, TypeErrorKind::GpuNotSafe);
        }
    }
}

/// DFS for the first cycle reachable from `node`. `path`/`on_path` track the
/// current gray frontier (a back-edge into it is a cycle); `done` is the
/// black set so a fully-explored subtree is not re-walked. Returns the chain
/// from `node`'s subtree to the repeated node (the last element repeats an
/// earlier one), or `None` if no cycle is reachable.
fn find_cycle(
    node: &str,
    graph: &HashMap<String, GpuNode>,
    path: &mut Vec<String>,
    on_path: &mut HashSet<String>,
    done: &mut HashSet<String>,
) -> Option<Vec<String>> {
    path.push(node.to_string());
    on_path.insert(node.to_string());

    if let Some(n) = graph.get(node) {
        for callee in &n.callees {
            if on_path.contains(callee) {
                // Back-edge → cycle. Report the path so far plus the repeat.
                let mut chain = path.clone();
                chain.push(callee.clone());
                return Some(chain);
            }
            if !done.contains(callee) {
                if let Some(found) = find_cycle(callee, graph, path, on_path, done) {
                    return Some(found);
                }
            }
        }
    }

    on_path.remove(node);
    path.pop();
    done.insert(node.to_string());
    None
}

/// Bare base-type name of an impl target (`Point`, `Vec` — not `Vec[i64]`),
/// matching the key scheme `method_callee_types` and `crate::call_graph` use.
fn render_target_base(ty: &TypeExpr) -> String {
    match &ty.kind {
        TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
        _ => crate::formatter::render_type_expr(ty),
    }
}

/// Resolve a `Call` callee expression to a graph key (`name` or
/// `Type.method`), or `None` for an indirect/closure-valued callee.
fn callee_key(callee: &Expr) -> Option<String> {
    match &callee.kind {
        ExprKind::Identifier(name) => Some(name.clone()),
        ExprKind::FieldAccess { object, field } => match &object.kind {
            ExprKind::Identifier(root) => Some(format!("{root}.{field}")),
            _ => None,
        },
        ExprKind::Path { segments, .. } => match segments.len() {
            2 => Some(format!("{}.{}", segments[0], segments[1])),
            1 => Some(segments[0].clone()),
            _ => None,
        },
        _ => None,
    }
}

// ── Precise edge collection (AST walk) ───────────────────────────
//
// Mirrors `crate::call_graph`'s body walk for completeness, but adds an
// edge only when the callee resolves exactly to a `known` node — direct
// calls by key, method calls via the precise `method_callee` side-table.

fn collect_edges_block(
    block: &Block,
    known: &HashSet<String>,
    method_callee: &HashMap<SpanKey, String>,
    out: &mut Vec<String>,
) {
    for stmt in &block.stmts {
        collect_edges_stmt(stmt, known, method_callee, out);
    }
    if let Some(e) = &block.final_expr {
        collect_edges_expr(e, known, method_callee, out);
    }
}

fn collect_edges_stmt(
    stmt: &Stmt,
    known: &HashSet<String>,
    method_callee: &HashMap<SpanKey, String>,
    out: &mut Vec<String>,
) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => {}
        StmtKind::Let { value, .. } => collect_edges_expr(value, known, method_callee, out),
        StmtKind::LetUninit { .. } => {}
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            collect_edges_expr(value, known, method_callee, out);
            collect_edges_block(else_block, known, method_callee, out);
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            collect_edges_block(body, known, method_callee, out);
        }
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            collect_edges_expr(target, known, method_callee, out);
            collect_edges_expr(value, known, method_callee, out);
        }
        StmtKind::Expr(e) => collect_edges_expr(e, known, method_callee, out),
    }
}

fn collect_edges_expr(
    expr: &Expr,
    known: &HashSet<String>,
    method_callee: &HashMap<SpanKey, String>,
    out: &mut Vec<String>,
) {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            if let Some(key) = callee_key(callee) {
                if known.contains(&key) {
                    out.push(key);
                }
            }
            collect_edges_expr(callee, known, method_callee, out);
            for a in args {
                collect_edges_expr(&a.value, known, method_callee, out);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            if let Some(key) = method_callee.get(&SpanKey::from_span(&expr.span)) {
                if known.contains(key) {
                    out.push(key.clone());
                }
            }
            collect_edges_expr(object, known, method_callee, out);
            for a in args {
                collect_edges_expr(&a.value, known, method_callee, out);
            }
        }
        ExprKind::OptionalChain {
            object,
            args: Some(args),
            ..
        } => {
            if let Some(key) = method_callee.get(&SpanKey::from_span(&expr.span)) {
                if known.contains(key) {
                    out.push(key.clone());
                }
            }
            collect_edges_expr(object, known, method_callee, out);
            for a in args {
                collect_edges_expr(&a.value, known, method_callee, out);
            }
        }
        ExprKind::OptionalChain { object, .. }
        | ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. } => {
            collect_edges_expr(object, known, method_callee, out);
        }
        ExprKind::Index { object, index } => {
            collect_edges_expr(object, known, method_callee, out);
            collect_edges_expr(index, known, method_callee, out);
        }
        ExprKind::NilCoalesce { left, right }
        | ExprKind::Binary { left, right, .. }
        | ExprKind::Pipe { left, right } => {
            collect_edges_expr(left, known, method_callee, out);
            collect_edges_expr(right, known, method_callee, out);
        }
        ExprKind::Unary { operand, .. } => collect_edges_expr(operand, known, method_callee, out),
        ExprKind::Question(inner) => collect_edges_expr(inner, known, method_callee, out),
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b)
        | ExprKind::Unsafe(b) => collect_edges_block(b, known, method_callee, out),
        ExprKind::Lock { body, .. } => collect_edges_block(body, known, method_callee, out),
        ExprKind::Providers { bindings, body } => {
            for binding in bindings {
                collect_edges_expr(&binding.value, known, method_callee, out);
            }
            collect_edges_block(body, known, method_callee, out);
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_edges_expr(condition, known, method_callee, out);
            collect_edges_block(then_block, known, method_callee, out);
            if let Some(e) = else_branch {
                collect_edges_expr(e, known, method_callee, out);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            collect_edges_expr(value, known, method_callee, out);
            collect_edges_block(then_block, known, method_callee, out);
            if let Some(e) = else_branch {
                collect_edges_expr(e, known, method_callee, out);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_edges_expr(scrutinee, known, method_callee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_edges_expr(g, known, method_callee, out);
                }
                collect_edges_expr(&arm.body, known, method_callee, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_edges_expr(condition, known, method_callee, out);
            collect_edges_block(body, known, method_callee, out);
        }
        ExprKind::WhileLet { value, body, .. } => {
            collect_edges_expr(value, known, method_callee, out);
            collect_edges_block(body, known, method_callee, out);
        }
        ExprKind::For { iterable, body, .. } => {
            collect_edges_expr(iterable, known, method_callee, out);
            collect_edges_block(body, known, method_callee, out);
        }
        ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => {
            collect_edges_block(body, known, method_callee, out);
        }
        ExprKind::Closure { body, .. } => collect_edges_expr(body, known, method_callee, out),
        ExprKind::Cast { expr, .. } => collect_edges_expr(expr, known, method_callee, out),
        ExprKind::Return(Some(e)) => collect_edges_expr(e, known, method_callee, out),
        ExprKind::Break { value: Some(e), .. } => collect_edges_expr(e, known, method_callee, out),
        ExprKind::Tuple(items)
        | ExprKind::ArrayLiteral(items)
        | ExprKind::PrefixCollectionLiteral { items, .. } => {
            for it in items {
                collect_edges_expr(it, known, method_callee, out);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            collect_edges_expr(value, known, method_callee, out);
            collect_edges_expr(count, known, method_callee, out);
        }
        ExprKind::MapLiteral(entries) => {
            for (k, v) in entries {
                collect_edges_expr(k, known, method_callee, out);
                collect_edges_expr(v, known, method_callee, out);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for field in fields {
                collect_edges_expr(&field.value, known, method_callee, out);
            }
            if let Some(s) = spread {
                collect_edges_expr(s, known, method_callee, out);
            }
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                collect_edges_expr(s, known, method_callee, out);
            }
            if let Some(e) = end {
                collect_edges_expr(e, known, method_callee, out);
            }
        }
        ExprKind::InterpolatedStringLit(parts) => {
            for part in parts {
                if let ParsedInterpolationPart::Expr(e) = part {
                    collect_edges_expr(e, known, method_callee, out);
                }
            }
        }
        // Leaves — no sub-expressions to walk.
        ExprKind::Identifier(_)
        | ExprKind::Path { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::Integer(_, _)
        | ExprKind::Float(_, _)
        | ExprKind::Bool(_)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
        | ExprKind::Continue { .. }
        | ExprKind::Return(None)
        | ExprKind::Break { value: None, .. }
        | ExprKind::OffsetOf { .. }
        | ExprKind::PipePlaceholder
        | ExprKind::Error => {}
    }
}
