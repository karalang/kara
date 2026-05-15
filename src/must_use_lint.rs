// src/must_use_lint.rs
//! `must_use` lint — slice 1 of the `#[must_use]` mandate
//! (`docs/implementation_checklist/phase-5-diagnostics.md` §
//! `#[must_use]` mandate).
//!
//! Slice 1 ships the *implicit* `#[must_use]` recognition for the two
//! language-level types `Result[T, E]` and `Option[T]`. Discarding the
//! return value of an expression of either type at statement position
//! produces a `warning[must_use]` pointing at the discarded value, with
//! a `help` line offering the canonical fix (`let _ = ...` to
//! acknowledge the discard explicitly, or `match` / `if let` to consume
//! the value) and a `note` line explaining *why* these two types are
//! treated as implicitly must-use (silently dropping them abandons the
//! error / absence branch the author meant to handle).
//!
//! Why a lint module rather than the typechecker's error stream: the
//! typechecker treats `TypeCheckResult.errors` as fatal (the codegen
//! path bails on the first non-empty errors list — see
//! `src/cli.rs::pipeline.typed.errors`-gated bail), so a must-use
//! diagnostic emitted through that channel would block compilation
//! rather than warn. The lint module pattern (`undocumented_unsafe`,
//! `unsafe_op_in_unsafe_fn`, `ffi_float_eq`, `ambiguous_not_comparison`)
//! is the right shape for a non-fatal warning that consumers can
//! acknowledge by binding to `_`.
//!
//! Slices 2–4 of the broader `#[must_use]` mandate extend this module:
//! slice 2 applies the `#[must_use]` attribute to stdlib types (iterator
//! adapters, guards, builders, `JoinHandle[T]`, pure-transformation
//! methods); slice 3 adds the `missing_must_use` stdlib-hygiene lint;
//! slice 4 generalises the discarded-value detection to honour the
//! attribute on user-defined types and functions. The walker shape and
//! diagnostic shape established here carry forward unchanged.

