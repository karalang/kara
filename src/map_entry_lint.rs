// src/map_entry_lint.rs
//! `map_value_clone_reinsert` — the quadratic map-of-containers idiom.
//!
//! B-2026-08-03-9. Extending a container held as a map VALUE has an O(1)
//! spelling in Kāra, `Map.entry(k).or_insert(default)`, which hands back a
//! `mut ref V` into the map's own slot so the append lands through the borrow.
//! It also has a natural-looking O(k) one:
//!
//! ```text
//! match index.get(w) {
//!     Some(existing) => {
//!         let mut hits: Vec[i64] = existing.clone();   // <-- copies the whole list
//!         hits.push(i);
//!         let _ = index.insert(w, hits);
//!     }
//!     None => {
//!         let mut hits: Vec[i64] = Vec.new();
//!         hits.push(i);
//!         let _ = index.insert(w, hits);
//!     }
//! }
//! ```
//!
//! Appending the k-th occurrence of a key copies k-1 elements, so building an
//! index for a key seen k times costs O(k²). MEASURED on the degenerate
//! single-key case, `KARAC_AUTO_PAR=0` AOT: 0.10 s at n=32,000 rising to
//! 13.89 s at n=128,000, against a flat 0.006–0.008 s for the `entry` form —
//! **1736× at n=128,000**, and a complexity class, not a constant.
//!
//! ## Why this is a lint and not a doc fix
//!
//! Nothing in the language stops the slow form, nothing in `karac check`
//! flagged it, and it compiles clean and answers correctly. So it is invisible
//! until someone profiles. It was not hypothetical: the kata corpus taught this
//! shape as THE `Map[K, Vec[V]]` pattern, two katas inherited it, one of them
//! grew an entire extra variant file to route around the resulting slowness,
//! and three separate places recorded "Kāra has no in-place map-value mutation"
//! as a language limitation — all while two in-place spellings shipped and one
//! was already in use by a sibling kata (#332's `adj[k].push(v)`).
//!
//! A lint is the durable fix because it turns a discoverability gap into
//! something `karac fix` applies mechanically, which is what would have kept
//! the corpus off the slow path in the first place.
//!
//! ## Shape recognized (deliberately narrow)
//!
//! Only the exact accumulate shape above, with every one of these holding:
//!
//! * the scrutinee is `M.get(K)` where `M`'s value type is a CONTAINER — a
//!   scalar-valued map's read-modify-write is O(1) and not a target;
//! * two arms, `Some(b)` and `None`, in either order;
//! * the `Some` arm binds `let mut v = b.clone();`, mutates `v` with one
//!   method call, and re-inserts it at the same key;
//! * the `None` arm builds a fresh value, applies the SAME method with the
//!   SAME argument text, and inserts at the same key;
//! * `M` and `K` are spelled identically in both arms and the scrutinee.
//!
//! Anything else is left alone. The lint is a suggestion attached to code that
//! already works, so a false positive costs a reader's trust for no correctness
//! benefit — the bar for firing is "this is certainly the accumulate idiom",
//! not "this looks a bit like it". Widening (e.g. `if let`, compound bodies,
//! `SortedMap`) is additive and should come with its own tests.
//!
//! ## Fix
//!
//! One atomic edit replacing the whole `match` with
//! `M.entry(K).or_insert(<fresh>).<method>(<args>);`, built from the ORIGINAL
//! source text of each sub-expression (via spans) rather than re-rendered from
//! the AST, so the user's own spelling, spacing and comments inside those
//! expressions survive the rewrite.

use crate::ast::{
    Block, Expr, ExprKind, ImplItem, Item, ParsedInterpolationPart, Program, Stmt, StmtKind,
    TraitItem,
};
use crate::resolver::SpanKey;
use crate::token::Span;
use crate::typechecker::{FixIt, Type, TypeCheckResult, TypeError, TypeErrorKind};

/// Stable lint name, surfaced in `--output=json` as `lint_name` so consumers
/// can filter or `#[allow]` it.
pub const LINT_NAME: &str = "map_value_clone_reinsert";

