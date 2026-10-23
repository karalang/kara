//! Compile-time evaluation: `comptime { .. }` blocks, `#[derive(X)]` backed
//! by a `comptime fn derive_x`, and `#[proto_schema]` constants.
//!
//! Evaluation runs on the MIR interpreter ([`evaluate_on_mir`], in
//! `comptime_mir.rs`). This module holds what the pipeline asks before it
//! evaluates (is there comptime work, are there derives to expand), the
//! pre-resolve `#[proto_schema]` expansion, and the helpers both share.
//!
//! The tree-walk evaluator this module used to drive went with the legacy
//! interpreter at M3. A program checked without the library (no
//! `library_start`) cannot evaluate: [`evaluate`] reports that instead.

use crate::ast::*;
use crate::token::Span;

#[path = "comptime_mir.rs"]
mod on_mir;
pub use on_mir::{evaluate_on_mir, STEP_LIMIT};

/// Parses `s` as one Kāra expression: an `ast.expr(s)` quote.
pub(crate) fn parse_quoted_expr(s: &str) -> Result<Expr, String> {
    let tokens = crate::tokenize(s);
    let mut parser = crate::parser::Parser::new(tokens);
    match parser.parse_expression() {
        Some(expr) if parser.errors.is_empty() => Ok(expr),
        _ => {
            let msgs: Vec<String> = parser.errors.iter().map(|e| e.message.clone()).collect();
            Err(if msgs.is_empty() {
                "not a valid expression".to_string()
            } else {
                msgs.join("; ")
            })
        }
    }
}

/// Parses `s` as one top-level Kāra item: an `ast.item(s)` quote.
pub(crate) fn parse_quoted_item(s: &str) -> Result<Item, String> {
    let tokens = crate::tokenize(s);
    let result = crate::parser::Parser::new(tokens).items_only().parse();
    if !result.errors.is_empty() {
        let msgs: Vec<String> = result.errors.iter().map(|e| e.message.clone()).collect();
        return Err(msgs.join("; "));
    }
    let mut items = result.program.items;
    match items.len() {
        1 => Ok(items.pop().unwrap()),
        0 => Err("not a valid item".to_string()),
        _ => Err("quote contains more than one item".to_string()),
    }
}

/// True iff `program` has comptime work: a `comptime { .. }` block or a
/// derive backed by a `comptime fn`.
pub fn has_comptime_work(program: &Program) -> bool {
    program_has_comptime(program) || has_derives_to_expand(program)
}
use crate::typechecker::TypeCheckResult;

/// A diagnostic produced by the comptime fold pass. Mirrors the
/// span+message shape of the other phases' error records so the CLI can
/// render and count them alongside typecheck / effect / ownership errors.
#[derive(Debug, Clone)]
pub struct ComptimeError {
    pub message: String,
    pub span: Span,
}

/// Pre-resolve expansion of `#[proto_schema]` module-level `const`s into the
/// message `struct` types their `.proto` text declares (protobuf slice 3).
///
/// Unlike [`evaluate`] (which runs after `resolve`/`typecheck` and so can only
/// emit *methods* that dispatch dynamically), this pass runs **before**
/// resolution: the structs it splices must be visible to name resolution so the
/// rest of the program can reference them. The `.proto` parser itself is
/// ordinary stdlib comptime Kāra (`proto_parse_schema` in `protobuf.kara`); this
/// function just drives it. Each emitted struct carries `#[derive(Message)]`, so
/// the later [`evaluate`] derive pass supplies its `encode`/`decode`/`merge`.
///
/// It evaluates on the MIR interpreter, which needs the library: a program
/// without it gets one error per schema.
pub fn expand_proto_schemas(program: &mut Program) -> Vec<ComptimeError> {
    // Plan the work from the un-mutated program: (item index, schema-text expr).
    let planned_consts: Vec<(usize, Expr)> = program
        .items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| match item {
            Item::ConstDecl(c) if c.attributes.iter().any(|a| a.is_bare("proto_schema")) => {
                Some((idx, c.value.clone()))
            }
            _ => None,
        })
        .collect();
    if planned_consts.is_empty() {
        return Vec::new();
    }
    // The v2 pipeline's program carries the library: its schemas run on the
    // MIR interpreter (B-2026-10-22-20).
    if program.library_start.is_some() {
        let resolved = crate::resolve_v2(program);
        let typed =
            crate::typecheck_with_pruned_library_source(program, &resolved, Default::default());
        return evaluate_on_mir(program, &typed, "", true, &|p, r| {
            crate::typecheck_with_pruned_library_source(p, r, Default::default())
        });
    }

    planned_consts
        .iter()
        .map(|(_, value)| needs_library(value.span))
        .collect()
}

