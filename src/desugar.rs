//! AST-rewriting pre-resolve passes that eliminate sugar so downstream
//! phases only see the canonical form.
//!
//! Today this houses one pass: slice 2 of the `impl Trait` epic —
//! argument-position `impl Trait` desugars to a fresh anonymous generic
//! parameter on the enclosing function. See `docs/design.md § `impl
//! Trait` (Existential Types)` and `phase-5-diagnostics.md` line 395.
//!
//! Pipeline placement: between [`crate::parse`] and [`crate::resolve`].
//! The compilation drivers in `lib.rs` and `cli.rs` invoke
//! [`desugar_program`] on the mutable `Program` before resolution; the
//! formatter path deliberately skips this pass so `impl Trait` round-trips
//! verbatim.

use crate::ast::*;
use crate::token::Span;

/// Run every AST-rewriting pre-resolve pass over `program` in place.
/// Today: argument-position `impl Trait` desugar (slice 2) and
/// parallel/destructuring-assignment desugar.
pub fn desugar_program(program: &mut Program) {
    desugar_impl_trait_args_in_program(program);
    desugar_multi_assign_in_program(program);
}

// ── parallel / destructuring assignment desugar ─────────────────
//
// `t1, ..., tn = v1, ..., vn;` (parsed as `StmtKind::MultiAssign`) is rewritten
// into a block-expr statement that binds every right-hand value to a fresh
// temporary (left to right) and then writes each target from its temporary:
//
//     { let _t0 = v0; ...; let _tn = vn; target0 = _t0; ...; targetn = _tn; }
//
// Evaluating all values before writing any target is what gives `a, b = b, a`
// its swap semantics. After this pass no `StmtKind::MultiAssign` remains, so
// every phase from the resolver onward treats it as ordinary `let`/`Assign`
// nodes. The formatter skips this pass, so it still sees — and round-trips —
// the surface node.

fn desugar_multi_assign_in_program(program: &mut Program) {
    for item in &mut program.items {
        match item {
            Item::Function(f) => walk_block(&mut f.body),
            Item::ImplBlock(imp) => {
                for it in &mut imp.items {
                    if let ImplItem::Method(m) = it {
                        walk_block(&mut m.body);
                    }
                }
            }
            Item::TraitDef(t) => {
                for it in &mut t.items {
                    if let TraitItem::Method(m) = it {
                        if let Some(body) = &mut m.body {
                            walk_block(body);
                        }
                    }
                }
            }
            Item::TestCase(tc) => walk_block(&mut tc.body),
            Item::ConstDecl(c) => walk_expr(&mut c.value),
            _ => {}
        }
    }
}

fn walk_block(block: &mut Block) {
    for stmt in &mut block.stmts {
        walk_stmt(stmt);
    }
    if let Some(e) = &mut block.final_expr {
        walk_expr(e);
    }
}

fn walk_stmt(stmt: &mut Stmt) {
    match &mut stmt.kind {
        StmtKind::Let { value, .. } => walk_expr(value),
        StmtKind::LetUninit { .. } => {}
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            walk_expr(value);
            walk_block(else_block);
        }
        StmtKind::Defer { body } => walk_block(body),
        StmtKind::ErrDefer { body, .. } => walk_block(body),
        StmtKind::Assign { target, value } => {
            walk_expr(target);
            walk_expr(value);
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            walk_expr(target);
            walk_expr(value);
        }
        StmtKind::Expr(e) => walk_expr(e),
        StmtKind::MultiAssign { .. } => {
            let span = stmt.span.clone();
            let placeholder = StmtKind::Expr(Expr {
                kind: ExprKind::Error,
                span: span.clone(),
            });
            let StmtKind::MultiAssign {
                mut targets,
                mut values,
            } = std::mem::replace(&mut stmt.kind, placeholder)
            else {
                unreachable!("matched MultiAssign above")
            };
            // Operands may themselves contain nested blocks (e.g. a block-expr
            // value) that hold further multi-assigns — recurse before expanding.
            for t in targets.iter_mut() {
                walk_expr(t);
            }
            for v in values.iter_mut() {
                walk_expr(v);
            }
            stmt.kind = expand_multi_assign(targets, values, span);
        }
    }
}

