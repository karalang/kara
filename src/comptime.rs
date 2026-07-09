//! Comptime fold pass — the compile-time evaluator (substrate 1).
//!
//! This is the first of the four comptime substrates from
//! [`docs/deferred.md` § Comptime — "Implementation phases"]: a treewalk
//! evaluator over the typed AST that runs `comptime { ... }` blocks at
//! compile time and splices their constant results back into the program.
//! As the spec puts it, the evaluator "is essentially the existing Phase 4
//! interpreter retargeted to compile-time invocation" — so rather than
//! re-implement evaluation, this pass drives the real
//! [`crate::interpreter::Interpreter`] against a snapshot of the program
//! and folds the resulting [`Value`] into a literal [`Expr`].
//!
//! ## Pipeline position
//!
//! Runs after `lower` and before the interpreter / codegen consume the
//! program:
//!
//! ```text
//! … → typecheck → lower → [comptime fold] → effectcheck/ownership → interpret/codegen
//! ```
//!
//! Folding after `lower` means the snapshot interpreter sees exactly the
//! same (lowered) tree the real interpreter would, and downstream phases
//! see plain constants in place of every `comptime { ... }` node.
//!
//! ## Scope
//!
//! - Evaluates `comptime { ... }` block expressions anywhere an expression
//!   can appear (function bodies, `const` initializers, impl methods, …).
//! - Folds scalar results (int / float / bool / char / string / unit) plus
//!   homogeneous compound results (tuples, arrays / vecs) into literals.
//! - Splices a generated `Expr` (`ast.expr(...)`, substrate 3) at the
//!   comptime site instead of folding a constant.
//! - Expands `#[derive(X)]` (substrate 4): each derive whose `derive_x`
//!   comptime fn exists is invoked as `derive_x(T)` and the `Vec[Item]` it
//!   returns is spliced into the module after the derive site. See
//!   [`Folder::expand_derives`].
//! - Surfaces a comptime panic as a compile error
//!   (`E_COMPTIME_PANIC`), a non-terminating evaluation as
//!   `E_COMPTIME_ITER_LIMIT_EXCEEDED` (enforced via a wall-clock guard on
//!   the shared interpreter deadline hook), and a result shape that can't
//!   be expressed as a literal as `E_COMPTIME_NON_FOLDABLE_RESULT`.
//!
//! Because this pass runs after `resolve` / `typecheck`, derive-generated
//! items are seen by the interpreter (dynamic dispatch) but not by name
//! resolution: a generated *method* on the derived type is callable, while a
//! generated top-level item referenced *by name* elsewhere would not resolve.
//! Moving derive expansion ahead of resolution (so generated names resolve)
//! is future work; the effect restriction (`E_RUNTIME_EFFECT_AT_COMPTIME`)
//! is a later substrate and is not part of this pass.

use std::time::{Duration, Instant};

use crate::ast::*;
use crate::interpreter::{Interpreter, Value};
use crate::resolver::SpanKey;
use crate::token::{FloatSuffix, IntSuffix, Span};
use crate::typechecker::{FloatSize, IntSize, Type, TypeCheckResult, UIntSize};

/// Wall-clock ceiling for a single `comptime { ... }` evaluation. The
/// language spec mandates an instruction ceiling (`2^24` per top-level
/// invocation, `--comptime-iter-limit`); slice 2 approximates it with a
/// wall-clock deadline routed through the interpreter's existing
/// statement-boundary deadline poll (`set_test_deadline`). A real
/// instruction counter is future work; the deadline is the safety net that
/// keeps a runaway `comptime` loop from hanging the compiler.
const COMPTIME_WALL_CLOCK_LIMIT: Duration = Duration::from_secs(5);

/// A diagnostic produced by the comptime fold pass. Mirrors the
/// span+message shape of the other phases' error records so the CLI can
/// render and count them alongside typecheck / effect / ownership errors.
#[derive(Debug, Clone)]
pub struct ComptimeError {
    pub message: String,
    pub span: Span,
}