/// True iff the program has at least one `#[derive(X)]` whose `derive_x`
/// comptime fn exists — i.e. [`evaluate`] will SPLICE new items into the
/// program. Those generated items (methods, impls) must be re-resolved and
/// re-typechecked BEFORE operator-lowering so their bodies get name resolution
/// and codegen's span-keyed side-tables (element types of un-annotated locals,
/// etc.); the `Pipeline::lower` reorder gates on this (B-2026-07-08-15 Layer 1).
/// Pure `comptime { … }`-block folding (no derive) does not add items and keeps
/// the original lower→fold order, so this predicate is false for it.
pub fn has_derives_to_expand(program: &Program) -> bool {
    let derive_fns = collect_derive_fns(program);
    !derive_fns.is_empty() && program_has_derive_to_expand(program, &derive_fns)
}

/// The comptime work of a program checked without the library: none, or an
/// error saying it needs the library ([`evaluate_on_mir`] does the work).
pub fn evaluate(program: &mut Program, _typed: &TypeCheckResult) -> Vec<ComptimeError> {
    // Two kinds of work drive this pass: folding `comptime { ... }` blocks
    // (substrates 1–3) and expanding `#[derive(X)]` attributes that resolve to
    // a `comptime fn derive_x` (substrate 4).
    let needs_fold = program_has_comptime(program);
    let derive_fns = collect_derive_fns(program);
    let needs_derive = !derive_fns.is_empty() && program_has_derive_to_expand(program, &derive_fns);

    // Fast path: nothing to do ⇒ no snapshot, no interpreter.
    if !needs_fold && !needs_derive {
        return Vec::new();
    }
    // The v2 pipeline's programs carry the library and evaluate on MIR
    // ([`evaluate_on_mir`]); one without it cannot evaluate (B-2026-10-22-20).
    vec![needs_library(Span::default())]
}

/// A comptime evaluation asked of a program checked without the library.
fn needs_library(span: Span) -> ComptimeError {
    ComptimeError {
        message: "comptime evaluation runs on the MIR interpreter, which needs the program \
                  checked with the library"
            .to_string(),
        span,
    }
}

/// Collect the names of every `comptime fn derive_*` a `#[derive(X)]` can
/// dispatch to (lookup convention: `#[derive(TraitName)]` →
/// `derive_<snake(TraitName)>`). Both the user program and the baked stdlib
/// are scanned — the latter is how a stdlib-provided derive such as
/// `derive_message` (for `#[derive(Message)]`) becomes available without the
/// user defining it (the interpreter registers baked comptime fns to match).
fn collect_derive_fns(program: &Program) -> std::collections::HashSet<String> {
    fn derive_fn_names<'a>(items: &'a [Item]) -> impl Iterator<Item = String> + 'a {
        items.iter().filter_map(|item| match item {
            Item::Function(f) if f.is_comptime && f.name.starts_with("derive_") => {
                Some(f.name.clone())
            }
            _ => None,
        })
    }
    derive_fn_names(&program.items)
        .chain(
            crate::prelude::STDLIB_PROGRAMS
                .iter()
                .flat_map(|(_, p)| derive_fn_names(&p.items)),
        )
        .collect()
}

