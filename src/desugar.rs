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
    synthesize_default_impls(program);
    synthesize_trait_default_methods(program);
    propagate_codegen_hints(program);
    desugar_impl_trait_args_in_program(program);
    desugar_multi_assign_in_program(program);
}

/// Materialize trait **default method bodies** into every impl that does not
/// override them, so a default method is callable on an implementor without
/// the impl re-implementing it (B-2026-07-03-8). For `impl Tr for T` where
/// trait `Tr` declares `fn m(self) -> R { <default body> }` and the impl body
/// provides no `m`, this copies `m` (converted from its `TraitMethod` node to
/// the `Function` node an impl method carries) into the impl's items. All
/// downstream phases then see the default exactly as if the user had written
/// it in the impl — which is the one form that already worked end-to-end
/// (typecheck method resolution, `eval_method_call` dispatch, and codegen's
/// `make_impl_method_function` synthesis all key off the impl's item list).
/// `Self` in the copied body/signature resolves to the impl target through the
/// existing impl-method `Self` handling (`current_self_type` in the
/// typechecker, `rewrite_self_in_type_expr` in codegen).
///
/// Scope: only traits declared in the user program are consulted (baked
/// stdlib traits are spliced separately and carry their own default
/// machinery), and only methods with a body are candidates. Overriding impls
/// keep their own method (the `provided` guard). Runs pre-resolve so the
/// synthesized methods are visible to name resolution and every later phase.
fn synthesize_trait_default_methods(program: &mut Program) {
    use std::collections::{HashMap, HashSet};

    // trait name -> its default-bodied methods, already converted to the
    // `Function` shape an `ImplItem::Method` carries.
    let mut trait_defaults: HashMap<String, Vec<Function>> = HashMap::new();
    for item in &program.items {
        if let Item::TraitDef(t) = item {
            // Skip GENERIC traits: a default body that mentions the trait's
            // own type params (`trait Box[T] { fn twice(self) -> T { .. } }`)
            // would be copied into `impl Box[i64] for W` verbatim, where `T`
            // is out of scope — the copy needs the impl's trait-args
            // substituted through the body, which this pass does not yet do.
            // Synthesizing anyway would trade the pre-fix "no method" error
            // for a confusing "undefined type 'T'". Generic-trait defaults are
            // a tracked follow-on (B-2026-07-03-10).
            if t.generic_params.is_some() {
                continue;
            }
            for ti in &t.items {
                if let TraitItem::Method(m) = ti {
                    if m.body.is_some() {
                        trait_defaults
                            .entry(t.name.clone())
                            .or_default()
                            .push(trait_method_to_function(m, t.stdlib_origin));
                    }
                }
            }
        }
    }
    if trait_defaults.is_empty() {
        return;
    }

    for item in &mut program.items {
        let Item::ImplBlock(imp) = item else { continue };
        let Some(trait_path) = &imp.trait_name else {
            continue;
        };
        let trait_name = match trait_path.segments.last() {
            Some(n) => n.clone(),
            None => continue,
        };
        let Some(defaults) = trait_defaults.get(&trait_name) else {
            continue;
        };
        let provided: HashSet<String> = imp
            .items
            .iter()
            .filter_map(|it| match it {
                ImplItem::Method(m) => Some(m.name.clone()),
                _ => None,
            })
            .collect();
        for def_fn in defaults {
            if !provided.contains(&def_fn.name) {
                imp.items.push(ImplItem::Method(Box::new(def_fn.clone())));
            }
        }
    }
}

