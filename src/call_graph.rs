//! `karac query affected-by` — call-graph reach query.
//!
//! Plain-data call-graph extractor over a parsed [`Program`]. Walks
//! every user-defined function body (free fn, impl method, trait
//! method with a default body) and collects callee references —
//! direct `f()` calls, `T.m()` associated-fn calls, and `obj.m()`
//! method calls.
//!
//! Method resolution is conservative for the cases the v1 query has
//! to handle without re-running the typechecker: a `MethodCall` with
//! method name `m` links to *every* known node whose key ends in
//! `.m`. False positives (impls the runtime never reaches) are
//! acceptable for affected-by; false negatives would break the
//! test-selection use case the query is a prerequisite for.
//!
//! Phase-5-diagnostics line 653 / `docs/deferred.md § karac query
//! affected-by — Call-Graph Reach Query`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::ast::*;
use crate::token::Span;

/// One function node in the call graph.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    /// Canonical key — `name` for a free fn, `Type.method` for an
    /// impl method or trait method. Matches the shape used in
    /// `EffectCheckResult.inferred_effects` / `declared_effects` so
    /// effect-query consumers can join on this field.
    pub key: String,
    /// Source file the node was defined in (caller-supplied at build
    /// time — single-file mode passes the file path verbatim).
    pub file: String,
    /// 1-indexed source line of the `fn` keyword.
    pub line: usize,
    /// 1-indexed source column of the `fn` keyword.
    pub col: usize,
    /// Byte offset of the function definition span.
    pub offset: usize,
    /// Byte length of the function definition span.
    pub length: usize,
    /// True when this node is a discovered test function — name
    /// starts with `test_` AND the source file is a `_test.kara`
    /// file (the discovery rule [`crate::cli`] already uses).
    pub is_test: bool,
}

/// Plain-data call graph — nodes plus forward and reverse edge maps.
/// Sorted maps so query output is deterministic.
#[derive(Debug, Clone, Default)]
pub struct CallGraph {
    pub nodes: BTreeMap<String, NodeInfo>,
    /// caller key → set of callee keys.
    pub forward: BTreeMap<String, BTreeSet<String>>,
    /// callee key → set of caller keys.
    pub reverse: BTreeMap<String, BTreeSet<String>>,
}