/// True if any struct/enum carries a `#[derive(X)]` whose `derive_x` comptime
/// fn is present in `derive_fns`. Derives without a matching comptime fn (the
/// built-in `Eq` / `Hash` / … handled natively today) do not trigger this pass.
fn program_has_derive_to_expand(
    program: &Program,
    derive_fns: &std::collections::HashSet<String>,
) -> bool {
    program.items.iter().any(|item| {
        let attrs = match item {
            Item::StructDef(s) => &s.attributes,
            Item::EnumDef(e) => &e.attributes,
            _ => return false,
        };
        ordered_derived_traits(attrs)
            .iter()
            .any(|t| derive_fns.contains(&format!("derive_{}", to_snake_case(t))))
    })
}

/// True if any item in the program contains a `comptime { ... }` node.
fn program_has_comptime(program: &Program) -> bool {
    program.items.iter().any(item_has_comptime)
}

fn item_has_comptime(item: &Item) -> bool {
    match item {
        Item::Function(f) => block_has_comptime(&f.body),
        Item::ImplBlock(imp) => imp.items.iter().any(|it| match it {
            ImplItem::Method(m) => block_has_comptime(&m.body),
            _ => false,
        }),
        Item::ConstDecl(c) => expr_has_comptime(&c.value),
        _ => false,
    }
}

fn block_has_comptime(block: &Block) -> bool {
    block.stmts.iter().any(stmt_has_comptime)
        || block
            .final_expr
            .as_ref()
            .is_some_and(|e| expr_has_comptime(e))
}

fn stmt_has_comptime(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Let { value, .. } => expr_has_comptime(value),
        StmtKind::LetElse {
            value, else_block, ..
        } => expr_has_comptime(value) || block_has_comptime(else_block),
        StmtKind::LetUninit { .. } => false,
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => block_has_comptime(body),
        StmtKind::Assign { target, value } => expr_has_comptime(target) || expr_has_comptime(value),
        StmtKind::CompoundAssign { target, value, .. } => {
            expr_has_comptime(target) || expr_has_comptime(value)
        }
        StmtKind::Expr(e) => expr_has_comptime(e),
        StmtKind::MultiAssign { .. } => false,
    }
}

fn expr_has_comptime(expr: &Expr) -> bool {
    let mut found = false;
    walk_child_exprs(expr, &mut |e| {
        if matches!(e.kind, ExprKind::Comptime(_)) {
            found = true;
        }
        !found // keep descending only while nothing found
    });
    found
}