/// Evaluate every `comptime { ... }` block in `program` at compile time and
/// replace each with its folded constant, in place. Returns the
/// diagnostics produced (empty on success). `typed` is the typecheck result
/// for `program`; it supplies the result type used to pick literal suffixes
/// and to distinguish `Vec`-typed from `Array`-typed collection results.
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
/// To evaluate the parser it needs an interpreter, which needs a typecheck
/// context — so it runs `resolve` + `typecheck` on the current program
/// internally. Forward references to the not-yet-generated message types make
/// those passes report (non-fatal) errors, which is fine: the parser comptime fn
/// only touches string literals + `ast.item`, never the user types. After
/// splicing, the outer pipeline resolves/typechecks the expanded program
/// cleanly. No `#[proto_schema]` const ⇒ this is a cheap scan and a no-op.
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

    // Build an evaluation context. `resolve`/`typecheck` here see the forward
    // references to the soon-to-be-generated types and report errors; those are
    // discarded — only the schema parser runs, and it needs none of that info.
    let resolved = crate::resolve(program);
    let typed = crate::typecheck(program, &resolved);
    let snapshot = program.clone();
    let mut interp = Interpreter::new(&snapshot, &typed);
    interp.register_items();
    interp.captured_output = Some(Vec::new());
    let mut folder = Folder {
        interp,
        typed: &typed,
        errors: Vec::new(),
    };

    // Evaluate each schema, then splice last-to-first so earlier indices stay
    // valid. Each `#[proto_schema]` const is replaced by the items it expands to.
    let mut planned_items: Vec<(usize, Vec<Item>)> = Vec::new();
    for (idx, value) in planned_consts {
        let site = value.span.clone();
        if let Some(items) = folder.eval_schema_const(&value, &site) {
            planned_items.push((idx, items));
        } else {
            // On error, drop the const but emit nothing — the recorded
            // diagnostic carries the reason.
            planned_items.push((idx, Vec::new()));
        }
    }
    let errors = folder.errors;
    for (idx, items) in planned_items.into_iter().rev() {
        program.items.remove(idx);
        for (k, it) in items.into_iter().enumerate() {
            program.items.insert(idx + k, it);
        }
    }
    errors
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