use crate::ast::{
    Block, Expr, ExprKind, FieldInit, ImplItem, Item, MatchArm, Program, Stmt, StmtKind, TraitItem,
};
use crate::resolver::SpanKey;
use crate::token::Span;
use crate::typechecker::{Type, TypeCheckResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LintLevel {
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct LintDiagnostic {
    pub level: LintLevel,
    pub span: Span,
    pub message: String,
    pub lint_name: String,
    /// Actionable suggestion. Rendered as a `= help:` continuation line
    /// under the primary diagnostic, mirroring `unsafe_lint`'s slice 4
    /// shape so the CLI rendering helper can carry across lint modules.
    pub help: Option<String>,
    /// Conceptual explanation: why the rule fires, surfaced in the same
    /// diagnostic so a first-time reader does not have to chase the spec
    /// to understand why discarding `Result` / `Option` is a hazard.
    pub note: Option<String>,
}

/// Run the implicit-`#[must_use]` lint over the parsed program.
///
/// `typed` is optional: the lint is a no-op when type information is
/// unavailable, since the check fundamentally consults `expr_types` to
/// recognise `Result[T, E]` / `Option[T]` at statement position. The
/// only-language-level-types shape of slice 1 means there is nothing
/// useful to report without typecheck data.
pub fn check_implicit_must_use(
    program: &Program,
    typed: Option<&TypeCheckResult>,
) -> Vec<LintDiagnostic> {
    let Some(typed) = typed else {
        return Vec::new();
    };
    let mut diags: Vec<LintDiagnostic> = Vec::new();
    {
        let mut walker = Walker {
            typed,
            diags: &mut diags,
        };
        for item in &program.items {
            match item {
                Item::Function(f) => walker.walk_block(&f.body),
                Item::ImplBlock(imp) => {
                    for it in &imp.items {
                        if let ImplItem::Method(m) = it {
                            walker.walk_block(&m.body);
                        }
                    }
                }
                Item::TraitDef(t) => {
                    for ti in &t.items {
                        if let TraitItem::Method(m) = ti {
                            if let Some(body) = &m.body {
                                walker.walk_block(body);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    diags
}

struct Walker<'a> {
    typed: &'a TypeCheckResult,
    diags: &'a mut Vec<LintDiagnostic>,
}

impl Walker<'_> {
    fn walk_block(&mut self, block: &Block) {
        // Statement-position expressions are the discard sites — the
        // value flows nowhere. The block's `final_expr` is the block's
        // *value* and is consumed by whatever consumes the block, so it
        // is recursed into but NOT checked for must-use at this level
        // (the enclosing context decides whether the value flows out or
        // is itself discarded).
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(tail) = &block.final_expr {
            self.walk_expr(tail);
        }
    }

    fn walk_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Expr(e) => {
                self.check_discard(e);
                self.walk_expr(e);
            }
            StmtKind::Let { value, .. } => self.walk_expr(value),
            StmtKind::LetUninit { .. } => {}
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                self.walk_expr(value);
                self.walk_block(else_block);
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.walk_expr(target);
                self.walk_expr(value);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.walk_block(body);
            }
        }
    }

    /// At a statement-position expression, look up the expression's
    /// inferred type and emit a `must_use` warning when it is one of the
    /// two language-level implicit-must-use types. The match against
    /// `Type::Named { name, .. }` is intentionally name-based — these
    /// types live in the prelude (`runtime/stdlib/option.kara`,
    /// `runtime/stdlib/result.kara`) and the typechecker already
    /// surfaces them as `Type::Named { name: "Option" / "Result", .. }`
    /// (see `src/typechecker.rs:4224-4227`). Slice 4 will extend this
    /// check to honour `#[must_use]` on arbitrary user-defined types
    /// and on function returns through the registry.
    fn check_discard(&mut self, expr: &Expr) {
        let key = SpanKey::from_span(&expr.span);
        let Some(ty) = self.typed.expr_types.get(&key) else {
            return;
        };
        let Type::Named { name, .. } = ty else {
            return;
        };
        let (kind, why) = match name.as_str() {
            "Result" => ("Result", "an `Err` branch the caller meant to handle"),
            "Option" => ("Option", "a `None` branch the caller meant to handle"),
            _ => return,
        };
        self.diags.push(LintDiagnostic {
            level: LintLevel::Warning,
            span: expr.span.clone(),
            message: format!("discarded `{kind}` value — `{kind}` is implicitly `#[must_use]`",),
            lint_name: "must_use".to_string(),
            help: Some(
                "bind the value with `let _ = ...` to acknowledge the discard \
                 explicitly, or consume it via `match` / `if let` so each variant \
                 is handled."
                    .to_string(),
            ),
            note: Some(format!(
                "`{kind}` is treated as implicitly `#[must_use]` because dropping it \
                 silently abandons {why} — the two language-level types `Result[T, E]` \
                 and `Option[T]` carry this recognition in the typechecker rather than \
                 as a user-visible `#[must_use]` attribute."
            )),
        });
    }

    fn walk_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Block(block)
            | ExprKind::Unsafe(block)
            | ExprKind::Loop { body: block, .. }
            | ExprKind::LabeledBlock { body: block, .. }
            | ExprKind::Seq(block)
            | ExprKind::Par(block)
            | ExprKind::Try(block) => self.walk_block(block),
            ExprKind::Lock { body, .. } | ExprKind::Providers { body, .. } => {
                self.walk_block(body);
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.walk_expr(condition);
                self.walk_block(then_block);
                if let Some(e) = else_branch {
                    self.walk_expr(e);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.walk_expr(value);
                self.walk_block(then_block);
                if let Some(e) = else_branch {
                    self.walk_expr(e);
                }
            }
            ExprKind::While {
                condition, body, ..
            }
            | ExprKind::WhileLet {
                value: condition,
                body,
                ..
            } => {
                self.walk_expr(condition);
                self.walk_block(body);
            }
            ExprKind::For { iterable, body, .. } => {
                self.walk_expr(iterable);
                self.walk_block(body);
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee);
                for arm in arms {
                    self.walk_match_arm(arm);
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.walk_expr(operand),
            ExprKind::NilCoalesce { left, right } | ExprKind::Pipe { left, right } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Call { callee, args } => {
                self.walk_expr(callee);
                for a in args {
                    self.walk_expr(&a.value);
                }
            }
            ExprKind::MethodCall { object, args, .. }
            | ExprKind::OptionalChain {
                object,
                args: Some(args),
                ..
            } => {
                self.walk_expr(object);
                for a in args {
                    self.walk_expr(&a.value);
                }
            }
            ExprKind::OptionalChain {
                object, args: None, ..
            } => self.walk_expr(object),
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.walk_expr(object);
            }
            ExprKind::Index { object, index } => {
                self.walk_expr(object);
                self.walk_expr(index);
            }
            ExprKind::Closure { body, .. } => self.walk_expr(body),
            ExprKind::Return(Some(e)) | ExprKind::Question(e) | ExprKind::Cast { expr: e, .. } => {
                self.walk_expr(e);
            }
            ExprKind::Break { value: Some(e), .. } => self.walk_expr(e),
            ExprKind::Tuple(elems) | ExprKind::ArrayLiteral(elems) => {
                for e in elems {
                    self.walk_expr(e);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.walk_expr(value);
                self.walk_expr(count);
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.walk_expr(e);
                }
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.walk_expr(k);
                    self.walk_expr(v);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.walk_field_init(f);
                }
                if let Some(s) = spread {
                    self.walk_expr(s);
                }
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.walk_expr(s);
                }
                if let Some(e) = end {
                    self.walk_expr(e);
                }
            }
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::CharLit(..)
            | ExprKind::StringLit(..)
            | ExprKind::MultiStringLit(..)
            | ExprKind::InterpolatedStringLit(..)
            | ExprKind::Bool(..)
            | ExprKind::Identifier(..)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::PipePlaceholder
            | ExprKind::Return(None)
            | ExprKind::Break { value: None, .. }
            | ExprKind::Continue { .. }
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}
        }
    }

    fn walk_match_arm(&mut self, arm: &MatchArm) {
        if let Some(guard) = &arm.guard {
            self.walk_expr(guard);
        }
        self.walk_expr(&arm.body);
    }

    fn walk_field_init(&mut self, f: &FieldInit) {
        self.walk_expr(&f.value);
    }
}