/// Build the block-expr `StmtKind` a parallel assignment lowers to. The
/// temporaries carry a `__karac_pa_<offset>_<i>` name that user code cannot
/// collide with and live only inside the synthesized block's scope.
fn expand_multi_assign(targets: Vec<Expr>, values: Vec<Expr>, span: Span) -> StmtKind {
    let n = targets.len();
    let mut stmts: Vec<Stmt> = Vec::with_capacity(n * 2);
    let mut temp_names: Vec<String> = Vec::with_capacity(n);
    for (i, value) in values.into_iter().enumerate() {
        let name = format!("__karac_pa_{}_{}", span.offset, i);
        let vspan = value.span.clone();
        temp_names.push(name.clone());
        stmts.push(Stmt {
            span: vspan.clone(),
            kind: StmtKind::Let {
                is_mut: false,
                pattern: Pattern {
                    kind: PatternKind::Binding(name),
                    span: vspan,
                },
                ty: None,
                value,
            },
        });
    }
    for (target, name) in targets.into_iter().zip(temp_names) {
        let tspan = target.span.clone();
        stmts.push(Stmt {
            span: tspan.clone(),
            kind: StmtKind::Assign {
                target,
                value: Expr {
                    kind: ExprKind::Identifier(name),
                    span: tspan,
                },
            },
        });
    }
    StmtKind::Expr(Expr {
        kind: ExprKind::Block(Block {
            stmts,
            final_expr: None,
            span: span.clone(),
        }),
        span,
    })
}