pub fn evaluate(program: &mut Program, typed: &TypeCheckResult) -> Vec<ComptimeError> {
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

    // The interpreter borrows its program immutably for its whole lifetime,
    // but this pass mutates `program` to splice in folded constants. Resolve
    // the conflict by evaluating against an independent snapshot: the
    // interpreter borrows `snapshot`, while the mutable walk rewrites
    // `program`. Spans are preserved by `Clone`, so a node found in `program`
    // matches its twin in `snapshot` for any span-keyed lookup the
    // interpreter performs.
    let snapshot = program.clone();
    let mut interp = Interpreter::new(&snapshot, typed);
    // Prime the global environment exactly as `run()` does (prelude variants,
    // baked stdlib impls, user items / functions) — but without calling
    // `main()`. Comptime fn calls resolve through these registrations.
    interp.register_items();
    // Capture (and discard) any output a comptime block produces so stray
    // prints don't leak into the build log. `compiler.print` is a later
    // substrate; until then this just keeps the channel quiet.
    interp.captured_output = Some(Vec::new());

    let mut folder = Folder {
        interp,
        typed,
        errors: Vec::new(),
    };
    // Pass 1: fold `comptime { ... }` blocks in existing item bodies.
    if needs_fold {
        for item in &mut program.items {
            folder.fold_item(item);
        }
    }
    // Pass 2: derive desugaring — expand each `#[derive(X)]` whose `derive_x`
    // comptime fn exists into the items that fn returns, spliced after the
    // derive site. Spec: deferred.md § Comptime — Code generation and derive
    // desugaring.
    if needs_derive {
        folder.expand_derives(program, &derive_fns);
    }
    folder.errors
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
        ExprKind::MapLiteral(pairs) => {
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
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
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

// ── Mutating fold walker ────────────────────────────────────────

struct Folder<'a> {
    interp: Interpreter<'a>,
    typed: &'a TypeCheckResult,
    errors: Vec<ComptimeError>,
}

impl Folder<'_> {
    fn fold_item(&mut self, item: &mut Item) {
        match item {
            Item::Function(f) => self.fold_block(&mut f.body),
            Item::ImplBlock(imp) => {
                for it in &mut imp.items {
                    if let ImplItem::Method(m) = it {
                        self.fold_block(&mut m.body);
                    }
                }
            }
            Item::ConstDecl(c) => self.fold_expr(&mut c.value),
            _ => {}
        }
    }

    fn fold_block(&mut self, block: &mut Block) {
        for stmt in &mut block.stmts {
            self.fold_stmt(stmt);
        }
        if let Some(e) = &mut block.final_expr {
            self.fold_expr(e);
        }
    }

    fn fold_stmt(&mut self, stmt: &mut Stmt) {
        match &mut stmt.kind {
            StmtKind::Let { value, .. } => self.fold_expr(value),
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                self.fold_expr(value);
                self.fold_block(else_block);
            }
            StmtKind::LetUninit { .. } | StmtKind::MultiAssign { .. } => {}
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => self.fold_block(body),
            StmtKind::Assign { target, value } => {
                self.fold_expr(target);
                self.fold_expr(value);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.fold_expr(target);
                self.fold_expr(value);
            }
            StmtKind::Expr(e) => self.fold_expr(e),
        }
    }

    fn fold_expr(&mut self, expr: &mut Expr) {
        // A `comptime { ... }` node is evaluated whole — the snapshot
        // interpreter handles any nesting inside it (including nested
        // `comptime` blocks), so we do NOT pre-recurse into its children.
        if matches!(expr.kind, ExprKind::Comptime(_)) {
            self.eval_and_splice(expr);
            return;
        }
        // Every other node: recurse into children so comptime nodes buried
        // anywhere in the tree get folded.
        match &mut expr.kind {
            ExprKind::Binary { left, right, .. } | ExprKind::NilCoalesce { left, right } => {
                self.fold_expr(left);
                self.fold_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.fold_expr(operand),
            ExprKind::Question(inner) | ExprKind::Cast { expr: inner, .. } => self.fold_expr(inner),
            ExprKind::OptionalChain { object, args, .. } => {
                self.fold_expr(object);
                if let Some(args) = args {
                    for a in args {
                        self.fold_expr(&mut a.value);
                    }
                }
            }
            ExprKind::Call { callee, args } => {
                self.fold_expr(callee);
                for a in args {
                    self.fold_expr(&mut a.value);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                self.fold_expr(object);
                for a in args {
                    self.fold_expr(&mut a.value);
                }
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.fold_expr(object)
            }
            ExprKind::Index { object, index } => {
                self.fold_expr(object);
                self.fold_expr(index);
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b)
            | ExprKind::LabeledBlock { body: b, .. }
            | ExprKind::Loop { body: b, .. } => self.fold_block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.fold_expr(condition);
                self.fold_block(then_block);
                if let Some(eb) = else_branch {
                    self.fold_expr(eb);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.fold_expr(value);
                self.fold_block(then_block);
                if let Some(eb) = else_branch {
                    self.fold_expr(eb);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.fold_expr(scrutinee);
                for arm in arms {
                    if let Some(g) = &mut arm.guard {
                        self.fold_expr(g);
                    }
                    self.fold_expr(&mut arm.body);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.fold_expr(condition);
                self.fold_block(body);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.fold_expr(value);
                self.fold_block(body);
            }
            ExprKind::For { iterable, body, .. } => {
                self.fold_expr(iterable);
                self.fold_block(body);
            }
            ExprKind::Closure { body, .. } => self.fold_expr(body),
            ExprKind::Return(opt) | ExprKind::Break { value: opt, .. } => {
                if let Some(e) = opt {
                    self.fold_expr(e);
                }
            }
            ExprKind::Tuple(es) | ExprKind::ArrayLiteral(es) => {
                for e in es {
                    self.fold_expr(e);
                }
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.fold_expr(e);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.fold_expr(value);
                self.fold_expr(count);
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.fold_expr(k);
                    self.fold_expr(v);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for fi in fields {
                    self.fold_expr(&mut fi.value);
                }
                if let Some(s) = spread {
                    self.fold_expr(s);
                }
            }
            ExprKind::Pipe { left, right } => {
                self.fold_expr(left);
                self.fold_expr(right);
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.fold_expr(s);
                }
                if let Some(e) = end {
                    self.fold_expr(e);
                }
            }
            ExprKind::Lock { mutex, body, .. } => {
                self.fold_expr(mutex);
                self.fold_block(body);
            }
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.fold_expr(&mut b.value);
                }
                self.fold_block(body);
            }
            // `Comptime` handled above; leaf nodes have no children.
            ExprKind::Comptime(_)
            | ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
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

    /// Evaluate a `comptime { ... }` node and replace it with the folded
    /// constant literal, recording a diagnostic on any failure.
    fn eval_and_splice(&mut self, expr: &mut Expr) {
        let span = expr.span.clone();
        let block = match &expr.kind {
            ExprKind::Comptime(b) => b.clone(),
            _ => return,
        };

        // Evaluate the inner block via the snapshot interpreter, wrapped as a
        // plain block expression (the interpreter has no separate comptime
        // entry point — same eval path the defensive `Comptime` arm uses).
        let wrapped = Expr {
            kind: ExprKind::Block(block),
            span: span.clone(),
        };

        // Reset per-evaluation interpreter state and arm the wall-clock guard.
        self.interp.pending_cf = None;
        let errors_before = self.interp.runtime_errors.len();
        let user_errors_before = self.interp.comptime_user_errors.len();
        self.interp.timed_out = false;
        self.interp
            .set_test_deadline(Some(Instant::now() + COMPTIME_WALL_CLOCK_LIMIT));

        let value = self.interp.eval_expr(&wrapped);

        self.interp.set_test_deadline(None);

        // Drain any `compiler.error(msg)` diagnostics emitted during this
        // block's evaluation (substrate 3 — compile-time validation). These
        // are non-halting: collect them and continue to the splice/fold below
        // so a block that reports several issues surfaces all of them.
        for diag in self
            .interp
            .comptime_user_errors
            .split_off(user_errors_before)
        {
            self.errors.push(ComptimeError {
                message: format!("error[E_COMPTIME_ERROR]: {}", diag.message),
                span: diag.span,
            });
        }

        // Runaway evaluation hit the wall-clock guard.
        if self.interp.timed_out {
            self.errors.push(ComptimeError {
                message: format!(
                    "error[E_COMPTIME_ITER_LIMIT_EXCEEDED]: `comptime` evaluation did not \
                     terminate within {}s; a `comptime` block must compute a constant in \
                     bounded time (deferred.md § Comptime — Resource limits)",
                    COMPTIME_WALL_CLOCK_LIMIT.as_secs()
                ),
                span,
            });
            return;
        }

        // A comptime panic is a compile error, not a runtime panic — the
        // diagnostic surfaces at the calling site (deferred.md § Comptime —
        // Effect system integration: `panics`).
        if self.interp.runtime_errors.len() > errors_before {
            let detail = self
                .interp
                .runtime_errors
                .last()
                .map(|e| e.message.clone())
                .unwrap_or_else(|| "comptime evaluation failed".to_string());
            self.errors.push(ComptimeError {
                message: format!(
                    "error[E_COMPTIME_PANIC]: `comptime` evaluation panicked: {detail}"
                ),
                span,
            });
            return;
        }

        // Code generation (substrate 3): when the block yields an `Expr` AST
        // value (a quasi-quote `ast.expr(...)`), splice the *generated code*
        // at the comptime site rather than folding a constant. The spliced
        // expression is then evaluated/compiled in the surrounding scope, so
        // it can reference runtime bindings.
        if let Value::AstExpr(generated) = value {
            *expr = *generated;
            return;
        }

        // Fold the resulting value into a literal, using the typechecked
        // result type to pick literal suffixes / collection shape.
        let result_ty = self
            .typed
            .expr_types
            .get(&SpanKey::from_span(&span))
            .cloned();
        match value_to_expr(&value, result_ty.as_ref(), &span) {
            Ok(folded) => *expr = folded,
            Err(reason) => self.errors.push(ComptimeError {
                message: format!(
                    "error[E_COMPTIME_NON_FOLDABLE_RESULT]: `comptime` block produced a value \
                     that cannot be spliced as a constant: {reason}"
                ),
                span,
            }),
        }
    }

    /// Derive desugaring (substrate 4). For every struct/enum carrying a
    /// `#[derive(X)]` whose `derive_x` comptime fn exists, evaluate
    /// `derive_x(T)` and splice the returned `Vec[Item]` into the module
    /// immediately after the derive site (source-order semantics: generated
    /// items see items declared earlier, not later). Spec: deferred.md §
    /// Comptime — Code generation and derive desugaring.
    fn expand_derives(
        &mut self,
        program: &mut Program,
        derive_fns: &std::collections::HashSet<String>,
    ) {
        // Plan first (immutable scan), splice after — so item indices stay
        // valid while we evaluate, and the snapshot interpreter never sees the
        // half-mutated program.
        let mut planned: Vec<(usize, Vec<Item>)> = Vec::new();
        for (idx, item) in program.items.iter().enumerate() {
            let (type_name, attributes, site) = match item {
                Item::StructDef(s) => (s.name.clone(), &s.attributes, s.span.clone()),
                Item::EnumDef(e) => (e.name.clone(), &e.attributes, e.span.clone()),
                _ => continue,
            };
            let traits = ordered_derived_traits(attributes);
            let mut generated: Vec<Item> = Vec::new();
            for trait_name in traits {
                let fn_name = format!("derive_{}", to_snake_case(&trait_name));
                // A derive without a backing comptime fn (the native built-ins:
                // Eq / Hash / Display / …) is left to the existing handling —
                // skip it here rather than erroring.
                if !derive_fns.contains(&fn_name) {
                    continue;
                }
                if let Some(items) = self.eval_derive_call(&fn_name, &type_name, &site) {
                    generated.extend(items);
                }
            }
            if !generated.is_empty() {
                planned.push((idx, generated));
            }
        }

        // Splice last-to-first so each insertion leaves earlier indices intact.
        for (idx, items) in planned.into_iter().rev() {
            let at = idx + 1;
            for (k, it) in items.into_iter().enumerate() {
                program.items.insert(at + k, it);
            }
        }
    }

    /// Evaluate `fn_name(type_name)` via the snapshot interpreter and extract
    /// the returned `Vec[Item]`. Records (and returns `None` on) a panic, a
    /// runaway loop, a `compiler.error`, or a non-`Vec[Item]` result. Mirrors
    /// the per-evaluation state reset / guards of [`Self::eval_and_splice`].
    fn eval_derive_call(
        &mut self,
        fn_name: &str,
        type_name: &str,
        site: &Span,
    ) -> Option<Vec<Item>> {
        // Build `derive_x(TypeName)`. The bare type-name argument evaluates to
        // a `Type` pseudovalue (substrate 2), binding the `comptime T: Type`
        // parameter. Synthesized directly rather than parsed — no string round
        // trip, and the span is the derive site from the start.
        let call = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Identifier(fn_name.to_string()),
                    span: site.clone(),
                }),
                args: vec![CallArg {
                    label: None,
                    mut_marker: false,
                    value: Expr {
                        kind: ExprKind::Identifier(type_name.to_string()),
                        span: site.clone(),
                    },
                    span: site.clone(),
                }],
            },
            span: site.clone(),
        };
        self.run_items_call(&call, &format!("derive `{fn_name}`"), site)
    }

    /// Expand one `#[proto_schema]` const: evaluate the stdlib
    /// `proto_parse_schema(<schema text>)` parser on the const's value and
    /// return the message-type items it produced. `value` is the const's
    /// initializer expression (the `.proto` source string), reused verbatim as
    /// the call argument. Protobuf slice 3.
    fn eval_schema_const(&mut self, value: &Expr, site: &Span) -> Option<Vec<Item>> {
        let call = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Identifier("proto_parse_schema".to_string()),
                    span: site.clone(),
                }),
                args: vec![CallArg {
                    label: None,
                    mut_marker: false,
                    value: value.clone(),
                    span: site.clone(),
                }],
            },
            span: site.clone(),
        };
        self.run_items_call(&call, "proto schema", site)
    }

    /// Evaluate a synthesized comptime call expected to return `Vec[Item]`,
    /// applying the shared per-evaluation guards (state reset, wall-clock
    /// deadline, `compiler.error` draining, panic / timeout reporting) and
    /// interpreting the result via [`Self::items_from_derive_value`]. `what`
    /// names the construct for diagnostics (e.g. ``derive `derive_x` `` or
    /// `proto schema`). Shared by derive expansion and `#[proto_schema]`.
    fn run_items_call(&mut self, call: &Expr, what: &str, site: &Span) -> Option<Vec<Item>> {
        self.interp.pending_cf = None;
        let errors_before = self.interp.runtime_errors.len();
        let user_errors_before = self.interp.comptime_user_errors.len();
        self.interp.timed_out = false;
        self.interp
            .set_test_deadline(Some(Instant::now() + COMPTIME_WALL_CLOCK_LIMIT));

        let value = self.interp.eval_expr(call);

        self.interp.set_test_deadline(None);

        // Drain `compiler.error(msg)` diagnostics raised while it ran.
        for diag in self
            .interp
            .comptime_user_errors
            .split_off(user_errors_before)
        {
            self.errors.push(ComptimeError {
                message: format!("error[E_COMPTIME_ERROR]: {}", diag.message),
                span: diag.span,
            });
        }

        if self.interp.timed_out {
            self.errors.push(ComptimeError {
                message: format!(
                    "error[E_COMPTIME_ITER_LIMIT_EXCEEDED]: {what} did not terminate \
                     within {}s (deferred.md § Comptime — Resource limits)",
                    COMPTIME_WALL_CLOCK_LIMIT.as_secs()
                ),
                span: site.clone(),
            });
            return None;
        }

        if self.interp.runtime_errors.len() > errors_before {
            let detail = self
                .interp
                .runtime_errors
                .last()
                .map(|e| e.message.clone())
                .unwrap_or_else(|| "evaluation failed".to_string());
            self.errors.push(ComptimeError {
                message: format!("error[E_COMPTIME_PANIC]: {what} panicked: {detail}"),
                span: site.clone(),
            });
            return None;
        }

        self.items_from_derive_value(value, what, site)
    }

    /// Interpret an item-emitting comptime call's return value as a list of
    /// items. Accepts an array/`Vec` of `AstItem`s (the `vec![ast.item(...)]`
    /// form) or a bare single `AstItem`. Anything else is `E_COMPTIME_ERROR`.
    /// `what` names the construct for diagnostics (derive or proto schema).
    fn items_from_derive_value(
        &mut self,
        value: Value,
        what: &str,
        site: &Span,
    ) -> Option<Vec<Item>> {
        let mut out: Vec<Item> = Vec::new();
        let elements: Vec<Value> = match value {
            Value::AstItem(it) => return Some(vec![*it]),
            Value::Array(rc) => rc.read().unwrap().clone(),
            other => {
                self.errors.push(ComptimeError {
                    message: format!(
                        "error[E_COMPTIME_ERROR]: {what} must return `Vec[Item]` (a \
                         list of `ast.item(...)` values), got `{}`",
                        other.variant_name()
                    ),
                    span: site.clone(),
                });
                return None;
            }
        };
        for v in elements {
            match v {
                Value::AstItem(it) => out.push(*it),
                other => {
                    self.errors.push(ComptimeError {
                        message: format!(
                            "error[E_COMPTIME_ERROR]: {what} returned a `Vec` element \
                             that is not an `Item` (expected `ast.item(...)`), got `{}`",
                            other.variant_name()
                        ),
                        span: site.clone(),
                    });
                    return None;
                }
            }
        }
        Some(out)
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

/// Convert an evaluated [`Value`] into a constant [`Expr`] carrying `span`.
/// `ty` is the typechecked result type of the `comptime` block (when
/// known) — it drives literal-suffix selection and the `Vec`-vs-`Array`
/// collection-literal choice. Returns `Err(reason)` for value shapes that
/// have no constant literal form in slice 2.
fn value_to_expr(value: &Value, ty: Option<&Type>, span: &Span) -> Result<Expr, String> {
    let kind = match value {
        Value::Int(n) => ExprKind::Integer(*n, int_suffix(ty)),
        Value::Float(f) => ExprKind::Float(*f, float_suffix(ty)),
        Value::Bool(b) => ExprKind::Bool(*b),
        Value::Char(c) => ExprKind::CharLit(*c),
        Value::String(s) => ExprKind::StringLit(s.clone()),
        Value::Unit => ExprKind::Tuple(Vec::new()),
        Value::Tuple(items) => {
            let elem_tys = tuple_element_types(ty);
            let folded = items
                .iter()
                .enumerate()
                .map(|(i, v)| value_to_expr(v, elem_tys.and_then(|ts| ts.get(i)), span))
                .collect::<Result<Vec<_>, _>>()?;
            ExprKind::Tuple(folded)
        }
        Value::Array(cell) => {
            let elem_ty = element_type(ty);
            let folded = cell
                .read()
                .unwrap()
                .iter()
                .map(|v| value_to_expr(v, elem_ty.as_ref(), span))
                .collect::<Result<Vec<_>, _>>()?;
            // A `Vec`-typed result must round-trip as a heap Vec literal; a
            // fixed `Array[T, N]` result as a bare array literal. `lower`
            // has already run, so we emit the canonical post-lower shape
            // directly (Vec → prefix literal; Array → array literal).
            if matches!(ty, Some(Type::Named { name, .. }) if name == "Vec") {
                ExprKind::PrefixCollectionLiteral {
                    type_name: "Vec".to_string(),
                    items: folded,
                }
            } else {
                ExprKind::ArrayLiteral(folded)
            }
        }
        Value::Function { .. } => return Err("a function value".to_string()),
        Value::Struct { name, .. } => {
            return Err(format!(
                "a `{name}` struct value (struct folding is a later comptime substrate)"
            ))
        }
        Value::SharedStruct(_) => {
            return Err(
                "a shared-struct value (struct folding is a later comptime substrate)".to_string(),
            )
        }
        Value::EnumVariant { enum_name, .. } => {
            return Err(format!(
                "a `{enum_name}` enum value (enum folding is a later comptime substrate)"
            ))
        }
        Value::Map(_) => {
            return Err("a map value (map folding is a later comptime substrate)".into())
        }
        other => {
            return Err(format!(
                "an unsupported value shape ({}); only scalars, tuples, and arrays fold in slice 2",
                value_kind_name(other)
            ))
        }
    };
    Ok(Expr {
        kind,
        span: span.clone(),
    })
}

/// A short tag for the runtime `Value` variant, for diagnostic text.
fn value_kind_name(v: &Value) -> &'static str {
    match v {
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Bool(_) => "bool",
        Value::Char(_) => "char",
        Value::String(_) => "string",
        Value::Unit => "unit",
        Value::Tuple(_) => "tuple",
        Value::Array(_) => "array",
        Value::Map(_) => "map",
        Value::Struct { .. } => "struct",
        Value::EnumVariant { .. } => "enum",
        Value::Function { .. } => "function",
        _ => "value",
    }
}