/// Container types whose whole contents are copied by `.clone()`. A map whose
/// value is one of these is the quadratic case; a scalar-valued map is not.
fn is_container_value_type(ty: &Type) -> bool {
    match ty {
        Type::Named { name, .. } => matches!(
            name.as_str(),
            "Vec" | "VecDeque" | "Set" | "SortedSet" | "Map" | "SortedMap" | "String"
        ),
        _ => false,
    }
}

/// The map's value type, read off the SCRUTINEE (`m.get(k)`, typed
/// `Option[V]`) rather than off the receiver.
///
/// Typing the receiver looks more direct and does not work: `expr_types` is
/// keyed by span, and a chained access shares its start offset with the
/// enclosing call, so the entry at `m`'s span is the `Option[V]` of `m.get(k)`
/// — the same overwrite hazard `ownership.rs` documents for
/// projection spans. The scrutinee's payload is the value type anyway, and its
/// span is unambiguous, so read it there.
///
/// `None` when the scrutinee is not an `Option` of a container, or has no
/// recorded type. The lint is fail-quiet: no type information, no diagnostic.
fn option_payload_container_type(scrutinee: &Expr, typed: &TypeCheckResult) -> Option<Type> {
    let ty = typed.expr_types.get(&SpanKey::from_span(&scrutinee.span))?;
    let Type::Named { name, args } = ty else {
        return None;
    };
    if name != "Option" {
        return None;
    }
    let value = args.first()?;
    is_container_value_type(value).then(|| value.clone())
}

/// Source text covered by `span`, when the span lies inside `src`. Used to
/// reproduce the author's own spelling of each sub-expression in the fix.
fn text_at<'a>(src: &'a str, span: &Span) -> Option<&'a str> {
    src.get(span.offset..span.offset.saturating_add(span.length))
}

/// One arm's decomposed accumulate body: the binding it builds, the single
/// mutating method call it applies, and the key it re-inserts at.
struct ArmShape<'a> {
    /// RHS of the arm's `let mut` — `b.clone()` in the `Some` arm, the fresh
    /// constructor in the `None` arm.
    init: &'a Expr,
    /// The mutating method applied to the binding (`push`, `insert`, …).
    method: &'a str,
    /// That call's argument list, as one expression span run.
    args: &'a [crate::ast::CallArg],
    /// Receiver of the re-insert (`M`) and its key argument (`K`).
    map: &'a Expr,
    key: &'a Expr,
}

/// Match the three-statement accumulate body shared by both arms:
/// `let mut V = <init>;` / `V.<method>(<args>);` / `let _ = M.insert(K, V);`
fn arm_shape<'a>(body: &'a Expr) -> Option<ArmShape<'a>> {
    let ExprKind::Block(block) = &body.kind else {
        return None;
    };
    if block.final_expr.is_some() || block.stmts.len() != 3 {
        return None;
    }

    // 1. `let mut V = <init>;`
    let StmtKind::Let {
        is_mut: true,
        pattern,
        value,
        ..
    } = &block.stmts[0].kind
    else {
        return None;
    };
    let crate::ast::PatternKind::Binding(binding) = &pattern.kind else {
        return None;
    };

    // 2. `V.<method>(<args>);` — one mutating call on the binding just made.
    let StmtKind::Expr(mutate) = &block.stmts[1].kind else {
        return None;
    };
    let ExprKind::MethodCall {
        object,
        method,
        args,
        ..
    } = &mutate.kind
    else {
        return None;
    };
    if !is_identifier(object, binding) {
        return None;
    }

    // 3. `let _ = M.insert(K, V);` — the reinsert whose whole point this lint
    //    is to remove. The discard is what the corpus writes; a bound result
    //    means the old value is being USED, which is a different program.
    let StmtKind::Let {
        pattern: sink,
        value: reinsert,
        ..
    } = &block.stmts[2].kind
    else {
        return None;
    };
    if !matches!(&sink.kind, crate::ast::PatternKind::Wildcard) {
        return None;
    }
    let ExprKind::MethodCall {
        object: map,
        method: ins,
        args: ins_args,
        ..
    } = &reinsert.kind
    else {
        return None;
    };
    if ins != "insert" || ins_args.len() != 2 {
        return None;
    }
    if !is_identifier(&ins_args[1].value, binding) {
        return None;
    }

    Some(ArmShape {
        init: value,
        method,
        args,
        map,
        key: &ins_args[0].value,
    })
}