fn walk_expr(expr: &mut Expr) {
    match &mut expr.kind {
        // Leaves — no sub-expressions or blocks.
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::CharLit(..)
        | ExprKind::ByteLit(..)
        | ExprKind::StringLit(..)
        | ExprKind::MultiStringLit(..)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(..)
        | ExprKind::Identifier(..)
        | ExprKind::Path { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::PipePlaceholder
        | ExprKind::Continue { .. }
        | ExprKind::OffsetOf { .. }
        | ExprKind::Error => {}

        ExprKind::InterpolatedStringLit(parts) => {
            for part in parts.iter_mut() {
                if let ParsedInterpolationPart::Expr(e) = part {
                    walk_expr(e);
                }
            }
        }
        ExprKind::Binary { left, right, .. }
        | ExprKind::NilCoalesce { left, right }
        | ExprKind::Pipe { left, right } => {
            walk_expr(left);
            walk_expr(right);
        }
        ExprKind::Unary { operand, .. } => walk_expr(operand),
        ExprKind::Question(e) => walk_expr(e),
        ExprKind::OptionalChain { object, args, .. } => {
            walk_expr(object);
            if let Some(args) = args {
                for a in args.iter_mut() {
                    walk_expr(&mut a.value);
                }
            }
        }
        ExprKind::Call { callee, args } => {
            walk_expr(callee);
            for a in args.iter_mut() {
                walk_expr(&mut a.value);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            walk_expr(object);
            for a in args.iter_mut() {
                walk_expr(&mut a.value);
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            walk_expr(object)
        }
        ExprKind::Index { object, index } => {
            walk_expr(object);
            walk_expr(index);
        }
        ExprKind::Block(b) => walk_block(b),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            walk_expr(condition);
            walk_block(then_block);
            if let Some(e) = else_branch {
                walk_expr(e);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            walk_expr(value);
            walk_block(then_block);
            if let Some(e) = else_branch {
                walk_expr(e);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_expr(scrutinee);
            for arm in arms.iter_mut() {
                if let Some(g) = &mut arm.guard {
                    walk_expr(g);
                }
                walk_expr(&mut arm.body);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            walk_expr(condition);
            walk_block(body);
        }
        ExprKind::WhileLet { value, body, .. } => {
            walk_expr(value);
            walk_block(body);
        }
        ExprKind::For { iterable, body, .. } => {
            walk_expr(iterable);
            walk_block(body);
        }
        ExprKind::Loop { body, .. } => walk_block(body),
        ExprKind::LabeledBlock { body, .. } => walk_block(body),
        ExprKind::Closure { body, .. } => walk_expr(body),
        ExprKind::Return(opt) => {
            if let Some(e) = opt {
                walk_expr(e);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(e) = value {
                walk_expr(e);
            }
        }
        ExprKind::Tuple(items)
        | ExprKind::ArrayLiteral(items)
        | ExprKind::PrefixCollectionLiteral { items, .. } => {
            for e in items.iter_mut() {
                walk_expr(e);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            walk_expr(value);
            walk_expr(count);
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs.iter_mut() {
                walk_expr(k);
                walk_expr(v);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields.iter_mut() {
                walk_expr(&mut f.value);
            }
            if let Some(s) = spread {
                walk_expr(s);
            }
        }
        ExprKind::Cast { expr: e, .. } => walk_expr(e),
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                walk_expr(s);
            }
            if let Some(e) = end {
                walk_expr(e);
            }
        }
        ExprKind::Unsafe(b) | ExprKind::Try(b) | ExprKind::Seq(b) | ExprKind::Par(b) => {
            walk_block(b)
        }
        ExprKind::Lock { mutex, body, .. } => {
            walk_expr(mutex);
            walk_block(body);
        }
        ExprKind::Providers { bindings, body } => {
            for bnd in bindings.iter_mut() {
                walk_expr(&mut bnd.value);
            }
            walk_block(body);
        }
    }
}

// ── `impl Trait` argument-position desugar ──────────────────────

fn desugar_impl_trait_args_in_program(program: &mut Program) {
    for item in &mut program.items {
        match item {
            Item::Function(f) => desugar_impl_trait_args_in_function(f),
            Item::ImplBlock(imp) => {
                for it in &mut imp.items {
                    if let ImplItem::Method(method) = it {
                        desugar_impl_trait_args_in_function(method);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Rewrite every top-level `TypeKind::ImplTrait` on `f.params[i].ty` into a
/// `TypeKind::Path` reference to a freshly synthesized anonymous generic
/// parameter `T_impl_arg_N`, and append that parameter (with the original
/// trait as its bound) to `f.generic_params`. Per-occurrence: two
/// `impl T` parameters produce two distinct synthetic params so the
/// typechecker never unifies them.
///
/// Only top-level argument-position occurrences are desugared. Return-position
/// `impl Trait` (slice 3) and TAIT-RHS `impl Trait` (slice 6) are intentionally
/// left intact so the typechecker's slice-1 stub continues to surface them.
/// Nested-through-generic-args (`Vec[impl T]`) and trait-method argument
/// position were already rejected at parse (slice 1), so they never reach
/// this pass.
///
/// `use_effects` on argument-position `impl Trait` is dropped: per the parent
/// spec the argument-position desugar produces "the same bounds (no
/// existential, no special handling downstream)" — the `with E'` clause is
/// meaningful only on return-position existentials, where slice 3 + Phase 8
/// pick it up.
fn desugar_impl_trait_args_in_function(f: &mut Function) {
    let mut synthetic_params: Vec<GenericParam> = Vec::new();
    let mut counter = 0usize;
    for param in &mut f.params {
        let TypeKind::ImplTrait {
            trait_path,
            args,
            span: impl_trait_span,
            ..
        } = &param.ty.kind
        else {
            continue;
        };

        let synthetic_name = format!("T_impl_arg_{counter}");
        counter += 1;

        let bound = TraitBound {
            path: trait_path.segments.clone(),
            generic_args: if args.is_empty() {
                None
            } else {
                Some(args.clone())
            },
            span: impl_trait_span.clone(),
        };
        synthetic_params.push(GenericParam {
            name: synthetic_name.clone(),
            bounds: vec![bound],
            is_const: false,
            const_type: None,
            variance: Variance::Invariant,
            variance_span: None,
            is_variadic_shape: false,
            span: impl_trait_span.clone(),
        });

        let original_span = param.ty.span.clone();
        param.ty = TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![synthetic_name],
                generic_args: None,
                span: original_span.clone(),
            }),
            span: original_span,
        };
    }

    if synthetic_params.is_empty() {
        return;
    }

    match &mut f.generic_params {
        Some(existing) => existing.params.extend(synthetic_params),
        None => {
            let span = synthetic_params
                .first()
                .map(|p| p.span.clone())
                .unwrap_or_else(|| Span {
                    line: 0,
                    column: 0,
                    offset: 0,
                    length: 0,
                });
            f.generic_params = Some(GenericParams {
                params: synthetic_params,
                effect_params: Vec::new(),
                span,
            });
        }
    }
}