/// Map an integer result type to its literal suffix. `usize` has no integer
/// literal suffix in the surface syntax, so it folds to a suffixless literal
/// (the type is recovered from context downstream).
fn int_suffix(ty: Option<&Type>) -> Option<IntSuffix> {
    match ty? {
        Type::Int(IntSize::I8) => Some(IntSuffix::I8),
        Type::Int(IntSize::I16) => Some(IntSuffix::I16),
        Type::Int(IntSize::I32) => Some(IntSuffix::I32),
        Type::Int(IntSize::I64) => Some(IntSuffix::I64),
        Type::Int(IntSize::I128) => Some(IntSuffix::I128),
        Type::UInt(UIntSize::U8) => Some(IntSuffix::U8),
        Type::UInt(UIntSize::U16) => Some(IntSuffix::U16),
        Type::UInt(UIntSize::U32) => Some(IntSuffix::U32),
        Type::UInt(UIntSize::U64) => Some(IntSuffix::U64),
        Type::UInt(UIntSize::U128) => Some(IntSuffix::U128),
        _ => None,
    }
}

fn float_suffix(ty: Option<&Type>) -> Option<FloatSuffix> {
    match ty? {
        Type::Float(FloatSize::F32) => Some(FloatSuffix::F32),
        Type::Float(FloatSize::F64) => Some(FloatSuffix::F64),
        _ => None,
    }
}

/// Element type of an `Array[T, N]` or `Vec[T]` result type, if known.
fn element_type(ty: Option<&Type>) -> Option<Type> {
    match ty? {
        Type::Array { element, .. } => Some((**element).clone()),
        Type::Named { name, args } if name == "Vec" && args.len() == 1 => Some(args[0].clone()),
        _ => None,
    }
}

/// Element types of a tuple result type, if known.
fn tuple_element_types(ty: Option<&Type>) -> Option<&[Type]> {
    match ty? {
        Type::Tuple(elems) => Some(elems.as_slice()),
        _ => None,
    }
}