/// Convert a default-bodied `TraitMethod` into the `Function` node an impl
/// method carries. Mirrors the synthesis in `TypeChecker::check_trait_def`
/// but preserves the codegen-relevant markers (`unsafe`, `#[track_caller]`,
/// inline/cold/gpu hints, deprecation/unstable, attributes) so a synthesized
/// default behaves like a hand-written impl method. Only called for methods
/// whose `body` is `Some`.
fn trait_method_to_function(m: &TraitMethod, stdlib_origin: bool) -> Function {
    Function {
        span: m.span.clone(),
        attributes: m.attributes.clone(),
        doc_comment: m.doc_comment.clone(),
        is_pub: false,
        is_private: false,
        is_unsafe: m.is_unsafe,
        is_comptime: false,
        name: m.name.clone(),
        generic_params: m.generic_params.clone(),
        params: m.params.clone(),
        self_param: m.self_param.clone(),
        return_type: m.return_type.clone(),
        effects: m.effects.clone(),
        requires: m.requires.clone(),
        ensures: m.ensures.clone(),
        where_clause: m.where_clause.clone(),
        body: m.body.clone().expect("caller guards on body.is_some()"),
        stdlib_origin,
        deprecation: m.deprecation.clone(),
        unstable: m.unstable.clone(),
        is_track_caller: m.is_track_caller,
        inline_hint: m.inline_hint,
        is_cold: m.is_cold,
        is_gpu: m.is_gpu,
        lint_overrides: Vec::new(),
        profile_compat: Vec::new(),
        abi: None,
    }
}

// ── Codegen-hint trait → impl propagation ────────────────────────
//
// A codegen-hint attribute (`#[inline]` / `#[inline(always)]` /
// `#[inline(never)]` / `#[cold]`) on a trait *method declaration*
// applies to every impl of that method unless the impl carries its own
// override — last-writer-wins, paralleling `#[track_caller]` (design.md
// § Codegen Hint Attributes > "Where they may appear"). The two axes
// (inline / cold) propagate independently: an impl that sets only its
// own `#[inline(never)]` still inherits the trait's `#[cold]`.
//
// Trait resolution at this pre-resolve stage is by name only — the last
// segment of the impl's `trait_name` path matched against `TraitDef`s in
// the same program. That covers same-program trait + impl (the common
// case and the v1 floor); cross-package trait hints are not propagated
// here (additive-later, alongside cross-package IR inlining).
fn propagate_codegen_hints(program: &mut Program) {
    use std::collections::HashMap;

    // trait name → (method name → (inline_hint, is_cold)), only for
    // trait methods that actually carry a hint.
    let mut trait_hints: HashMap<String, HashMap<String, (Option<InlineHint>, bool)>> =
        HashMap::new();
    for item in &program.items {
        if let Item::TraitDef(t) = item {
            for ti in &t.items {
                if let TraitItem::Method(m) = ti {
                    if m.inline_hint.is_some() || m.is_cold {
                        trait_hints
                            .entry(t.name.clone())
                            .or_default()
                            .insert(m.name.clone(), (m.inline_hint, m.is_cold));
                    }
                }
            }
        }
    }
    if trait_hints.is_empty() {
        return;
    }

    for item in &mut program.items {
        let Item::ImplBlock(imp) = item else { continue };
        let Some(trait_path) = &imp.trait_name else {
            continue;
        };
        let Some(trait_name) = trait_path.segments.last() else {
            continue;
        };
        let Some(methods) = trait_hints.get(trait_name) else {
            continue;
        };
        for ii in &mut imp.items {
            if let ImplItem::Method(m) = ii {
                if let Some(&(hint, cold)) = methods.get(&m.name) {
                    if m.inline_hint.is_none() {
                        m.inline_hint = hint;
                    }
                    if !m.is_cold {
                        m.is_cold = cold;
                    }
                }
            }
        }
    }
}