fn is_identifier(expr: &Expr, name: &str) -> bool {
    matches!(&expr.kind, ExprKind::Identifier(n) if n == name)
}

/// Same source text ⇒ same expression, for the purposes of "is this the same
/// map / the same key in both arms". Textual rather than structural because
/// the fix reproduces the user's spelling anyway, so anything that renders
/// differently must not be silently merged.
fn same_text(a: &Expr, b: &Expr, src: &str) -> bool {
    match (text_at(src, &a.span), text_at(src, &b.span)) {
        (Some(x), Some(y)) => x.trim() == y.trim(),
        _ => false,
    }
}

/// Inspect one `match` expression; produce a warning if it is the accumulate
/// idiom on a container-valued map.
fn check_match(expr: &Expr, typed: &TypeCheckResult, src: &str) -> Option<TypeError> {
    let ExprKind::Match { scrutinee, arms } = &expr.kind else {
        return None;
    };
    if arms.len() != 2 {
        return None;
    }

    // Scrutinee must be `M.get(K)` on a container-valued map.
    let ExprKind::MethodCall {
        object: map,
        method,
        args,
        ..
    } = &scrutinee.kind
    else {
        return None;
    };
    if method != "get" || args.len() != 1 {
        return None;
    }
    option_payload_container_type(scrutinee, typed)?;

    // Sort the arms into Some / None regardless of written order.
    let mut some_arm = None;
    let mut none_arm = None;
    for arm in arms {
        match &arm.pattern.kind {
            crate::ast::PatternKind::TupleVariant { path, patterns }
                if path.last().map(String::as_str) == Some("Some") && patterns.len() == 1 =>
            {
                let crate::ast::PatternKind::Binding(b) = &patterns[0].kind else {
                    return None;
                };
                some_arm = Some((b.as_str(), arm));
            }
            crate::ast::PatternKind::TupleVariant { path, patterns }
                if path.last().map(String::as_str) == Some("None") && patterns.is_empty() =>
            {
                none_arm = Some(arm);
            }
            // A bare `None` arm carries no payload parens, so the parser may
            // hand it back as a plain identifier pattern rather than a
            // zero-arity variant. Accept both spellings.
            crate::ast::PatternKind::Binding(b) if b == "None" => {
                none_arm = Some(arm);
            }
            _ => return None,
        }
    }
    let (some_binding, some_arm) = some_arm?;
    let none_arm = none_arm?;

    let occupied = arm_shape(&some_arm.body)?;
    let vacant = arm_shape(&none_arm.body)?;

    // The occupied arm must CLONE the matched value — that clone is the cost
    // this lint exists to remove. Without it there is nothing quadratic here.
    let ExprKind::MethodCall {
        object: cloned,
        method: clone_method,
        ..
    } = &occupied.init.kind
    else {
        return None;
    };
    if clone_method != "clone" || !is_identifier(cloned, some_binding) {
        return None;
    }

    // Both arms must mutate the same way at the same key of the same map, or
    // the two branches are not one logical append and the rewrite would change
    // behaviour.
    if occupied.method != vacant.method
        || occupied.args.len() != vacant.args.len()
        || !same_text(occupied.map, vacant.map, src)
        || !same_text(occupied.map, map, src)
        || !same_text(occupied.key, vacant.key, src)
        || !same_text(occupied.key, &args[0].value, src)
    {
        return None;
    }
    for (a, b) in occupied.args.iter().zip(vacant.args.iter()) {
        if !same_text(&a.value, &b.value, src) {
            return None;
        }
    }

    // Build the replacement from the author's own source text.
    let map_text = text_at(src, &map.span)?.trim();
    let key_text = text_at(src, &args[0].value.span)?.trim();
    let fresh_text = text_at(src, &vacant.init.span)?.trim();
    let arg_texts: Vec<&str> = occupied
        .args
        .iter()
        .map(|a| text_at(src, &a.value.span).map(str::trim))
        .collect::<Option<Vec<_>>>()?;
    let replacement = format!(
        "{map_text}.entry({key_text}).or_insert({fresh_text}).{}({});",
        occupied.method,
        arg_texts.join(", ")
    );

    Some(TypeError {
        message: format!(
            "extending a map's container value by clone-and-reinsert copies the whole \
             value on every append — appending the k-th item under one key copies k-1 \
             elements, so building this map is O(k²) where it could be O(k). \
             `{map_text}.entry({key_text}).or_insert(..)` returns a `mut ref` into the \
             map's own slot, so `{}` appends in place and nothing is copied",
            occupied.method
        ),
        span: expr.span,
        kind: TypeErrorKind::TypeMismatch,
        lint_name: Some(LINT_NAME.to_string()),
        fix_it: Some(FixIt {
            span: expr.span,
            replacement,
        }),
        class: Some(crate::diagnostic_class::DiagnosticClass::LintWarning),
        expected: None,
        got: None,
    })
}