/// Visit `expr` and every sub-expression, calling `f` on each. The closure
/// returns `true` to continue descending into that node's children, `false`
/// to prune. Used by the read-only `expr_has_comptime` probe; the mutating
/// rewrite lives in `Folder::fold_expr`.
fn walk_child_exprs(expr: &Expr, f: &mut impl FnMut(&Expr) -> bool) {
    if !f(expr) {
        return;
    }
    macro_rules! go {
        ($e:expr) => {
            walk_child_exprs($e, f)
        };
    }
    match &expr.kind {
        ExprKind::Binary { left, right, .. } | ExprKind::NilCoalesce { left, right } => {
            go!(left);
            go!(right);
        }
        ExprKind::Unary { operand, .. } => go!(operand),
        ExprKind::Question(inner) | ExprKind::Cast { expr: inner, .. } => go!(inner),
        ExprKind::OptionalChain { object, args, .. } => {
            go!(object);
            if let Some(args) = args {
                for a in args {
                    go!(&a.value);
                }
            }
        }
        ExprKind::Call { callee, args } => {
            go!(callee);
            for a in args {
                go!(&a.value);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            go!(object);
            for a in args {
                go!(&a.value);
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => go!(object),
        ExprKind::Index { object, index } => {
            go!(object);
            go!(index);
        }
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. } => walk_child_exprs_block(b, f),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            go!(condition);
            walk_child_exprs_block(then_block, f);
            if let Some(eb) = else_branch {
                go!(eb);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            go!(value);
            walk_child_exprs_block(then_block, f);
            if let Some(eb) = else_branch {
                go!(eb);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            go!(scrutinee);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    go!(g);
                }
                go!(&arm.body);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            go!(condition);
            walk_child_exprs_block(body, f);
        }
        ExprKind::WhileLet { value, body, .. } => {
            go!(value);
            walk_child_exprs_block(body, f);
        }
        ExprKind::For { iterable, body, .. } => {
            go!(iterable);
            walk_child_exprs_block(body, f);
        }
        ExprKind::Closure { body, .. } => go!(body),
        ExprKind::Return(opt) | ExprKind::Break { value: opt, .. } => {
            if let Some(e) = opt {
                go!(e);
            }
        }
        ExprKind::Tuple(es) | ExprKind::ArrayLiteral(es) => {
            for e in es {
                go!(e);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for e in items {
                go!(e);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            go!(value);
            go!(count);
        }
        ExprKind::MapLiteral { entries: pairs, .. } => {
            for (k, v) in pairs {
                go!(k);
                go!(v);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for fi in fields {
                go!(&fi.value);
            }
            if let Some(s) = spread {
                go!(s);
            }
        }
        ExprKind::Pipe { left, right } => {
            go!(left);
            go!(right);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                go!(s);
            }
            if let Some(e) = end {
                go!(e);
            }
        }
        ExprKind::Lock { mutex, body, .. } => {
            go!(mutex);
            walk_child_exprs_block(body, f);
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                go!(&b.value);
            }
            walk_child_exprs_block(body, f);
        }
        // Leaf nodes.
        ExprKind::Integer(_, _)
        | ExprKind::Float(_, _)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::ByteStringLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::InterpolatedStringLit(_)
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
    }
}

fn walk_child_exprs_block(block: &Block, f: &mut impl FnMut(&Expr) -> bool) {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Let { value, .. } => walk_child_exprs(value, f),
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                walk_child_exprs(value, f);
                walk_child_exprs_block(else_block, f);
            }
            StmtKind::LetUninit { .. } | StmtKind::MultiAssign { .. } => {}
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                walk_child_exprs_block(body, f)
            }
            StmtKind::Assign { target, value } => {
                walk_child_exprs(target, f);
                walk_child_exprs(value, f);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                walk_child_exprs(target, f);
                walk_child_exprs(value, f);
            }
            StmtKind::Expr(e) => walk_child_exprs(e, f),
        }
    }
    if let Some(e) = &block.final_expr {
        walk_child_exprs(e, f);
    }
}

/// Trait names from a declaration's `#[derive(...)]` attributes, in source
/// order (the ordered analogue of the typechecker's `extract_derived_traits`).
/// Order is preserved so the spliced impls land deterministically.
pub(crate) fn ordered_derived_traits(attributes: &[Attribute]) -> Vec<String> {
    let mut traits = Vec::new();
    for attr in attributes {
        if !attr.is_bare("derive") {
            continue;
        }
        for arg in &attr.args {
            let name = match &arg.value {
                // `#[derive(Eq)]` — bare identifier.
                Some(Expr {
                    kind: ExprKind::Identifier(name),
                    ..
                }) => Some(name.clone()),
                // `#[derive(Display(snake_case))]` — call form; take the callee.
                Some(Expr {
                    kind: ExprKind::Call { callee, .. },
                    ..
                }) => match &callee.kind {
                    ExprKind::Identifier(name) => Some(name.clone()),
                    _ => None,
                },
                _ => None,
            };
            if let Some(name) = name {
                if !traits.contains(&name) {
                    traits.push(name);
                }
            }
        }
    }
    traits
}

/// Convert a trait name to its `derive_` fn suffix: `CamelCase` → `snake_case`.
/// `Eq` → `eq`, `PartialEq` → `partial_eq`, `JSON` → `json`. An underscore is
/// inserted before each uppercase letter that follows a lowercase letter or
/// that begins a new word after a run of uppercase letters.
pub(crate) fn to_snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            let prev_lower =
                i > 0 && (chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit());
            let next_lower = i + 1 < chars.len() && chars[i + 1].is_lowercase();
            let after_upper_run = i > 0 && chars[i - 1].is_uppercase();
            if i > 0 && (prev_lower || (after_upper_run && next_lower)) {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

// ── Value → literal Expr folding ────────────────────────────────