// ── `#[derive(Default)]` → synthetic `default()` assoc fn ────────
//
// `#[derive(Default)] struct Config { ... }` does not, on its own, give
// the type a `Config.default()` associated function — the dispatch
// machinery for `Type.default()` only fires against a real `default`
// method in an impl block. This pass closes that gap by synthesizing an
// inherent impl:
//
//     impl Config { fn default() -> Config { Config { f1: <d1>, ... } } }
//
// where each field initializer `<di>` is the field type's "zero-like"
// value — `0` / `0.0` / `false` / `""` / `'\0'` for primitives, and a
// recursive `FieldType.default()` call for a nested user type that also
// carries a `default` (derive-synthesized or hand-written). Because the
// synthesized body is built entirely from ordinary struct/enum-literal
// and literal AST, every downstream phase (typecheck, interpreter,
// codegen) handles it through already-tested paths — no per-backend
// special-casing of `default`. Spec: book appendix C (`Default`):
// "calls `.default()` on each field in declaration order and constructs
// the struct. For enums, the `#[default]`-marked variant is used" — a
// `#[derive(Default)]` enum must mark exactly one field-less variant
// with `#[default]` (enforced by the typechecker's
// `validate_derive_default`); the synthesized body is `Enum.Variant`.
//
// Scope (v1 floor): primitives + nested user types. Generic types and
// container/generic-argument field types (`Vec[T]`, `Option[T]`, tuples,
// refs, arrays, …) are out of scope here — the pass declines to
// synthesize for them, and the typechecker's `validate_derive_default`
// emits the clean "field ... is not Default" diagnostic instead.
fn synthesize_default_impls(program: &mut Program) {
    use std::collections::HashSet;

    // Names that will have a callable `default` — a non-generic
    // struct/enum carrying `#[derive(Default)]`, or any type with a
    // hand-written `default` method in an impl block. A nested field of
    // such a type lowers to `FieldType.default()`; anything else is not
    // (yet) defaultable and blocks synthesis for the enclosing type.
    let mut defaultable: HashSet<String> = HashSet::new();
    for item in &program.items {
        match item {
            Item::StructDef(s) if s.generic_params.is_none() && derives_default(&s.attributes) => {
                defaultable.insert(s.name.clone());
            }
            Item::EnumDef(e) if e.generic_params.is_none() && derives_default(&e.attributes) => {
                defaultable.insert(e.name.clone());
            }
            Item::ImplBlock(imp) => {
                let provides_default = imp
                    .items
                    .iter()
                    .any(|it| matches!(it, ImplItem::Method(m) if m.name == "default"));
                if provides_default {
                    if let Some(name) = type_leaf_name(&imp.target_type) {
                        defaultable.insert(name);
                    }
                }
            }
            _ => {}
        }
    }

    // Types that already have a hand-written `default` — never
    // double-define (the user's impl wins; deriving on top is their
    // call to make, and a redundant synthesized fn would collide).
    let mut has_user_default: HashSet<String> = HashSet::new();
    for item in &program.items {
        if let Item::ImplBlock(imp) = item {
            let provides_default = imp
                .items
                .iter()
                .any(|it| matches!(it, ImplItem::Method(m) if m.name == "default"));
            if provides_default {
                if let Some(name) = type_leaf_name(&imp.target_type) {
                    has_user_default.insert(name);
                }
            }
        }
    }

    let mut synthesized: Vec<Item> = Vec::new();
    for item in &program.items {
        match item {
            Item::StructDef(s)
                if s.generic_params.is_none()
                    && derives_default(&s.attributes)
                    && !has_user_default.contains(&s.name) =>
            {
                if let Some(body) = struct_default_body(s, &defaultable) {
                    synthesized.push(make_default_impl(&s.name, body, s.span.clone()));
                }
            }
            Item::EnumDef(e)
                if e.generic_params.is_none()
                    && derives_default(&e.attributes)
                    && !has_user_default.contains(&e.name) =>
            {
                if let Some(body) = enum_default_body(e) {
                    synthesized.push(make_default_impl(&e.name, body, e.span.clone()));
                }
            }
            _ => {}
        }
    }
    program.items.extend(synthesized);
}

fn derives_default(attributes: &[Attribute]) -> bool {
    crate::typechecker::extract_derived_traits(attributes).contains("Default")
}

/// Leaf type name of a single-segment, non-generic path type — the only
/// shape `default()` synthesis recognizes. `None` for tuples, refs,
/// arrays, generic-argument types, and multi-segment paths.
fn type_leaf_name(ty: &TypeExpr) -> Option<String> {
    if let TypeKind::Path(p) = &ty.kind {
        if p.segments.len() == 1 && p.generic_args.is_none() {
            return Some(p.segments[0].clone());
        }
    }
    None
}

/// The default initializer expression for a field of type `ty`, or
/// `None` when the type is outside this pass's v1 scope (containers,
/// generics, tuples, refs, or a named type with no reachable `default`).
fn default_field_expr(
    ty: &TypeExpr,
    defaultable: &std::collections::HashSet<String>,
) -> Option<Expr> {
    let span = ty.span.clone();
    let name = type_leaf_name(ty)?;
    let kind = match name.as_str() {
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" => ExprKind::Integer(0, None),
        "f32" | "f64" => ExprKind::Float(0.0, None),
        "bool" => ExprKind::Bool(false),
        "char" => ExprKind::CharLit('\0'),
        "String" => ExprKind::StringLit(String::new()),
        other if defaultable.contains(other) => ExprKind::Call {
            callee: Box::new(Expr {
                kind: ExprKind::Path {
                    segments: vec![other.to_string(), "default".to_string()],
                    generic_args: None,
                },
                span: span.clone(),
            }),
            args: Vec::new(),
        },
        _ => return None,
    };
    Some(Expr { kind, span })
}