/// Walk the program and collect one warning per accumulate site.
///
/// `src` is the original source text; the lint is a no-op without it, since
/// both the same-expression checks and the fix reproduce the author's own
/// spelling by span.
pub fn check_map_value_clone_reinsert(
    program: &Program,
    typed: &TypeCheckResult,
    src: &str,
    cli_lint_overrides: &crate::lints::CliLintOverrides,
) -> Vec<TypeError> {
    // Honour `--allow`/`--deny` and the manifest's `[lints]` table. Per-ITEM
    // `#[allow(map_value_clone_reinsert)]` is NOT yet honoured — this module
    // emits outside `TypeChecker::type_lint_warning`, which is what consults
    // item attributes. That matches `must_use_lint`, which passes the same
    // `false` triple for the source-level flags; wiring attribute scope is a
    // shared follow-up for both, not something to fake here.
    let severity = crate::lints::effective_level_for_module_lint(
        false,
        false,
        false,
        cli_lint_overrides,
        LINT_NAME,
    );
    if matches!(severity, crate::lints::ModuleLintSeverity::Suppress) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for item in &program.items {
        match item {
            Item::Function(f) => walk_block(&f.body, typed, src, &mut out),
            Item::ImplBlock(imp) => {
                for it in &imp.items {
                    if let ImplItem::Method(m) = it {
                        walk_block(&m.body, typed, src, &mut out);
                    }
                }
            }
            Item::TraitDef(t) => {
                for it in &t.items {
                    if let TraitItem::Method(m) = it {
                        if let Some(body) = &m.body {
                            walk_block(body, typed, src, &mut out);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn walk_block(block: &Block, typed: &TypeCheckResult, src: &str, out: &mut Vec<TypeError>) {
    for stmt in &block.stmts {
        walk_stmt(stmt, typed, src, out);
    }
    if let Some(e) = &block.final_expr {
        walk_expr(e, typed, src, out);
    }
}

fn walk_stmt(stmt: &Stmt, typed: &TypeCheckResult, src: &str, out: &mut Vec<TypeError>) {
    // EXHAUSTIVE on purpose — no `_ => {}`. B-2026-08-17-2: this lint shared
    // the partial-walk idiom B-2026-08-16-12 removed from its sibling
    // (`chained_receiver_lint`), and the same measurement showed the same
    // hole — the clone-reinsert idiom inside a CLOSURE body went unreported.
    // An exhaustive match makes the next `StmtKind` addition a compile error
    // here instead of a silently unvisited position.
    match &stmt.kind {
        StmtKind::Let { value, .. } => walk_expr(value, typed, src, out),
        StmtKind::LetUninit { .. } => {}
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            walk_expr(value, typed, src, out);
            walk_block(else_block, typed, src, out);
        }
        StmtKind::Defer { body } => walk_block(body, typed, src, out),
        StmtKind::ErrDefer { body, .. } => walk_block(body, typed, src, out),
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            walk_expr(target, typed, src, out);
            walk_expr(value, typed, src, out);
        }
        StmtKind::MultiAssign { targets, values } => {
            for t in targets {
                walk_expr(t, typed, src, out);
            }
            for v in values {
                walk_expr(v, typed, src, out);
            }
        }
        StmtKind::Expr(e) => walk_expr(e, typed, src, out),
    }
}

fn walk_expr(expr: &Expr, typed: &TypeCheckResult, src: &str, out: &mut Vec<TypeError>) {
    if let Some(diag) = check_match(expr, typed, src) {
        out.push(diag);
        // Do not descend into a site already reported — a nested report would
        // produce overlapping fix-its for one rewrite.
        return;
    }
    // EXHAUSTIVE on purpose — no `_ => {}`; see walk_stmt. Arm inventory
    // mirrors `span_visitor::visit_expr`, the complete in-tree walk. The
    // detection stays on Match nodes only; widening the WALK cannot over-fire,
    // it can only reach Match nodes the old arm set never visited (a closure
    // body being the measured miss).
    match &expr.kind {
        ExprKind::Integer(_, _)
        | ExprKind::Float(_, _)
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
        | ExprKind::Continue { .. }
        | ExprKind::OffsetOf { .. }
        | ExprKind::Error => {}
        ExprKind::InterpolatedStringLit(parts) => {
            for p in parts {
                if let ParsedInterpolationPart::Expr(inner, _) = p {
                    walk_expr(inner, typed, src, out);
                }
            }
        }
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Par(b)
        | ExprKind::Seq(b)
        | ExprKind::Try(b)
        | ExprKind::Unsafe(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. }
        | ExprKind::Lock { body: b, .. } => walk_block(b, typed, src, out),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            walk_expr(condition, typed, src, out);
            walk_block(then_block, typed, src, out);
            if let Some(e) = else_branch {
                walk_expr(e, typed, src, out);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            walk_expr(value, typed, src, out);
            walk_block(then_block, typed, src, out);
            if let Some(e) = else_branch {
                walk_expr(e, typed, src, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            walk_expr(condition, typed, src, out);
            walk_block(body, typed, src, out);
        }
        ExprKind::WhileLet { value, body, .. } => {
            walk_expr(value, typed, src, out);
            walk_block(body, typed, src, out);
        }
        ExprKind::For { iterable, body, .. } => {
            walk_expr(iterable, typed, src, out);
            walk_block(body, typed, src, out);
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_expr(scrutinee, typed, src, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    walk_expr(g, typed, src, out);
                }
                walk_expr(&arm.body, typed, src, out);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            walk_expr(object, typed, src, out);
            for a in args {
                walk_expr(&a.value, typed, src, out);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            walk_expr(callee, typed, src, out);
            for a in args {
                walk_expr(&a.value, typed, src, out);
            }
        }
        ExprKind::OptionalChain { object, args, .. } => {
            walk_expr(object, typed, src, out);
            if let Some(args) = args {
                for a in args {
                    walk_expr(&a.value, typed, src, out);
                }
            }
        }
        ExprKind::Index { object, index } => {
            walk_expr(object, typed, src, out);
            walk_expr(index, typed, src, out);
        }
        ExprKind::Binary { left, right, .. }
        | ExprKind::NilCoalesce { left, right }
        | ExprKind::Pipe { left, right } => {
            walk_expr(left, typed, src, out);
            walk_expr(right, typed, src, out);
        }
        ExprKind::Unary { operand, .. } => walk_expr(operand, typed, src, out),
        ExprKind::Question(inner) => walk_expr(inner, typed, src, out),
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            walk_expr(object, typed, src, out)
        }
        ExprKind::Cast { expr: inner, .. } => walk_expr(inner, typed, src, out),
        ExprKind::Closure { body, .. } => walk_expr(body, typed, src, out),
        ExprKind::Return(opt) => {
            if let Some(inner) = opt {
                walk_expr(inner, typed, src, out);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(v) = value {
                walk_expr(v, typed, src, out);
            }
        }
        ExprKind::Tuple(exprs) | ExprKind::ArrayLiteral(exprs) => {
            for x in exprs {
                walk_expr(x, typed, src, out);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for x in items {
                walk_expr(x, typed, src, out);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            walk_expr(value, typed, src, out);
            walk_expr(count, typed, src, out);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs {
                walk_expr(k, typed, src, out);
                walk_expr(v, typed, src, out);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                walk_expr(&f.value, typed, src, out);
            }
            if let Some(sp) = spread {
                walk_expr(sp, typed, src, out);
            }
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(st) = start {
                walk_expr(st, typed, src, out);
            }
            if let Some(en) = end {
                walk_expr(en, typed, src, out);
            }
        }
        ExprKind::Providers { bindings, body } => {
            for pb in bindings {
                walk_expr(&pb.value, typed, src, out);
            }
            walk_block(body, typed, src, out);
        }
    }
}
