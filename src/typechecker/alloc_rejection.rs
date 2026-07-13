//! `E_PANICKING_ALLOC_REJECTED` pass (phase-8-stdlib-floor item 4).
//!
//! Under `panic_on_alloc_failure = false`, a post-typecheck walk over every
//! function / method body flags each panicking heap-allocating site against the
//! fallible-allocation registry (`crate::fallible_alloc`): a base alloc method
//! on a builtin collection (`v.push(x)`), a panicking constructor
//! (`Vec.with_capacity(n)`), and the implicit-allocation lowering primitives
//! (collection literals, f-string interpolation, `String` concatenation). Each
//! site is reported with the `try_*` companion where one exists, or a
//! restructure hint where none does.
//!
//! This is a dedicated source-AST walk rather than inline emission during
//! inference for two reasons: (a) it sees the literal `try_push` /
//! `try_with_capacity` node and skips it (the companions are the fix, never
//! flagged), and (b) it is immune to the `check_expr`-vs-`infer_call`
//! divergence that an annotated `let v: Vec[i64] = Vec.with_capacity(8)` takes.
//! Receiver types come from the populated `expr_types` table.

use crate::ast::*;
use crate::fallible_alloc;
use crate::resolver::SpanKey;
use crate::token::Span;

use super::types::Type;

/// One flagged site: where to report, the operation subject (already
/// back-tick-wrapped where it names code), and the `try_*` companion to suggest
/// (or `None` for a restructure hint).
struct AllocSite {
    span: Span,
    subject: String,
    companion: Option<String>,
}

/// Builtin heap-allocating collection display name (`"Vec"` / `"VecDeque"` /
/// `"Map"` / `"Set"` / `"SortedSet"` / `"String"`) of a receiver type, peeling
/// `ref` / `mut ref`. Pure (no inference). Shared with `infer_method_call`,
/// which records the result keyed by the method-call span for this pass.
pub(super) fn builtin_collection_name(ty: &Type) -> Option<&'static str> {
    match ty {
        Type::Str => Some("String"),
        Type::Named { name, .. } => match name.as_str() {
            "Vec" => Some("Vec"),
            "VecDeque" => Some("VecDeque"),
            "Map" => Some("Map"),
            "SortedMap" => Some("SortedMap"),
            "Set" => Some("Set"),
            "SortedSet" => Some("SortedSet"),
            _ => None,
        },
        Type::Ref(inner) | Type::MutRef(inner) => builtin_collection_name(inner),
        _ => None,
    }
}

impl<'a> super::TypeChecker<'a> {
    /// Entry point — invoked from `check()` after inference. No-op in the
    /// default (`panic_on_alloc_failure` unset / `true`) mode.
    pub(super) fn check_panicking_alloc_rejections(&mut self) {
        if self.profile_config.panics_on_alloc_failure() {
            return;
        }
        let program = self.program; // `&'a Program` — Copy, independent of `self`
        let mut sites: Vec<AllocSite> = Vec::new();
        for item in &program.items {
            self.collect_alloc_sites_item(item, &mut sites);
        }
        for site in sites {
            self.reject_panicking_alloc(&site.span, &site.subject, site.companion.as_deref());
        }
    }