/// Build the call graph for `program`. `filename` is recorded on
/// every node's `file` field; `is_test_file` controls whether
/// `test_*`-prefixed nodes carry `is_test = true`.
pub fn build(program: &Program, filename: &str, is_test_file: bool) -> CallGraph {
    let mut nodes: BTreeMap<String, NodeInfo> = BTreeMap::new();
    let mut bodies: Vec<(String, &Block)> = Vec::new();

    for item in &program.items {
        match item {
            Item::Function(f) => {
                let key = f.name.clone();
                nodes.insert(
                    key.clone(),
                    make_node(&key, &f.span, filename, is_test_file_test(f, is_test_file)),
                );
                bodies.push((key, &f.body));
            }
            Item::ImplBlock(b) => {
                let receiver = render_target_type(&b.target_type);
                for inner in &b.items {
                    if let ImplItem::Method(m) = inner {
                        let key = format!("{receiver}.{}", m.name);
                        nodes.insert(
                            key.clone(),
                            make_node(&key, &m.span, filename, is_test_file_test(m, is_test_file)),
                        );
                        bodies.push((key, &m.body));
                    }
                }
            }
            Item::TraitDef(t) => {
                for inner in &t.items {
                    if let TraitItem::Method(m) = inner {
                        if let Some(body) = &m.body {
                            let key = format!("{}.{}", t.name, m.name);
                            nodes.insert(key.clone(), make_node(&key, &m.span, filename, false));
                            bodies.push((key, body));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Precompute a method-name → set-of-keys index so MethodCall
    // resolution is conservative-by-design (every impl/trait method
    // with that name is treated as a possible callee per the design
    // doc's trait-object dispatch rule).
    let mut method_index: HashMap<String, BTreeSet<String>> = HashMap::new();
    for key in nodes.keys() {
        if let Some((_, m)) = key.split_once('.') {
            method_index
                .entry(m.to_string())
                .or_default()
                .insert(key.clone());
        }
    }
    let known_keys: HashSet<String> = nodes.keys().cloned().collect();

    let mut forward: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut reverse: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (key, body) in bodies {
        let mut callees: BTreeSet<String> = BTreeSet::new();
        collect_callees_in_block(body, &known_keys, &method_index, &mut callees);
        for callee in &callees {
            reverse
                .entry(callee.clone())
                .or_default()
                .insert(key.clone());
        }
        forward.insert(key, callees);
    }

    CallGraph {
        nodes,
        forward,
        reverse,
    }
}

fn is_test_file_test(f: &Function, is_test_file: bool) -> bool {
    is_test_file && f.name.starts_with("test_")
}

fn make_node(key: &str, span: &Span, filename: &str, is_test: bool) -> NodeInfo {
    NodeInfo {
        key: key.to_string(),
        file: filename.to_string(),
        line: span.line,
        col: span.column,
        offset: span.offset,
        length: span.length,
        is_test,
    }
}

/// Key an impl method by the receiver's BARE base-type name (`Point`,
/// `Vec`, `Box` — *not* `Vec[i64]` / `Box[T]`).
///
/// This MUST match how `EffectCheckResult` (`src/effectchecker.rs`) and
/// `ConcurrencyAnalysis` (`src/concurrency.rs`) key their per-function
/// maps — both use `p.segments.last()`, the bare base name. The node
/// `key` is the join column those consumers index by (see [`NodeInfo`]),
/// so a divergent key here silently breaks the join: a generic-receiver
/// method (`impl[T] Box[T] { fn f }`) keyed `Box[T].f` would never match
/// the analyses' `Box.f`, making the whole-program `query effects` report
/// it with no effects and the whole-program `query concurrency` drop it
/// entirely. Rendering the full generic form here was exactly that bug.
///
/// Non-`Path` impl targets (rare) fall back to the rendered form.
fn render_target_type(ty: &TypeExpr) -> String {
    match &ty.kind {
        TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
        _ => crate::formatter::render_type_expr(ty),
    }
}

// ── Call collection (AST walk) ──────────────────────────────────

fn collect_callees_in_block(
    block: &Block,
    known: &HashSet<String>,
    methods: &HashMap<String, BTreeSet<String>>,
    out: &mut BTreeSet<String>,
) {
    for stmt in &block.stmts {
        collect_callees_in_stmt(stmt, known, methods, out);
    }
    if let Some(e) = &block.final_expr {
        collect_callees_in_expr(e, known, methods, out);
    }
}

fn collect_callees_in_stmt(
    stmt: &Stmt,
    known: &HashSet<String>,
    methods: &HashMap<String, BTreeSet<String>>,
    out: &mut BTreeSet<String>,
) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { value, .. } => collect_callees_in_expr(value, known, methods, out),
        StmtKind::LetUninit { .. } => {}
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            collect_callees_in_expr(value, known, methods, out);
            collect_callees_in_block(else_block, known, methods, out);
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            collect_callees_in_block(body, known, methods, out);
        }
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            collect_callees_in_expr(target, known, methods, out);
            collect_callees_in_expr(value, known, methods, out);
        }
        StmtKind::Expr(e) => collect_callees_in_expr(e, known, methods, out),
    }
}

fn collect_callees_in_expr(
    expr: &Expr,
    known: &HashSet<String>,
    methods: &HashMap<String, BTreeSet<String>>,
    out: &mut BTreeSet<String>,
) {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            if let Some(key) = callee_key(callee) {
                if known.contains(&key) {
                    out.insert(key);
                }
            }
            collect_callees_in_expr(callee, known, methods, out);
            for a in args {
                collect_callees_in_expr(&a.value, known, methods, out);
            }
        }
        ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } => {
            // Conservative: every node whose key ends in `.method` is
            // a possible callee. Per design.md affected-by spec, false
            // positives on trait-object dispatch are acceptable; false
            // negatives would break test selection.
            if let Some(keys) = methods.get(method) {
                for k in keys {
                    out.insert(k.clone());
                }
            }
            collect_callees_in_expr(object, known, methods, out);
            for a in args {
                collect_callees_in_expr(&a.value, known, methods, out);
            }
        }
        ExprKind::OptionalChain {
            object,
            args: Some(args),
            field_or_method,
            ..
        } => {
            if let Some(keys) = methods.get(field_or_method) {
                for k in keys {
                    out.insert(k.clone());
                }
            }
            collect_callees_in_expr(object, known, methods, out);
            for a in args {
                collect_callees_in_expr(&a.value, known, methods, out);
            }
        }
        ExprKind::OptionalChain { object, .. } | ExprKind::FieldAccess { object, .. } => {
            collect_callees_in_expr(object, known, methods, out);
        }
        ExprKind::TupleIndex { object, .. } => {
            collect_callees_in_expr(object, known, methods, out);
        }
        ExprKind::Index { object, index } => {
            collect_callees_in_expr(object, known, methods, out);
            collect_callees_in_expr(index, known, methods, out);
        }
        ExprKind::NilCoalesce { left, right }
        | ExprKind::Binary { left, right, .. }
        | ExprKind::Pipe { left, right } => {
            collect_callees_in_expr(left, known, methods, out);
            collect_callees_in_expr(right, known, methods, out);
        }
        ExprKind::Unary { operand, .. } => {
            collect_callees_in_expr(operand, known, methods, out);
        }
        ExprKind::Question(inner) => collect_callees_in_expr(inner, known, methods, out),
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b) => {
            collect_callees_in_block(b, known, methods, out);
        }
        ExprKind::Unsafe(b) => collect_callees_in_block(b, known, methods, out),
        ExprKind::Lock { body, .. } => collect_callees_in_block(body, known, methods, out),
        ExprKind::Providers { bindings, body } => {
            for binding in bindings {
                collect_callees_in_expr(&binding.value, known, methods, out);
            }
            collect_callees_in_block(body, known, methods, out);
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_callees_in_expr(condition, known, methods, out);
            collect_callees_in_block(then_block, known, methods, out);
            if let Some(e) = else_branch {
                collect_callees_in_expr(e, known, methods, out);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            collect_callees_in_expr(value, known, methods, out);
            collect_callees_in_block(then_block, known, methods, out);
            if let Some(e) = else_branch {
                collect_callees_in_expr(e, known, methods, out);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_callees_in_expr(scrutinee, known, methods, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_callees_in_expr(g, known, methods, out);
                }
                collect_callees_in_expr(&arm.body, known, methods, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_callees_in_expr(condition, known, methods, out);
            collect_callees_in_block(body, known, methods, out);
        }
        ExprKind::WhileLet { value, body, .. } => {
            collect_callees_in_expr(value, known, methods, out);
            collect_callees_in_block(body, known, methods, out);
        }
        ExprKind::For { iterable, body, .. } => {
            collect_callees_in_expr(iterable, known, methods, out);
            collect_callees_in_block(body, known, methods, out);
        }
        ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => {
            collect_callees_in_block(body, known, methods, out);
        }
        ExprKind::Closure { body, .. } => {
            collect_callees_in_expr(body, known, methods, out);
        }
        ExprKind::Cast { expr, .. } => collect_callees_in_expr(expr, known, methods, out),
        ExprKind::Return(Some(e)) => collect_callees_in_expr(e, known, methods, out),
        ExprKind::Break { value: Some(e), .. } => collect_callees_in_expr(e, known, methods, out),
        ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
            for it in items {
                collect_callees_in_expr(it, known, methods, out);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for it in items {
                collect_callees_in_expr(it, known, methods, out);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            collect_callees_in_expr(value, known, methods, out);
            collect_callees_in_expr(count, known, methods, out);
        }
        ExprKind::MapLiteral(entries) => {
            for (k, v) in entries {
                collect_callees_in_expr(k, known, methods, out);
                collect_callees_in_expr(v, known, methods, out);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for field in fields {
                collect_callees_in_expr(&field.value, known, methods, out);
            }
            if let Some(s) = spread {
                collect_callees_in_expr(s, known, methods, out);
            }
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                collect_callees_in_expr(s, known, methods, out);
            }
            if let Some(e) = end {
                collect_callees_in_expr(e, known, methods, out);
            }
        }
        ExprKind::InterpolatedStringLit(parts) => {
            for part in parts {
                if let ParsedInterpolationPart::Expr(e) = part {
                    collect_callees_in_expr(e, known, methods, out);
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

/// Resolve the callee expression of a `Call` into a graph-key string.
/// Returns `Some("name")` for a bare identifier callee and
/// `Some("Type.method")` for a `Type.method`-shaped path. Returns
/// `None` for indirect callees (closure variables, expression-form
/// callees) — these don't have a stable graph identity at the
/// resolver layer the AST walk has access to.
fn callee_key(callee: &Expr) -> Option<String> {
    match &callee.kind {
        ExprKind::Identifier(name) => Some(name.clone()),
        ExprKind::FieldAccess { object, field } => {
            if let ExprKind::Identifier(root) = &object.kind {
                Some(format!("{root}.{field}"))
            } else {
                None
            }
        }
        ExprKind::Path { segments, .. } => {
            if segments.len() == 2 {
                Some(format!("{}.{}", segments[0], segments[1]))
            } else if segments.len() == 1 {
                Some(segments[0].clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

// ── Query traversals ────────────────────────────────────────────

impl CallGraph {
    /// All functions transitively reachable as callees from `start`.
    /// SCC-safe — visits each node at most once. Caller is included
    /// only if reachable through a cycle (not in the seed set).
    pub fn transitive_callees(&self, start: &str) -> Vec<&NodeInfo> {
        self.bfs_reach(start, &self.forward)
    }

    /// All functions that transitively call `start`. Same shape as
    /// [`transitive_callees`] but walking [`Self::reverse`].
    pub fn transitive_callers(&self, start: &str) -> Vec<&NodeInfo> {
        self.bfs_reach(start, &self.reverse)
    }

    /// Subset of [`transitive_callers`] restricted to nodes whose
    /// `is_test` flag is set. Surfaced as a separate view because the
    /// downstream test-selection consumer (`karac tdd` `--related` /
    /// `--since`) wants this filter directly without doing a second
    /// pass over the callers array.
    pub fn reaching_tests(&self, start: &str) -> Vec<&NodeInfo> {
        self.transitive_callers(start)
            .into_iter()
            .filter(|n| n.is_test)
            .collect()
    }

    fn bfs_reach(&self, start: &str, edges: &BTreeMap<String, BTreeSet<String>>) -> Vec<&NodeInfo> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut queue: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        if let Some(neighbors) = edges.get(start) {
            for n in neighbors {
                if seen.insert(n.clone()) {
                    queue.push_back(n.clone());
                }
            }
        }
        while let Some(cur) = queue.pop_front() {
            if let Some(neighbors) = edges.get(&cur) {
                for n in neighbors {
                    if seen.insert(n.clone()) {
                        queue.push_back(n.clone());
                    }
                }
            }
        }
        let mut out: Vec<&NodeInfo> = seen.iter().filter_map(|k| self.nodes.get(k)).collect();
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out
    }

    /// Resolve a [`TargetSpec`] into the set of node keys it covers.
    /// `File` matches every node defined in the file; `FileRange`
    /// further filters to nodes whose source line falls inside the
    /// inclusive range; `Function` matches a node by its exact key.
    pub fn resolve_target(&self, target: &TargetSpec) -> Vec<String> {
        match target {
            TargetSpec::Function(name) => {
                if self.nodes.contains_key(name) {
                    vec![name.clone()]
                } else {
                    Vec::new()
                }
            }
            TargetSpec::File(file) => self
                .nodes
                .values()
                .filter(|n| n.file == *file)
                .map(|n| n.key.clone())
                .collect(),
            TargetSpec::FileRange(file, lo, hi) => self
                .nodes
                .values()
                .filter(|n| n.file == *file && n.line >= *lo && n.line <= *hi)
                .map(|n| n.key.clone())
                .collect(),
        }
    }
}

/// What the `karac query affected-by` user typed on the command line.
/// The CLI parser resolves the string surface (`file.kara:42-58`,
/// `math::sort`, etc.) into one of these enum cases before handing
/// off to [`CallGraph::resolve_target`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetSpec {
    /// Bare file path — `src/sort.kara`.
    File(String),
    /// File + inclusive 1-indexed line range — `src/sort.kara:42-58`
    /// or `src/sort.kara:42` (single line, lo == hi).
    FileRange(String, usize, usize),
    /// Fully-qualified function path — `math::sort`, `Point.new`,
    /// `sort`. Multi-segment forms with `::` are rewritten to `.`-
    /// joined keys at parse time so the resolver sees the canonical
    /// shape.
    Function(String),
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Program {
        crate::parse(source).program
    }

    #[test]
    fn builds_nodes_for_free_fns_and_impl_methods() {
        let p = parse(
            "fn a() { }\nstruct Point { x: i64 }\nimpl Point { fn new() -> Point { Point { x: 0 } } }\n",
        );
        let g = build(&p, "m.kara", false);
        assert!(g.nodes.contains_key("a"));
        assert!(g.nodes.contains_key("Point.new"));
    }

    #[test]
    fn forward_edges_track_direct_calls() {
        let p = parse("fn helper() -> i64 { 0 }\nfn caller() -> i64 { helper() }\n");
        let g = build(&p, "m.kara", false);
        assert!(g.forward.get("caller").unwrap().contains("helper"));
        assert!(g.reverse.get("helper").unwrap().contains("caller"));
    }

    #[test]
    fn transitive_callees_walk_multi_hop() {
        let p = parse("fn c() -> i64 { 0 }\nfn b() -> i64 { c() }\nfn a() -> i64 { b() }\n");
        let g = build(&p, "m.kara", false);
        let callees: Vec<_> = g
            .transitive_callees("a")
            .into_iter()
            .map(|n| n.key.clone())
            .collect();
        assert_eq!(callees, vec!["b".to_string(), "c".to_string()]);
    }

    #[test]
    fn transitive_callers_walk_multi_hop() {
        let p = parse("fn c() -> i64 { 0 }\nfn b() -> i64 { c() }\nfn a() -> i64 { b() }\n");
        let g = build(&p, "m.kara", false);
        let callers: Vec<_> = g
            .transitive_callers("c")
            .into_iter()
            .map(|n| n.key.clone())
            .collect();
        assert_eq!(callers, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn recursion_does_not_infinite_loop() {
        let p = parse("fn loop_a() -> i64 { loop_b() }\nfn loop_b() -> i64 { loop_a() }\n");
        let g = build(&p, "m.kara", false);
        let callees: Vec<_> = g
            .transitive_callees("loop_a")
            .into_iter()
            .map(|n| n.key.clone())
            .collect();
        assert!(callees.contains(&"loop_a".to_string()));
        assert!(callees.contains(&"loop_b".to_string()));
    }

    #[test]
    fn method_call_links_to_every_impl_with_matching_name() {
        let p = parse(
            "struct A { x: i64 }\nstruct B { x: i64 }\nimpl A { fn do_thing(ref self) -> i64 { 1 } }\nimpl B { fn do_thing(ref self) -> i64 { 2 } }\nfn driver(a: ref A) -> i64 { a.do_thing() }\n",
        );
        let g = build(&p, "m.kara", false);
        let callees = g.forward.get("driver").unwrap();
        assert!(callees.contains("A.do_thing"));
        assert!(callees.contains("B.do_thing"));
    }

    #[test]
    fn type_dot_assoc_fn_call_resolves() {
        let p = parse(
            "struct Point { x: f64 }\nimpl Point { fn make() -> Point { Point { x: 0.0 } } }\nfn driver() -> Point { Point.make() }\n",
        );
        let g = build(&p, "m.kara", false);
        assert!(g.forward.get("driver").unwrap().contains("Point.make"));
    }

    #[test]
    fn reaching_tests_filters_by_is_test_flag() {
        let p = parse(
            "fn helper() -> i64 { 0 }\nfn test_helper_returns_zero() { let _ = helper(); }\nfn non_test_caller() -> i64 { helper() }\n",
        );
        let g = build(&p, "m.kara", true);
        let tests: Vec<_> = g
            .reaching_tests("helper")
            .into_iter()
            .map(|n| n.key.clone())
            .collect();
        assert_eq!(tests, vec!["test_helper_returns_zero".to_string()]);
    }

    #[test]
    fn reaching_tests_empty_when_file_is_not_test_file() {
        let p = parse("fn helper() -> i64 { 0 }\nfn test_helper() { let _ = helper(); }\n");
        let g = build(&p, "m.kara", false);
        assert!(g.reaching_tests("helper").is_empty());
    }

    #[test]
    fn resolve_target_function_returns_exact_key() {
        let p = parse("fn alpha() {}\nfn beta() {}\n");
        let g = build(&p, "m.kara", false);
        assert_eq!(
            g.resolve_target(&TargetSpec::Function("alpha".to_string())),
            vec!["alpha".to_string()]
        );
        assert!(g
            .resolve_target(&TargetSpec::Function("missing".to_string()))
            .is_empty());
    }

    #[test]
    fn resolve_target_file_returns_all_nodes_in_file() {
        let p = parse("fn alpha() {}\nfn beta() {}\n");
        let g = build(&p, "m.kara", false);
        let mut keys = g.resolve_target(&TargetSpec::File("m.kara".to_string()));
        keys.sort();
        assert_eq!(keys, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn resolve_target_file_range_filters_by_line() {
        let p = parse("fn alpha() {}\nfn beta() {}\nfn gamma() {}\n");
        let g = build(&p, "m.kara", false);
        let keys = g.resolve_target(&TargetSpec::FileRange("m.kara".to_string(), 2, 2));
        assert_eq!(keys, vec!["beta".to_string()]);
    }
}