/// `Name { f1: <d1>, ... }` literal for a derive-Default struct, or
/// `None` when any field is out of scope.
fn struct_default_body(
    s: &StructDef,
    defaultable: &std::collections::HashSet<String>,
) -> Option<Expr> {
    let mut fields = Vec::with_capacity(s.fields.len());
    for f in &s.fields {
        let value = default_field_expr(&f.ty, defaultable)?;
        fields.push(FieldInit {
            name: f.name.clone(),
            value,
            shorthand: false,
            span: f.span.clone(),
        });
    }
    Some(Expr {
        kind: ExprKind::StructLiteral {
            path: vec![s.name.clone()],
            fields,
            spread: None,
        },
        span: s.span.clone(),
    })
}

/// Default literal for a derive-Default enum: the unique `#[default]`-
/// marked, field-less variant, lowered to `Enum.Variant`. `None` when
/// the marker rule is not satisfied (zero or multiple markers, or the
/// marked variant carries a payload) — the typechecker's
/// `validate_derive_default` emits the focused diagnostic for each of
/// those cases, so declining here just suppresses a redundant
/// synthesized impl, never a silent acceptance.
fn enum_default_body(e: &EnumDef) -> Option<Expr> {
    let mut marked = e
        .variants
        .iter()
        .filter(|v| v.attributes.iter().any(|a| a.is_bare("default")));
    let variant = marked.next()?;
    // More than one marker — ambiguous, decline (typechecker reports).
    if marked.next().is_some() {
        return None;
    }
    // The marked variant must be field-less; a payload default is a
    // typechecker error, not a synthesizable body.
    if !matches!(variant.kind, VariantKind::Unit) {
        return None;
    }
    Some(Expr {
        kind: ExprKind::Path {
            segments: vec![e.name.clone(), variant.name.clone()],
            generic_args: None,
        },
        span: e.span.clone(),
    })
}

/// Wrap a `default()` body expression in an inherent
/// `impl Name { fn default() -> Name { <body> } }`. Non-`pub` so its
/// effects are *inferred* (a `pub` fn would have to declare them, and a
/// `String`-field default touches the allocator); this matches the
/// single-program v1 scope where `Name.default()` is called in-crate.
fn make_default_impl(type_name: &str, body: Expr, span: Span) -> Item {
    let ret_ty = TypeExpr {
        kind: TypeKind::Path(PathExpr {
            segments: vec![type_name.to_string()],
            generic_args: None,
            span: span.clone(),
        }),
        span: span.clone(),
    };
    let func = Function {
        span: span.clone(),
        attributes: Vec::new(),
        doc_comment: None,
        is_pub: false,
        is_private: false,
        is_unsafe: false,
        is_comptime: false,
        name: "default".to_string(),
        generic_params: None,
        params: Vec::new(),
        self_param: None,
        return_type: Some(ret_ty.clone()),
        effects: None,
        requires: Vec::new(),
        ensures: Vec::new(),
        where_clause: None,
        body: Block {
            stmts: Vec::new(),
            final_expr: Some(Box::new(body)),
            span: span.clone(),
        },
        stdlib_origin: false,
        deprecation: None,
        unstable: None,
        is_track_caller: false,
        is_gpu: false,
        inline_hint: None,
        is_cold: false,
        lint_overrides: Vec::new(),
        profile_compat: Vec::new(),
        abi: None,
    };
    Item::ImplBlock(ImplBlock {
        span: span.clone(),
        attributes: Vec::new(),
        generic_params: None,
        trait_name: None,
        target_type: ret_ty,
        where_clause: None,
        items: vec![ImplItem::Method(Box::new(func))],
        lint_overrides: Vec::new(),
        do_not_recommend: false,
    })
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
        ExprKind::Block(b) | ExprKind::Comptime(b) => walk_block(b),
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