    fn collect_alloc_sites_item(&self, item: &Item, out: &mut Vec<AllocSite>) {
        match item {
            Item::Function(f) => self.collect_in_block(&f.body, out),
            Item::ImplBlock(b) => {
                for it in &b.items {
                    if let ImplItem::Method(m) = it {
                        self.collect_in_block(&m.body, out);
                    }
                }
            }
            Item::TraitDef(t) => {
                for it in &t.items {
                    if let TraitItem::Method(m) = it {
                        if let Some(body) = &m.body {
                            self.collect_in_block(body, out);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn collect_in_block(&self, b: &Block, out: &mut Vec<AllocSite>) {
        for s in &b.stmts {
            self.collect_in_stmt(s, out);
        }
        if let Some(fe) = &b.final_expr {
            self.collect_in_expr(fe, out);
        }
    }

    fn collect_in_stmt(&self, s: &Stmt, out: &mut Vec<AllocSite>) {
        match &s.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, .. } => self.collect_in_expr(value, out),
            StmtKind::LetUninit { .. } => {}
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                self.collect_in_expr(value, out);
                self.collect_in_block(else_block, out);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.collect_in_block(body, out)
            }
            StmtKind::Assign { target, value } => {
                self.collect_in_expr(target, out);
                self.collect_in_expr(value, out);
            }
            // `s += t` on a `String` builds a fresh allocation (concatenation).
            StmtKind::CompoundAssign { target, value, op } => {
                if matches!(op, CompoundOp::Add) && self.expr_is_string(target) {
                    out.push(AllocSite {
                        span: s.span.clone(),
                        subject: "`String` `+=` concatenation".to_string(),
                        companion: None,
                    });
                }
                self.collect_in_expr(target, out);
                self.collect_in_expr(value, out);
            }
            StmtKind::Expr(e) => self.collect_in_expr(e, out),
        }
    }

    fn collect_in_expr(&self, e: &Expr, out: &mut Vec<AllocSite>) {
        match &e.kind {
            // ── Registry sites ────────────────────────────────────
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                if fallible_alloc::TRY_ALLOC_INSTANCE_BASES.contains(&method.as_str()) {
                    // The receiver collection name was recorded during inference
                    // keyed by the method-call span (the receiver's span equals
                    // it, so `expr_types` there holds the method's return type,
                    // not the receiver type).
                    if let Some(coll) = self
                        .method_receiver_collections
                        .get(&SpanKey::from_span(&e.span))
                    {
                        out.push(AllocSite {
                            span: e.span.clone(),
                            subject: format!("`{coll}.{method}`"),
                            companion: Some(format!("{coll}.try_{method}")),
                        });
                    }
                }
                self.collect_in_expr(object, out);
                for a in args {
                    self.collect_in_expr(&a.value, out);
                }
            }
            ExprKind::Call { callee, args } => {
                if let ExprKind::Path { segments, .. } = &callee.kind {
                    if segments.len() == 2
                        && fallible_alloc::TRY_ALLOC_STATIC_BASES.contains(&segments[1].as_str())
                        && matches!(segments[0].as_str(), "Vec" | "VecDeque" | "String")
                    {
                        out.push(AllocSite {
                            span: e.span.clone(),
                            subject: format!("`{}.{}`", segments[0], segments[1]),
                            companion: Some(format!("{}.try_{}", segments[0], segments[1])),
                        });
                    }
                }
                self.collect_in_expr(callee, out);
                for a in args {
                    self.collect_in_expr(&a.value, out);
                }
            }
            // ── Implicit-allocation lowering primitives (no companion) ──
            ExprKind::ArrayLiteral(items) => {
                if !items.is_empty() {
                    out.push(AllocSite {
                        span: e.span.clone(),
                        subject: "a `[...]` Vec literal".to_string(),
                        companion: None,
                    });
                }
                for x in items {
                    self.collect_in_expr(x, out);
                }
            }
            ExprKind::PrefixCollectionLiteral { type_name, items } => {
                if !items.is_empty() && matches!(type_name.as_str(), "Vec" | "Set") {
                    out.push(AllocSite {
                        span: e.span.clone(),
                        subject: format!("a `{type_name}[...]` collection literal"),
                        companion: None,
                    });
                }
                for x in items {
                    self.collect_in_expr(x, out);
                }
            }
            ExprKind::MapLiteral(pairs) => {
                if !pairs.is_empty() {
                    out.push(AllocSite {
                        span: e.span.clone(),
                        subject: "a `{ k: v }` Map literal".to_string(),
                        companion: None,
                    });
                }
                for (k, v) in pairs {
                    self.collect_in_expr(k, out);
                    self.collect_in_expr(v, out);
                }
            }
            ExprKind::RepeatLiteral {
                type_name,
                value,
                count,
            } => {
                // Bare `[v; n]` defaults to `Vec`; `Vec[v; n]` is explicit.
                // `Array[v; n]` is a fixed stack array — not a heap alloc.
                if !matches!(type_name.as_deref(), Some("Array")) {
                    out.push(AllocSite {
                        span: e.span.clone(),
                        subject: "a `[v; n]` Vec repeat-literal".to_string(),
                        companion: None,
                    });
                }
                self.collect_in_expr(value, out);
                self.collect_in_expr(count, out);
            }
            ExprKind::InterpolatedStringLit(parts) => {
                if parts
                    .iter()
                    .any(|p| matches!(p, ParsedInterpolationPart::Expr(_, _)))
                {
                    out.push(AllocSite {
                        span: e.span.clone(),
                        subject: "an f-string interpolation".to_string(),
                        companion: None,
                    });
                }
                for p in parts {
                    if let ParsedInterpolationPart::Expr(inner, _) = p {
                        self.collect_in_expr(inner, out);
                    }
                }
            }
            ExprKind::Binary { op, left, right } => {
                if matches!(op, BinOp::Add) && self.expr_is_string(left) {
                    out.push(AllocSite {
                        span: e.span.clone(),
                        subject: "`String` `+` concatenation".to_string(),
                        companion: None,
                    });
                }
                self.collect_in_expr(left, out);
                self.collect_in_expr(right, out);
            }
            // ── Pure recursion (mirror of span_visitor::visit_expr) ──
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
            ExprKind::Unary { operand, .. } => self.collect_in_expr(operand, out),
            ExprKind::Question(inner) => self.collect_in_expr(inner, out),
            ExprKind::OptionalChain { object, args, .. } => {
                self.collect_in_expr(object, out);
                if let Some(a) = args {
                    for arg in a {
                        self.collect_in_expr(&arg.value, out);
                    }
                }
            }
            ExprKind::NilCoalesce { left, right } | ExprKind::Pipe { left, right } => {
                self.collect_in_expr(left, out);
                self.collect_in_expr(right, out);
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.collect_in_expr(object, out)
            }
            ExprKind::Index { object, index } => {
                self.collect_in_expr(object, out);
                self.collect_in_expr(index, out);
            }
            ExprKind::Block(b) | ExprKind::Comptime(b) => self.collect_in_block(b, out),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.collect_in_expr(condition, out);
                self.collect_in_block(then_block, out);
                if let Some(eb) = else_branch {
                    self.collect_in_expr(eb, out);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.collect_in_expr(value, out);
                self.collect_in_block(then_block, out);
                if let Some(eb) = else_branch {
                    self.collect_in_expr(eb, out);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.collect_in_expr(scrutinee, out);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.collect_in_expr(guard, out);
                    }
                    self.collect_in_expr(&arm.body, out);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.collect_in_expr(condition, out);
                self.collect_in_block(body, out);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.collect_in_expr(value, out);
                self.collect_in_block(body, out);
            }
            ExprKind::For { iterable, body, .. } => {
                self.collect_in_expr(iterable, out);
                self.collect_in_block(body, out);
            }
            ExprKind::Loop { body, .. } => self.collect_in_block(body, out),
            ExprKind::LabeledBlock { body, .. } => self.collect_in_block(body, out),
            ExprKind::Closure { body, .. } => self.collect_in_expr(body, out),
            ExprKind::Return(opt) | ExprKind::Break { value: opt, .. } => {
                if let Some(inner) = opt {
                    self.collect_in_expr(inner, out);
                }
            }
            ExprKind::Tuple(exprs) => {
                for x in exprs {
                    self.collect_in_expr(x, out);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.collect_in_expr(&f.value, out);
                }
                if let Some(sp) = spread {
                    self.collect_in_expr(sp, out);
                }
            }
            ExprKind::Cast { expr, .. } => self.collect_in_expr(expr, out),
            ExprKind::Range { start, end, .. } => {
                if let Some(st) = start {
                    self.collect_in_expr(st, out);
                }
                if let Some(en) = end {
                    self.collect_in_expr(en, out);
                }
            }
            ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b)
            | ExprKind::Lock { body: b, .. } => self.collect_in_block(b, out),
            ExprKind::Providers { bindings, body } => {
                for pb in bindings {
                    self.collect_in_expr(&pb.value, out);
                }
                self.collect_in_block(body, out);
            }
        }
    }

    /// Recorded inferred type of an expression, if any.
    fn expr_type(&self, e: &Expr) -> Option<&Type> {
        self.expr_types.get(&SpanKey::from_span(&e.span))
    }

    /// `true` when `e`'s inferred type is `String` (`Type::Str`), peeling
    /// `ref` / `mut ref`. Drives the `String` concatenation rejection.
    fn expr_is_string(&self, e: &Expr) -> bool {
        fn is_str(ty: &Type) -> bool {
            match ty {
                Type::Str => true,
                Type::Ref(inner) | Type::MutRef(inner) => is_str(inner),
                _ => false,
            }
        }
        self.expr_type(e).is_some_and(is_str)
    }
}
