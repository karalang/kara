// src/typechecker.rs

//! Type checking for the Kāra language.
//!
//! Walks the AST with resolved names, builds a type environment from
//! top-level definitions, then type-checks function bodies. Produces
//! typed expression info and diagnostics.

// The const-expression evaluator's `Result<ConstValue, ConstEvalError>`
// shape carries `Type` / `ConstValue` payloads in the error variants,
// pushing the Err variant size above clippy's default threshold. Boxing
// every error site would clutter the eval-and-emit happy path; the
// extra stack width is acceptable since const-eval Results don't fan out.
#![allow(clippy::result_large_err)]

use crate::ast::*;
use crate::resolver::{ResolveResult, SpanKey};
use crate::token::{FloatSuffix, IntSuffix, Span};
use std::collections::{HashMap, HashSet};

mod bounds;
mod closures;
mod const_eval;
mod derives;
pub mod env;
mod env_build;
mod exprs;
mod inference;
mod items;
mod lowering;
mod patterns;
mod stdlib_methods;
pub mod types;

pub use const_eval::ConstEvalError;
use const_eval::{
    binop_glyph, const_value_type, format_const_value, primitive_const_type, unaryop_glyph,
};
pub use env::{EnumInfo, FunctionSig, ImplInfo, StructInfo, TraitInfo, TypeEnv};
#[cfg(test)]
use inference::substitute_type_params;
pub use types::{
    const_arg_display, type_display, type_to_concrete_or_param_name, ConstArg, ConstVarId,
    FloatSize, IntSize, SubstValue, Type, TypeVarId, UIntSize, VariantTypeInfo,
};
#[cfg(test)]
use types::{contains_type_param, receiver_for_method_lookup, types_compatible};
use types::{integer_width_bits, is_integer, is_numeric, is_subtype};

// ── Attribute Helpers ───────────────────────────────────────────

/// Extract trait names from `#[derive(Eq, Hash, ...)]` attributes.
/// Also handles call-form args like `Display(snake_case)` — the trait name
/// (`"Display"`) is inserted regardless of arguments.
pub(super) fn extract_derived_traits(attributes: &[Attribute]) -> HashSet<String> {
    let mut traits = HashSet::new();
    for attr in attributes {
        if attr.name == "derive" {
            for arg in &attr.args {
                match &arg.value {
                    // `#[derive(Eq)]` — bare identifier
                    Some(Expr {
                        kind: ExprKind::Identifier(name),
                        ..
                    }) => {
                        traits.insert(name.clone());
                    }
                    // `#[derive(Display(snake_case))]` — call expression;
                    // extract the callee-name identifier as the trait name.
                    Some(Expr {
                        kind:
                            ExprKind::Call {
                                callee, args: _, ..
                            },
                        ..
                    }) => {
                        if let ExprKind::Identifier(name) = &callee.kind {
                            traits.insert(name.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    traits
}

/// Returns `true` when `attributes` contains `#[derive(Display(snake_case))]`.
pub(super) fn has_display_snake_case(attributes: &[Attribute]) -> bool {
    for attr in attributes {
        if attr.name == "derive" {
            for arg in &attr.args {
                if let Some(Expr {
                    kind:
                        ExprKind::Call {
                            callee,
                            args: call_args,
                            ..
                        },
                    ..
                }) = &arg.value
                {
                    if let ExprKind::Identifier(name) = &callee.kind {
                        if name == "Display" {
                            // Check for a single `snake_case` positional argument.
                            if let Some(first) = call_args.first() {
                                if let ExprKind::Identifier(flag) = &first.value.kind {
                                    if flag == "snake_case" {
                                        return true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

/// Normalize a function's inline generic-param bounds (`fn f[T: Hash + Eq]`)
/// into the where-clause representation (`WhereConstraint::TypeBound`),
/// merged with any existing where-clause constraints. Returns `None` when
/// there are no constraints from either source.
///
/// Used at FunctionSig construction (`src/typechecker.rs:6130` /
/// `src/typechecker.rs:6253`) so the call-site bound-discharge engine
/// (`discharge_type_bounds`) sees inline and where-clause bounds through
/// one uniform API.
///
/// Slice 0.a, sub-step 1 of monomorphized collections prereq
/// ([`phase-7-codegen.md`](../docs/implementation_checklist/phase-7-codegen.md)).
pub(super) fn normalize_bounds_into_where_clause(
    generic_params: &Option<GenericParams>,
    where_clause: &Option<WhereClause>,
) -> Option<WhereClause> {
    let mut constraints: Vec<WhereConstraint> = Vec::new();
    if let Some(ref gp) = generic_params {
        for param in &gp.params {
            if param.is_const || param.bounds.is_empty() {
                continue;
            }
            constraints.push(WhereConstraint::TypeBound {
                type_name: param.name.clone(),
                bounds: param.bounds.clone(),
                span: param.span.clone(),
            });
        }
    }
    if let Some(ref wc) = where_clause {
        constraints.extend(wc.constraints.iter().cloned());
    }
    if constraints.is_empty() {
        return None;
    }
    let span = where_clause
        .as_ref()
        .map(|wc| wc.span.clone())
        .or(generic_params.as_ref().map(|gp| gp.span.clone()))
        .unwrap_or(Span {
            line: 0,
            column: 0,
            offset: 0,
            length: 0,
        });
    Some(WhereClause { constraints, span })
}

/// Mode for `closure_consumes_captured_non_copy`'s body walk: tracks
/// whether the current position is a Reading or Consuming context.
/// Mirrors `use_classifier::Mode` so the typechecker's capture-consume
/// detection lines up with the legacy ownership-side detector. Round
/// 12.44 (Step 2 — once-callability inference at construction).
#[derive(Copy, Clone, Eq, PartialEq)]
pub(super) enum CaptureWalkMode {
    Reading,
    Consuming,
}

pub(super) struct LocalTypeScope {
    pub(super) scopes: Vec<HashMap<String, Type>>,
}

impl LocalTypeScope {
    pub(super) fn new() -> Self {
        LocalTypeScope {
            scopes: vec![HashMap::new()],
        }
    }

    pub(super) fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }

    pub(super) fn pop(&mut self) {
        self.scopes.pop();
    }

    pub(super) fn insert(&mut self, name: String, ty: Type) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, ty);
        }
    }

    pub(super) fn lookup(&self, name: &str) -> Option<&Type> {
        for scope in self.scopes.iter().rev() {
            if let Some(ty) = scope.get(name) {
                return Some(ty);
            }
        }
        None
    }
}

// ── Errors ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TypeError {
    pub message: String,
    pub span: Span,
    pub kind: TypeErrorKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeErrorKind {
    TypeMismatch,
    UndefinedField,
    WrongNumberOfArgs,
    MissingField,
    ExtraField,
    NonExhaustiveMatch,
    NotCallable,
    NotAStruct,
    InvalidBinaryOp,
    InvalidUnaryOp,
    InvalidCast,
    ConditionNotBool,
    BranchTypeMismatch,
    ReturnTypeMismatch,
    InvalidTupleIndex,
    LabelMismatch,
    NonContiguousLabels,
    InvalidPipePlaceholder,
    /// Call-site `mut` marker required but not written on a fresh binding
    /// passed to a `mut ref T` / `mut Slice[T]` parameter (design.md Part 1½).
    MissingMutMarker,
    /// Call-site `mut` marker written where it is not legal: either the
    /// parameter is not mutating, or the argument is already a mut-ref
    /// (e.g., forwarded binding, nested mut-ref return).
    InvalidMutMarker,
    /// 128-bit integer literal used (`123i128`, `0u128`). 128-bit integer
    /// types are not yet supported by the type system or codegen.
    UnsupportedNumericSuffix,
    /// A non-`pub` type appears in a `pub` signature position
    /// (function parameter/return, public struct field, public enum variant
    /// payload, public type alias, public constant). See design.md
    /// § Struct Field Visibility and § Three-level visibility. (CR-18.)
    PrivateTypeInPublicSignature,
    /// A refutable pattern (one that may not match all values) appears where
    /// only irrefutable patterns are allowed — function parameters, closure
    /// parameters, `let` bindings. Use `if let` or `match` for refutable cases.
    RefutablePattern,
    /// `impl Foo for T` is missing a required `impl Bar for T` where `Bar` is
    /// a supertrait of `Foo`. See design.md § Trait Constraints (Supertraits).
    MissingSupertrait,
    /// A type argument does not satisfy the required trait bound (e.g. T in
    /// `SortedSet[T]` must implement `Ord`; K in `Map[K, V]` must implement
    /// `Hash + Eq`).
    TraitBoundNotSatisfied,
    /// `T.method(...)` where T is a generic type parameter and two or more of
    /// its bound traits declare an associated function with that name. The
    /// programmer must use UFCS `Trait.method(...)` to disambiguate.
    AmbiguousAssocFn,
    /// `e.method(args)` where two or more user-impl candidates of the same
    /// priority tier survive method resolution on the receiver's type
    /// (typically two trait impls when no inherent matches; the
    /// inherent-beats-trait priority filter eliminates inherent-vs-trait
    /// ambiguity). The programmer must use UFCS `Trait.method(receiver, ...)`
    /// to disambiguate. Distinct from `AmbiguousAssocFn`, which targets the
    /// type-prefixed `T.method(...)` form on a generic type parameter.
    /// Slice 3 of the method-resolution CR — see
    /// `phase-4-interpreter.md` § "TypeChecker: implement full method
    /// resolution algorithm" item 4.
    AmbiguousMethod,
    /// Bare `method(args)` call appears in a synthesis position (no expected
    /// type) where the only candidate resolutions are trait associated
    /// functions. The typechecker cannot infer the target type — programmer
    /// must add a type annotation or use type-prefixed `T.method(...)`.
    CannotInferAssocFn,
    /// A once-callable closure (`OnceFn(...)` value, or a closure literal
    /// whose body consumes a captured owned non-Copy binding) is being
    /// assigned to a slot whose type is `Fn(...)` or `ref Fn(...)`. The slot
    /// promises repeatable invocation; the closure can only be called once.
    /// Round 12.45 (Step 3) — caller-side rejection of `OnceFn` at `Fn` /
    /// `ref Fn` parameter slots and any other Fn-shaped assignment boundary.
    OnceFnIntoFnSlot,
    /// `e.m(args)` where no candidate at any receiver level resolves to a
    /// method named `m`. Carries an optional `did you mean 'm2'?` tail when
    /// an edit-distance-≤2 candidate exists on the receiver type's impls.
    /// Method-resolution Step 7 — see phase-4-interpreter.md § TypeChecker:
    /// implement full method resolution algorithm.
    NoMethodFound,
    /// A match arm pattern is fully covered by an earlier (unguarded) arm,
    /// so its body can never execute. Emitted as a warning, not an error —
    /// codegen retains the arm. Reachability slice of the Maranget
    /// exhaustiveness upgrade (step 6).
    UnreachableArm,
    /// A generic call's return type contains a `TypeParam(T)` that no
    /// argument or expected-type context pinned. Today the permissive
    /// `TypeParam` arm of `types_compatible` lets these silently flow
    /// through; this diagnostic surfaces them at the consuming context
    /// (currently: synthesis-mode `let` bindings without an annotation).
    /// Item 131 sub-step 2a.
    CannotInferTypeParam,
    /// Two impls would coexist on the same `(trait_name, target_type)`
    /// where one is generic-on-name (`impl Foo for Bar[T]`) and the other
    /// is specialized to a concrete instantiation (`impl Foo for
    /// Bar[i32]`), or both are specialized to the same concrete
    /// instantiation. v1 rejects the overlap at impl registration time
    /// rather than picking a winner at the call site (Rust-style
    /// specialization is post-v1). Theme-4 slice — see
    /// `phase-4-interpreter.md` § `impl Option[Ordering]` deferred entry.
    ConflictingImpl,
}

impl std::fmt::Display for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: {}",
            self.span.line, self.span.column, self.message
        )
    }
}

// ── Result ──────────────────────────────────────────────────────

pub struct TypeCheckResult {
    pub errors: Vec<TypeError>,
    /// Non-fatal diagnostics: typecheck-time signals that don't block
    /// later phases. Currently carries `UnreachableArm` from the Maranget
    /// reachability pass; future signals belong here too.
    pub warnings: Vec<TypeError>,
    pub expr_types: HashMap<SpanKey, Type>,
    pub struct_info: HashMap<String, StructInfo>,
    pub enum_info: HashMap<String, EnumInfo>,
    /// Derived traits for each `distinct type` declaration.
    pub distinct_type_traits: HashMap<String, HashSet<String>>,
    /// For each `?` expression that requires cross-error-type conversion via
    /// `From`, the target error type's name. Interpreter / codegen consult
    /// this side-table to know whether to call `<target>.from(err)` on the
    /// propagated Err value.
    pub question_conversions: HashMap<SpanKey, String>,
    /// `(trait_name, target_type_name)` pairs for every impl registered at
    /// typecheck time. The lowering pass consults this to decide whether a
    /// non-primitive operand has an applicable trait impl (e.g. user
    /// `impl Eq for MyStruct` drives `==` dispatch).
    pub trait_impls: std::collections::HashSet<(String, String)>,
    /// For each `x.into()` call resolved against an expected type, the target
    /// type's name. Lowering rewrites these to `Target.from(x)` — the `Into`
    /// blanket impl is not materialized in `env.impls`, it's purely a lowering
    /// rewrite backed by the `From` dispatch already in place.
    pub into_conversions: HashMap<SpanKey, String>,
    /// For each `x.try_into()` call resolved against an expected `Result[T, E]`,
    /// the target type's name (the `T` inside the Result). Lowering rewrites
    /// these to `Target.try_from(x)` — same desugar architecture as `into`.
    pub try_into_conversions: HashMap<SpanKey, String>,
    /// Enum names that derive `Display(snake_case)`. The interpreter uses
    /// this to convert variant names to `lower_snake_case` in `to_string()`.
    pub display_snake_case_enums: HashSet<String>,
    /// For each `MethodCall` expression, the canonical `Type.method` callee
    /// key — the same shape used in `EffectCheckResult.{inferred,declared}_effects`
    /// and in `Program.callee_effectful`. Lowering forwards this to
    /// `Program.method_callee_types` so codegen can narrow the par-branch
    /// cooperative-cancel check at instance method sites.
    ///
    /// Populated alongside the receiver-type dispatch in `infer_method_call`.
    /// Keyed by call-expression `SpanKey` (note: the parser sets
    /// `MethodCall.span == receiver.span`, so callers must not reuse
    /// `expr_types` for this purpose — a separate map avoids the
    /// return-type-overwrites-receiver-type race).
    pub method_callee_types: HashMap<SpanKey, String>,
    /// Bare-call dispatch resolutions: span of a `Call(Identifier(name))` →
    /// resolved target type name (e.g. `"Wrapper"`). Populated when expected-
    /// type inference resolves a bare associated-function call to a concrete
    /// type. Lowering rewrites the call to `Target.name(args)` so the
    /// interpreter / codegen dispatches via the existing impl table without
    /// further special-casing.
    pub bare_assoc_fn_targets: HashMap<SpanKey, String>,
    /// Per-call-site generic-param substitutions: call-expression span → name
    /// → resolved type name. Concrete entries (`"Wrapper"`) come from the
    /// typechecker's solver; abstract entries (`"T"`) propagate the caller's
    /// generic binding and are resolved against the runtime substitution
    /// stack at execution time. Consumed by the interpreter to dispatch
    /// `T.method()` calls inside generic function bodies.
    pub call_type_subs: HashMap<SpanKey, HashMap<String, String>>,
    /// For each pattern-binding name introduced by `bind_pattern_types`, the
    /// canonical type name (e.g. `"MyError"`). Keyed by the pattern's span.
    /// Used by codegen to reconstitute struct payloads from the i64 word
    /// when binding match-arm variables: `Err(e)` where the variant payload
    /// is a struct, `e` is bound as i64 by the enum-payload codegen, and
    /// codegen uses this table to know the surface type of `e` so
    /// `e.field` field access can dispatch through the right struct shape.
    /// Only `Type::Named` types are recorded (primitives, refs, etc. don't
    /// need the reconstruction step).
    pub pattern_binding_types: HashMap<SpanKey, String>,
    /// Sibling table to `pattern_binding_types` carrying the inner element
    /// `TypeExpr` for `Vec[T]` / `Slice[T]` pattern bindings only. Keyed by
    /// the same `SpanKey` (the pattern's span). Populated alongside the
    /// String-name entry in `bind_pattern_types` / `check_pattern_against`
    /// when the surface type is `Vec[T]` or `Slice[T]`. Consumed by codegen
    /// at `bind_pattern_values` to populate `vec_elem_types` /
    /// `slice_elem_types` keyed by the binding's variable name, so direct
    /// method dispatch on a pattern-bound `Vec` / `Slice` payload (`xs.len()`,
    /// `xs[0]`, `xs.push(...)`) routes through the right element-typed path
    /// without going through function-arg routing as a work-around. Empty
    /// for non-collection bindings (the existing String-name table is
    /// sufficient for those). PB sibling slice (2026-05-09).
    pub pattern_binding_inner_types: HashMap<SpanKey, TypeExpr>,
    /// Per-leaf-binding borrow mode under a `ref` / `mut ref` scrutinee.
    /// Keyed by the leaf binding pattern's span (or, for struct shorthand
    /// fields without a sub-pattern, the field's span). Owned bindings are
    /// absent. Forwarded to `Program.pattern_binding_borrow_modes` by the
    /// lowering pass and consumed by codegen at `bind_pattern_values` to
    /// emit the ref-binding shim — see `ast::PatternBindingBorrow`.
    pub pattern_binding_borrow_modes: HashMap<SpanKey, crate::ast::PatternBindingBorrow>,
    /// Names of functions declared with `#[compiler_builtin]` (CR-202
    /// slice 2). The signature lives in `env.functions`; the entry here
    /// flags the function as having its body replaced by Rust dispatch.
    /// Empty in user-only programs (slice 1's resolver gate `E0237`
    /// prevents the attribute outside stdlib source).
    pub compiler_builtins: HashSet<String>,
}

// ── Cross-module visibility helpers (CR-24 slice 6) ─────────────

/// Return the declared `Visibility` of a top-level item named `name` inside
/// `module`. Returns `None` when the item does not exist or is not a kind
/// that carries top-level visibility (impl blocks, layouts, etc).
pub(super) fn find_item_visibility(
    module: &crate::module::Module,
    name: &str,
) -> Option<Visibility> {
    for item in &module.items {
        match item {
            Item::Function(f) if f.name == name => return Some(f.visibility()),
            Item::StructDef(s) if s.name == name => return Some(s.visibility()),
            Item::EnumDef(e) if e.name == name => return Some(e.visibility()),
            Item::TraitDef(t) if t.name == name => return Some(t.visibility()),
            Item::ConstDecl(c) if c.name == name => return Some(c.visibility()),
            Item::TypeAlias(t) if t.name == name => return Some(t.visibility()),
            Item::DistinctType(d) if d.name == name => return Some(d.visibility()),
            Item::ExternFunction(e) if e.name == name => return Some(e.visibility()),
            Item::ExternBlock(b) => {
                for it in &b.items {
                    match it {
                        ExternItem::Function(f) if f.name == name => {
                            return Some(f.visibility());
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Find the `StructDef` for a top-level struct named `name` in `module`, if
/// any. Used by `infer_field_access` to enforce cross-module field visibility.
fn find_struct_def<'m>(module: &'m crate::module::Module, name: &str) -> Option<&'m StructDef> {
    for item in &module.items {
        if let Item::StructDef(s) = item {
            if s.name == name {
                return Some(s);
            }
        }
    }
    None
}

// ── Type Checker ────────────────────────────────────────────────

pub struct TypeChecker<'a> {
    pub(super) program: &'a Program,
    pub(super) resolve_result: &'a ResolveResult,
    /// Optional project-wide tree for cross-module checks (CR-24 slice 6b):
    /// extends `E0221 PrivateTypeInPublicSignature` to imported types and
    /// turns on field-access rejection for cross-module struct fields.
    pub(super) tree: Option<&'a crate::module::ProgramTree>,
    /// The id of the module being typechecked, when `tree` is set. Used to
    /// scope cross-module visibility checks — an access is "cross-module"
    /// when the accessed item's origin differs from `current_module`.
    pub(super) current_module: Option<crate::module::ModuleId>,
    /// Local name → (canonical origin module path, canonical item name,
    /// declared visibility) for items imported into the current module from
    /// elsewhere in the tree. Slice 7: re-exports collapse to the canonical
    /// entry — `import M.X` where M re-exports `a.b.X` records
    /// `("X" → (["a","b"], "X", ...))`, and an alias `import M.Y as Z` maps
    /// `"Z" → (["a","b"], "Y", ...)`. Populated during `build_type_env` when
    /// `tree` is set.
    pub(super) type_origins: HashMap<String, (Vec<String>, String, Visibility)>,
    pub(super) env: TypeEnv,
    pub(super) local_scope: LocalTypeScope,
    pub(super) errors: Vec<TypeError>,
    pub(super) warnings: Vec<TypeError>,
    pub(super) expr_types: HashMap<SpanKey, Type>,
    pub(super) current_return_type: Option<Type>,
    /// LB3 — per-label collector stack for labeled-block break-with-value
    /// LUB inference. Pushed at labeled-block entry; each `Break { label:
    /// Some(name), value: Some(e) }` site appends `infer_expr(e)` to the
    /// matching frame; bare `break label` (no value) appends `Type::Unit`.
    /// Popped at labeled-block exit; the labeled block's type is the LUB
    /// of `tail_type` and the collected break types. Saved/restored at
    /// closure boundaries (LB4) so labels are lexical to the function-
    /// body control flow. Loops keep their existing `Type::Never`-by-
    /// default behavior — loop-LUB inference is a separate slice that
    /// will reuse the same machinery once the design entry promotes
    /// (out-of-scope here).
    pub(super) break_value_types: Vec<(String, Vec<Type>)>,
    pub(super) current_self_type: Option<Type>,
    /// True when type-checking inside a defer/errdefer block.
    pub(super) in_defer: bool,
    /// `?` cross-error From conversions (span → target error type name).
    pub(super) question_conversions: HashMap<SpanKey, String>,
    /// `x.into()` conversions (span of the MethodCall → target type name).
    pub(super) into_conversions: HashMap<SpanKey, String>,
    /// `x.try_into()` conversions (span of the MethodCall → target type name,
    /// where target is the `T` extracted from `Result[T, E]`).
    pub(super) try_into_conversions: HashMap<SpanKey, String>,
    /// Enum names that derive `Display(snake_case)`. Populated during
    /// `env_add_enum`; transferred to `TypeCheckResult`.
    pub(super) display_snake_case_enums: HashSet<String>,
    /// MethodCall span → `Type.method` canonical callee key. See the
    /// matching field on `TypeCheckResult` for the full rationale.
    pub(super) method_callee_types: HashMap<SpanKey, String>,
    /// Bare-call expected-type dispatch resolutions: call-expression span →
    /// resolved target type name (e.g. `"Wrapper"`). Populated when
    /// `try_apply_expected_assoc_fn_inference` resolves a bare `name(args)`
    /// call against a concrete expected type. The lowering pass rewrites
    /// these to `Target.name(args)` so the interpreter / codegen can dispatch
    /// through the existing `Type.method` impl table.
    pub(super) bare_assoc_fn_targets: HashMap<SpanKey, String>,
    /// Per-call-site type substitutions: call-expression span → name → resolved
    /// type name (concrete struct/enum, or another generic param if the caller
    /// is itself generic and propagates the binding). Populated by `infer_call`
    /// after solving and by `check_expr`'s expected-type-driven pass for
    /// zero-arg generic calls. Consumed by the interpreter at each call: it
    /// pushes the resolved frame so `T.method()` and bare-method calls inside
    /// the callee's body can look up `T`'s concrete binding.
    pub(super) call_type_subs: HashMap<SpanKey, HashMap<String, String>>,
    /// Pattern-binding name → canonical type name. See the public copy on
    /// `TypeCheckResult` for the consumer doc.
    pub(super) pattern_binding_types: HashMap<SpanKey, String>,
    /// Pattern-binding span → inner element `TypeExpr` for `Vec[T]` / `Slice[T]`
    /// bindings. Sibling to `pattern_binding_types`. See the public copy on
    /// `TypeCheckResult` for the full rationale (PB sibling slice 2026-05-09).
    pub(super) pattern_binding_inner_types: HashMap<SpanKey, TypeExpr>,
    /// Internal mirror of the public table; written by `check_pattern_against`
    /// at every leaf-binding site (and at struct shorthand fields) when
    /// the scrutinee mode is non-Owned. Surfaced in `check()`.
    pub(super) pattern_binding_borrow_modes: HashMap<SpanKey, crate::ast::PatternBindingBorrow>,
    /// Parallel to `pattern_binding_inner_types`, storing the raw `Type`
    /// (which may contain unresolved `Type::TypeVar`) captured at the
    /// recording site. After body inference completes, `finalize_pattern_
    /// binding_inner_types` walks this map, resolves typevars against
    /// `env.substitutions`, and overwrites `pattern_binding_inner_types`
    /// with the substituted `TypeExpr`. Without this, `let mut q =
    /// VecDeque.new(); q.push_back(x);` writes the inner-type entry at
    /// the let site (where `?T0` is still unsolved), and the resulting
    /// `TypeKind::Error` strands codegen with the wrong element type.
    pub(super) pattern_binding_inner_unresolved: HashMap<SpanKey, Type>,
    /// Trait bounds for the generic parameters in the current enclosing scope
    /// (impl-level + function/method-level). Indexed by the param's textual
    /// name so it pairs naturally with `Type::TypeParam(name)`. Populated on
    /// entering a generic-bearing scope and saved/restored on exit, mirroring
    /// the enclosing-generic-name list threaded through the lower / check
    /// path. Used to resolve bare `method(args)` calls at expected-type
    /// positions when the expected type is a generic param.
    pub(super) enclosing_bounds: HashMap<String, Vec<crate::ast::TraitBound>>,
    /// Name of the enclosing trait declaration when type-checking a default
    /// method body. Populated on entering `check_trait_def`, cleared on exit.
    /// Consumed by `dispatch_self_receiver_method` (slice 3.5 of the
    /// method-resolution CR — see `phase-4-interpreter.md` item 8): when a
    /// receiver-form `self.method()` call appears in a default body, the
    /// candidate methods are the enclosing trait's own methods plus every
    /// method on traits in its supertrait closure. Outside trait bodies this
    /// is `None` and `Self` falls through to the silent pre-existing path
    /// (impl-method bodies bind `Self` to the impl's target type via
    /// `current_self_type`, a different mechanism).
    pub(super) enclosing_trait: Option<String>,
    /// Closure expression span → reason that closure became once-callable.
    /// Populated by `closure_type_with_capture_inference` when the body walk
    /// finds a captured-non-Copy consume; consumed by `check_assignable` so
    /// `E_ONCE_FN_INTO_FN_SLOT` can name the consumed binding when a closure
    /// literal is rejected at a `Fn` slot. Round 12.45 (Step 3).
    pub(super) closure_once_reasons: HashMap<SpanKey, OnceReason>,
}

/// Why a closure is `OnceFunction`-typed: which captured outer binding the
/// body consumed, and where in the body the consume happened. Populated by
/// the once-callability walker when it flips its first identifier-leaf in
/// `Consuming` mode that resolves to an outer non-Copy binding.
#[derive(Debug, Clone)]
pub(super) struct OnceReason {
    /// The outer binding name (or `"self"`) that the closure body consumed.
    pub(super) consumed_binding: String,
    /// The body span where the consume occurred (the identifier-leaf, not
    /// the enclosing call). Used for diagnostics; not currently surfaced in
    /// the rejection message but kept for future polish in Step 5.
    #[allow(dead_code)]
    pub(super) consumed_span: Span,
}

impl<'a> TypeChecker<'a> {
    pub fn new(program: &'a Program, resolve_result: &'a ResolveResult) -> Self {
        TypeChecker {
            program,
            resolve_result,
            tree: None,
            current_module: None,
            type_origins: HashMap::new(),
            env: TypeEnv::new(),
            local_scope: LocalTypeScope::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            expr_types: HashMap::new(),
            current_return_type: None,
            break_value_types: Vec::new(),
            current_self_type: None,
            in_defer: false,
            question_conversions: HashMap::new(),
            into_conversions: HashMap::new(),
            try_into_conversions: HashMap::new(),
            display_snake_case_enums: HashSet::new(),
            method_callee_types: HashMap::new(),
            bare_assoc_fn_targets: HashMap::new(),
            call_type_subs: HashMap::new(),
            pattern_binding_types: HashMap::new(),
            pattern_binding_inner_types: HashMap::new(),
            pattern_binding_borrow_modes: HashMap::new(),
            pattern_binding_inner_unresolved: HashMap::new(),
            enclosing_bounds: HashMap::new(),
            enclosing_trait: None,
            closure_once_reasons: HashMap::new(),
        }
    }

    /// Attach a project-wide `ProgramTree` so cross-module visibility checks
    /// (CR-24 slice 6) can consult origin modules. Without the tree, the
    /// typechecker runs in single-file mode exactly as before.
    pub fn with_tree(
        mut self,
        tree: &'a crate::module::ProgramTree,
        module_id: crate::module::ModuleId,
    ) -> Self {
        self.tree = Some(tree);
        self.current_module = Some(module_id);
        self
    }

    pub fn check(mut self) -> TypeCheckResult {
        self.build_type_env();
        self.validate_derive_copy();
        self.validate_copy_implies_clone();
        self.validate_derived_traits_recursive();
        self.validate_enum_payload_no_nested_enum();
        self.validate_derive_arithmetic();
        self.check_signature_visibility();
        self.check_items();
        self.finalize_pattern_binding_inner_types();
        let trait_impls: std::collections::HashSet<(String, String)> = self
            .env
            .impls
            .iter()
            .filter_map(|imp| imp.trait_name.clone().map(|t| (t, imp.target_type.clone())))
            .collect();
        let distinct_type_traits = self.env.distinct_types.clone();
        let compiler_builtins = self.env.compiler_builtins.clone();
        TypeCheckResult {
            errors: self.errors,
            warnings: self.warnings,
            expr_types: self.expr_types,
            struct_info: self.env.structs,
            enum_info: self.env.enums,
            distinct_type_traits,
            question_conversions: self.question_conversions,
            trait_impls,
            into_conversions: self.into_conversions,
            try_into_conversions: self.try_into_conversions,
            display_snake_case_enums: self.display_snake_case_enums,
            method_callee_types: self.method_callee_types,
            bare_assoc_fn_targets: self.bare_assoc_fn_targets,
            call_type_subs: self.call_type_subs,
            pattern_binding_types: self.pattern_binding_types,
            pattern_binding_inner_types: self.pattern_binding_inner_types,
            pattern_binding_borrow_modes: self.pattern_binding_borrow_modes,
            compiler_builtins,
        }
    }

    pub(super) fn type_error(&mut self, message: String, span: Span, kind: TypeErrorKind) {
        self.errors.push(TypeError {
            message,
            span,
            kind,
        });
    }

    /// Render a `ConstEvalError` from the const-expression evaluator
    /// (slice 2) as a focused `type_error` diagnostic. Reuses
    /// `TypeErrorKind::TypeMismatch` as the kind for surface-level
    /// const-eval errors — a dedicated kind isn't introduced at slice 2;
    /// future work may split if downstream consumers (`karac explain`)
    /// need to distinguish const-eval failures from other type errors.
    pub(crate) fn emit_const_eval_error(&mut self, err: ConstEvalError) {
        use ConstEvalError::*;
        let (msg, span) = match err {
            NonConstShape(s) => (
                "expression is not a valid const expression \
                 (no function calls, closures, method calls, or runtime-only shapes)"
                    .to_string(),
                s,
            ),
            Overflow { op, lhs, rhs, span } => (
                format!(
                    "const expression overflow: {} {} {} overflows {}",
                    format_const_value(&lhs),
                    binop_glyph(&op),
                    format_const_value(&rhs),
                    type_display(&const_value_type(&lhs))
                ),
                span,
            ),
            UnaryOverflow { op, operand, span } => (
                format!(
                    "const expression overflow: {}{} overflows {}",
                    unaryop_glyph(&op),
                    format_const_value(&operand),
                    type_display(&const_value_type(&operand))
                ),
                span,
            ),
            DivByZero { span } => ("const expression: division by zero".to_string(), span),
            OutOfRange {
                value,
                target_ty,
                span,
            } => (
                format!(
                    "const expression: literal {} does not fit in {}",
                    value,
                    type_display(&target_ty)
                ),
                span,
            ),
            TypeMismatch {
                expected,
                found,
                span,
            } => (
                format!(
                    "const expression type mismatch: expected {}, found {}",
                    type_display(&expected),
                    type_display(&found)
                ),
                span,
            ),
            UndefinedConst { name, span } => (
                format!("const expression: '{}' is not a known const", name),
                span,
            ),
            ArithOnNonInt { ty, op, span } => (
                format!(
                    "arithmetic operator '{}' is not supported on {} \
                     (only integer types)",
                    binop_glyph(&op),
                    type_display(&ty)
                ),
                span,
            ),
            LogicalOnNonBool { ty, op, span } => (
                format!(
                    "logical operator '{}' is not supported on {} (only bool)",
                    binop_glyph(&op),
                    type_display(&ty)
                ),
                span,
            ),
            CompareIncomparable {
                lhs_ty,
                rhs_ty,
                span,
            } => (
                format!(
                    "cannot compare {} with {} in const expression \
                     — both sides must have the same type",
                    type_display(&lhs_ty),
                    type_display(&rhs_ty)
                ),
                span,
            ),
            CyclicConstDef { chain, span } => (
                format!(
                    "const expression: cyclic const definition ({})",
                    chain.join(" -> ")
                ),
                span,
            ),
        };
        self.type_error(msg, span, TypeErrorKind::TypeMismatch);
    }

    pub(super) fn type_warning(&mut self, message: String, span: Span, kind: TypeErrorKind) {
        self.warnings.push(TypeError {
            message,
            span,
            kind,
        });
    }

    /// Validate an `as` cast (`from as to`) and emit a focused diagnostic
    /// when the pair is rejected. Per design.md § Numeric Semantics > as-
    /// cast semantics (v60 item 49):
    ///
    /// Accepted: numeric → numeric (saturating float→int, sign-/zero-
    /// extending int→int, IEEE 754 int→float, fptrunc / fpext for
    /// float→float); `bool → iN/uN` (zero-extends from i1); `char → uN`
    /// for `N >= 32` and `char → iN` for `N >= 32` (Unicode scalar value
    /// fits in 21 bits).
    ///
    /// Rejected with focused diagnostics:
    /// - `char → iN/uN` with `N < 32` → `E_CHAR_AS_NARROW_INT`.
    /// - `iN/uN → char` → `E_INT_AS_CHAR`.
    /// - `iN/uN → bool` → `E_INT_AS_BOOL`.
    /// - `f32/f64 → bool` → `E_FLOAT_AS_BOOL`.
    ///
    /// All other unsupported pairs fall through to the generic
    /// `cannot cast` diagnostic.
    fn check_cast_pair(&mut self, from_ty: &Type, to_ty: &Type, span: &Span) {
        // Type::Error is a wildcard — silently accept; the original error
        // already surfaced elsewhere.
        if matches!(from_ty, Type::Error) || matches!(to_ty, Type::Error) {
            return;
        }

        // Numeric → numeric: always accepted (existing rule).
        if is_numeric(from_ty) && is_numeric(to_ty) {
            return;
        }

        // Bool → integer: produces 0/1.
        if matches!(from_ty, Type::Bool) && is_integer(to_ty) {
            return;
        }

        // Char → wide integer (>= 32 bits): Unicode scalar value fits.
        if matches!(from_ty, Type::Char) {
            if let Some(width) = integer_width_bits(to_ty) {
                if width >= 32 {
                    return;
                }
                // Char → narrow integer: rejected with focused diagnostic.
                self.type_error(
                    format!(
                        "error[E_CHAR_AS_NARROW_INT]: cannot cast `char` to \
                         `{}` directly because the Unicode scalar range \
                         (`0..=0x10FFFF`) does not fit in {width} bits; \
                         help: `c as u32 as {}` for explicit truncation, or \
                         `c.encode_utf8(buf)` for proper UTF-8 encoding",
                        type_display(to_ty),
                        type_display(to_ty)
                    ),
                    span.clone(),
                    TypeErrorKind::InvalidCast,
                );
                return;
            }
        }

        // Integer → char: rejected (use char.try_from for fallible
        // construction).
        if is_integer(from_ty) && matches!(to_ty, Type::Char) {
            self.type_error(
                format!(
                    "error[E_INT_AS_CHAR]: cannot cast `{}` to `char` \
                     directly because not every integer is a valid \
                     Unicode scalar (surrogate range, values above \
                     `0x10FFFF`); help: `char.try_from(n)` returns \
                     `Result[char, _]`",
                    type_display(from_ty)
                ),
                span.clone(),
                TypeErrorKind::InvalidCast,
            );
            return;
        }

        // Integer → bool: rejected (use `n != 0`).
        if is_integer(from_ty) && matches!(to_ty, Type::Bool) {
            self.type_error(
                format!(
                    "error[E_INT_AS_BOOL]: cannot cast `{}` to `bool`; \
                     help: write `n != 0` for the explicit non-zero \
                     check",
                    type_display(from_ty)
                ),
                span.clone(),
                TypeErrorKind::InvalidCast,
            );
            return;
        }

        // Float → bool: rejected (the operation is meaningless).
        if matches!(from_ty, Type::Float(_)) && matches!(to_ty, Type::Bool) {
            self.type_error(
                format!(
                    "error[E_FLOAT_AS_BOOL]: cannot cast `{}` to `bool`; \
                     a float-to-bool conversion is not well-defined \
                     (NaN? denormal? -0?); decide on a predicate \
                     explicitly (e.g., `f != 0.0`) before casting",
                    type_display(from_ty)
                ),
                span.clone(),
                TypeErrorKind::InvalidCast,
            );
            return;
        }

        // Anything else falls through to the generic diagnostic.
        self.type_error(
            format!(
                "cannot cast '{}' to '{}'",
                type_display(from_ty),
                type_display(to_ty)
            ),
            span.clone(),
            TypeErrorKind::InvalidCast,
        );
    }

    /// Emit `error[E_EMPTY_PREFIX_LITERAL_NEEDS_ANNOTATION]` for an empty
    /// `Vec[]` / `Array[]` / `Set[]` / `Map[]` literal that reached
    /// synthesis mode without an enclosing annotation. The diagnostic body
    /// names the literal kind, supplies a per-kind annotation skeleton, and
    /// suggests the corresponding constructor (`Vec.new()` / `Set.new()` /
    /// `Map.new()`) per design.md § Collection Literals.
    fn report_empty_prefix_literal(&mut self, type_name: &str, span: &Span) {
        let (annotation_skeleton, constructor) = match type_name {
            "Vec" => ("Vec[T]", Some("Vec.new()")),
            "Array" => ("Array[T, 0]", None),
            "Set" => ("Set[T]", Some("Set.new()")),
            "Map" => ("Map[K, V]", Some("Map.new()")),
            _ => (type_name, None),
        };
        let mut msg = format!(
            "error[E_EMPTY_PREFIX_LITERAL_NEEDS_ANNOTATION]: cannot infer \
             element type from empty `{type_name}[]` literal — \
             add a binding annotation: `let v: {annotation_skeleton} = {type_name}[]`"
        );
        if let Some(ctor) = constructor {
            msg.push_str(&format!(", or use `{ctor}`"));
        }
        self.type_error(msg, span.clone(), TypeErrorKind::TypeMismatch);
    }

    /// Emit `NoMethodFound` for an unknown stdlib method only when a close
    /// candidate exists in `known_methods` (edit distance ≤ 2 via
    /// `edit_distance::suggest_similar`). Used by per-type `infer_*_method`
    /// arms to surface typos without breaking the silent fallback for
    /// runtime-only methods that the typechecker has not yet enumerated.
    /// Each arm's `KNOWN_METHODS` constant is the typechecker's current
    /// enumeration of that type's surface — it grows as stdlib enumeration
    /// catches up to the interpreter, at which point the arm's `_` case
    /// can flip from "typo-only" to "always-error". See
    /// phase-4-interpreter.md § Method Resolution Step 7.
    fn maybe_emit_method_typo(
        &mut self,
        type_name: &str,
        method: &str,
        known_methods: &[&str],
        span: &Span,
    ) {
        if let Some(suggestion) = crate::edit_distance::suggest_similar(method, known_methods) {
            self.type_error(
                format!(
                    "no method '{}' on type '{}', did you mean '{}'?",
                    method, type_name, suggestion
                ),
                span.clone(),
                TypeErrorKind::NoMethodFound,
            );
        }
    }

    /// Default `_` arm body for per-type `infer_*_method` dispatch: emit a
    /// typo-suggestion diagnostic when the typed name is close to a known
    /// method, type-check the arguments, and return `Type::Error`. The
    /// silent fallback for far-from-anything names preserves the historical
    /// permissive behavior for runtime-only methods that the typechecker
    /// has not yet enumerated.
    ///
    /// Reserved for arms whose typechecker enumeration has *not yet* reached
    /// parity with the interpreter (currently the four phase-11 arms — Regex
    /// and the three HTTP types). Phase-8-floor arms have flipped to
    /// `require_known_method` so unknown methods on those types fail loudly.
    fn handle_unknown_method(
        &mut self,
        type_name: &str,
        method: &str,
        known_methods: &[&str],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        self.maybe_emit_method_typo(type_name, method, known_methods, span);
        for arg in args {
            self.infer_expr(&arg.value);
        }
        Type::Error
    }

    /// Default `_` arm body for per-type `infer_*_method` dispatch on arms
    /// whose typechecker enumeration has reached parity with the interpreter:
    /// **always** emit `NoMethodFound`, type-check the arguments, and return
    /// `Type::Error`. If the typed name is edit-distance ≤ 2 from a known
    /// method, the diagnostic includes a `did you mean ...?` suggestion;
    /// otherwise it reports the unknown name plainly. Either way the
    /// diagnostic fires — there is no silent fall-through.
    ///
    /// Used by phase-8-floor arms (String, Slice, Map, Entry, SortedSet,
    /// Set, Iterator, Sender, Receiver). Phase-11 arms keep using
    /// `handle_unknown_method` until their floor lands.
    /// See phase-4-interpreter.md § Method Resolution Step 7(d).
    fn require_known_method(
        &mut self,
        type_name: &str,
        method: &str,
        known_methods: &[&str],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let msg = match crate::edit_distance::suggest_similar(method, known_methods) {
            Some(suggestion) => format!(
                "no method '{}' on type '{}', did you mean '{}'?",
                method, type_name, suggestion
            ),
            None => format!("no method '{}' on type '{}'", method, type_name),
        };
        self.type_error(msg, span.clone(), TypeErrorKind::NoMethodFound);
        for arg in args {
            self.infer_expr(&arg.value);
        }
        Type::Error
    }

    /// Map a lexer-provided integer suffix to the concrete `Type` it denotes.
    /// `None` defaults to `i64`. `I128` / `U128` route to
    /// `IntSize::I128` / `UIntSize::U128` (added 2026-05-11 alongside
    /// const generics slice 2b — `IntSize`/`UIntSize` carry the 128-bit
    /// variants now; downstream consumers should handle them through
    /// the standard arms).
    fn type_from_int_suffix(&mut self, sfx: Option<IntSuffix>, _span: Span) -> Type {
        match sfx {
            None => Type::Int(IntSize::I64),
            Some(IntSuffix::I8) => Type::Int(IntSize::I8),
            Some(IntSuffix::I16) => Type::Int(IntSize::I16),
            Some(IntSuffix::I32) => Type::Int(IntSize::I32),
            Some(IntSuffix::I64) => Type::Int(IntSize::I64),
            Some(IntSuffix::I128) => Type::Int(IntSize::I128),
            Some(IntSuffix::U8) => Type::UInt(UIntSize::U8),
            Some(IntSuffix::U16) => Type::UInt(UIntSize::U16),
            Some(IntSuffix::U32) => Type::UInt(UIntSize::U32),
            Some(IntSuffix::U64) => Type::UInt(UIntSize::U64),
            Some(IntSuffix::U128) => Type::UInt(UIntSize::U128),
        }
    }

    /// Map a lexer-provided float suffix to the concrete `Type` it denotes.
    /// `None` defaults to `f64`.
    fn type_from_float_suffix(sfx: Option<FloatSuffix>) -> Type {
        match sfx {
            None | Some(FloatSuffix::F64) => Type::Float(FloatSize::F64),
            Some(FloatSuffix::F32) => Type::Float(FloatSize::F32),
        }
    }

    pub(super) fn check_assignable(&mut self, expected: &Type, found: &Type, span: Span) -> bool {
        if is_subtype(expected, found) {
            return true;
        }
        if Self::is_once_into_fn_shape(expected, found) {
            let mut msg = format!(
                "cannot pass once-callable closure where '{}' is expected; \
                 the slot promises repeatable invocation but the closure has type '{}'",
                type_display(expected),
                type_display(found),
            );
            let consumed = self
                .closure_once_reasons
                .get(&SpanKey::from_span(&span))
                .map(|r| r.consumed_binding.clone());
            if let Some(name) = &consumed {
                msg.push_str(&format!(
                    " (closure becomes once-callable because it consumes captured binding '{}')",
                    name
                ));
            }
            msg.push_str(
                "; help: clone the captured value before the closure body consumes it \
                 so the closure becomes repeatable, restructure the code to invoke the \
                 closure locally instead of routing it through this slot, or change the \
                 slot type to `OnceFn(...)` if you control its declaration",
            );
            self.type_error(msg, span, TypeErrorKind::OnceFnIntoFnSlot);
            return false;
        }
        self.type_error(
            format!(
                "expected '{}', found '{}'",
                type_display(expected),
                type_display(found)
            ),
            span,
            TypeErrorKind::TypeMismatch,
        );
        false
    }

    /// Returns `true` iff the assignment is a once-callable closure flowing
    /// into a `Fn`-shaped slot. Both `Fn(...)` and `ref Fn(...)` slots
    /// reject `OnceFn` arguments — the callee in either case may invoke
    /// the parameter many times, which violates the once-callable contract.
    /// Refs on either side are stripped before comparison so cross-wrapping
    /// (e.g., bare `OnceFn` arg into `ref Fn` slot) is also recognized as
    /// the once-callability violation rather than a generic ref-mismatch.
    /// Step 3 / round 12.45.
    fn is_once_into_fn_shape(expected: &Type, found: &Type) -> bool {
        fn unwrap(t: &Type) -> &Type {
            match t {
                Type::Ref(inner) | Type::MutRef(inner) => unwrap(inner),
                _ => t,
            }
        }
        matches!(
            (unwrap(expected), unwrap(found)),
            (Type::Function { .. }, Type::OnceFunction { .. })
        )
    }

    fn record_expr_type(&mut self, span: &Span, ty: &Type) {
        self.expr_types.insert(SpanKey::from_span(span), ty.clone());
    }

    // ── Check Items (Pass 2) ────────────────────────────────────

    fn infer_field_access(&mut self, object: &Expr, field: &str, span: &Span) -> Type {
        // Primitive-type associated constants — `i64.MAX`, `f64.INFINITY`,
        // `usize.MAX`, etc. The parser emits these as
        // `FieldAccess { object: Identifier("<primitive>"), field: "<NAME>" }`.
        // Intercept here before `infer_expr(object)` would silently return
        // `Type::Error` for the bare primitive identifier. The lookup
        // returns the const's typed surface so downstream code (`let x =
        // i64.MAX;`) sees the right `Type::Int(I64)` etc.
        if let ExprKind::Identifier(name) = &object.kind {
            if let Some(cv) = crate::prelude::lookup_primitive_const(name, field) {
                return primitive_const_type(cv);
            }
        }
        let obj_ty = self.infer_expr(object);
        if obj_ty == Type::Error {
            return Type::Error;
        }

        // Slice 1b: opaque foreign types (`unsafe extern { type Foo; }`)
        // have no fields visible to Kāra — the C side owns the layout, so
        // even `r.field` through a `ref Foo` / `mut ref Foo` has no
        // meaningful resolution. The bare `Type::Named` arm is a defensive
        // belt for typecheck-error-recovery flows; the by-value binding
        // itself would already have fired `E_OPAQUE_TYPE_REQUIRES_INDIRECTION`
        // upstream.
        let opaque_receiver_name = match &obj_ty {
            Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                Type::Named { name, .. } if self.env.opaque_foreign_types.contains(name) => {
                    Some(name.clone())
                }
                _ => None,
            },
            Type::Named { name, .. } if self.env.opaque_foreign_types.contains(name) => {
                Some(name.clone())
            }
            _ => None,
        };
        if let Some(name) = opaque_receiver_name {
            self.type_error(
                format!(
                    "error[E_OPAQUE_TYPE_NO_FIELDS]: opaque foreign type '{name}' \
                     has no fields visible to Kāra; the C side owns the layout. \
                     Field access through `ref {name}` / `mut ref {name}` is not \
                     supported — pass the reference to a foreign function declared \
                     in the same `unsafe extern {{ }}` block instead"
                ),
                span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return Type::Error;
        }

        let type_name = match &obj_ty {
            Type::Named { name, .. } => name.clone(),
            // Shared-struct receivers (`Type::Shared(name)` — a `shared
            // struct N { ... }`'s value type) carry the same struct
            // definition lookup as a bare `Type::Named { name, args: [] }`.
            // Without this arm, `node.field` on a pattern-bound shared
            // handle falls through to `Type::Error` and silently breaks
            // every downstream consumer (match scrutinee inference,
            // method dispatch, pattern-binding type recording).
            Type::Shared(name) => name.clone(),
            _ => return Type::Error,
        };

        if let Some(struct_info) = self.env.structs.get(&type_name) {
            let struct_info = struct_info.clone();
            for (fname, ftype, is_pub) in &struct_info.fields {
                if fname == field {
                    // CR-18 field-access half: reject non-`pub` field access
                    // on an imported struct from outside the defining module.
                    if !is_pub {
                        self.check_cross_module_field_access(&type_name, field, span);
                    }
                    return ftype.clone();
                }
            }
            let available: Vec<&str> = struct_info
                .fields
                .iter()
                .map(|(n, _, _)| n.as_str())
                .collect();
            self.type_error(
                format!(
                    "no field '{}' on struct '{}', available fields: {}",
                    field,
                    type_name,
                    available.join(", ")
                ),
                span.clone(),
                TypeErrorKind::UndefinedField,
            );
            Type::Error
        } else {
            // Not in the local env, but may be an imported struct — probe the
            // origin module directly so cross-module field access can still
            // be validated for CR-18.
            self.infer_imported_field_access(&type_name, field, span)
        }
    }

    /// Emit `E0221 PrivateTypeInPublicSignature` when a non-`pub` field is
    /// accessed on an imported struct from outside its defining module. For
    /// local structs (and when no `ProgramTree` is attached), silently
    /// accepts the access — slice 6b treats same-module field access as
    /// always allowed.
    fn check_cross_module_field_access(&mut self, struct_name: &str, field: &str, span: &Span) {
        let Some(tree) = self.tree else { return };
        let Some(current_id) = self.current_module else {
            return;
        };
        let current_path = tree.module(current_id).path.clone();

        // Find the defining module. For a local struct, origin == current.
        let origin_path: Vec<String> = match self.type_origins.get(struct_name) {
            Some((path, _, _)) => path.clone(),
            None => current_path.clone(),
        };
        if origin_path == current_path {
            // Same-module access — non-pub fields are always reachable to
            // sibling code.
            return;
        }
        self.type_error(
            format!(
                "private field '{}' of struct '{}' is not visible outside its defining module",
                field, struct_name,
            ),
            span.clone(),
            TypeErrorKind::PrivateTypeInPublicSignature,
        );
    }

    /// Access a field on a struct that isn't registered in the local env —
    /// typically an imported struct from another module. Consults the
    /// `ProgramTree` so we can (a) return the field type and (b) enforce
    /// the cross-module field-visibility rule.
    fn infer_imported_field_access(&mut self, struct_name: &str, field: &str, span: &Span) -> Type {
        let Some(tree) = self.tree else {
            return Type::Error;
        };
        let Some((origin_path, canonical_name, _vis)) = self.type_origins.get(struct_name).cloned()
        else {
            return Type::Error;
        };
        let Some(&origin_id) = tree.graph.by_path.get::<[String]>(&origin_path) else {
            return Type::Error;
        };
        let origin_module = tree.module(origin_id);
        // Look up by the canonical name — `struct_name` here may be an
        // import alias (`import db.Connection as Conn` binds `Conn` but the
        // struct is defined as `Connection`). The canonical name survives
        // the chain walked in `collect_import_origins`.
        let Some(struct_def) = find_struct_def(origin_module, &canonical_name) else {
            return Type::Error;
        };
        let field_def = match struct_def.fields.iter().find(|f| f.name == field) {
            Some(f) => f,
            None => {
                let available: Vec<&str> =
                    struct_def.fields.iter().map(|f| f.name.as_str()).collect();
                self.type_error(
                    format!(
                        "no field '{}' on struct '{}', available fields: {}",
                        field,
                        struct_name,
                        available.join(", ")
                    ),
                    span.clone(),
                    TypeErrorKind::UndefinedField,
                );
                return Type::Error;
            }
        };

        if !field_def.is_pub {
            // `origin_path` is guaranteed to differ from `current_module`'s
            // path because `type_origins` only holds cross-module entries.
            self.type_error(
                format!(
                    "private field '{}' of struct '{}' is not visible outside its defining module",
                    field, struct_name,
                ),
                span.clone(),
                TypeErrorKind::PrivateTypeInPublicSignature,
            );
        }

        // Return the field's declared type. We lower the TypeExpr with an
        // empty generic scope — the origin module's generics are not in
        // scope here, and that's OK for slice-6b's coarse cross-module type
        // surface.
        self.lower_type_expr(&field_def.ty, &[])
    }

    // ── Struct Literals ─────────────────────────────────────────

    fn infer_struct_literal(&mut self, path: &[String], fields: &[FieldInit], span: &Span) -> Type {
        let struct_name = path.last().cloned().unwrap_or_default();

        let struct_info = match self.env.structs.get(&struct_name) {
            Some(info) => info.clone(),
            None => {
                // Type-check field values anyway
                for f in fields {
                    self.infer_expr(&f.value);
                }
                self.type_error(
                    format!("'{}' is not a struct", struct_name),
                    span.clone(),
                    TypeErrorKind::NotAStruct,
                );
                return Type::Error;
            }
        };

        let expected_fields: HashSet<&str> = struct_info
            .fields
            .iter()
            .map(|(n, _, _)| n.as_str())
            .collect();
        let provided_fields: HashSet<&str> = fields.iter().map(|f| f.name.as_str()).collect();

        // Check for missing fields
        for (fname, _, _) in &struct_info.fields {
            if !provided_fields.contains(fname.as_str()) {
                self.type_error(
                    format!("missing field '{}' in struct '{}'", fname, struct_name),
                    span.clone(),
                    TypeErrorKind::MissingField,
                );
            }
        }

        // Check for extra fields
        for f in fields {
            if !expected_fields.contains(f.name.as_str()) {
                self.type_error(
                    format!("unknown field '{}' in struct '{}'", f.name, struct_name),
                    f.span.clone(),
                    TypeErrorKind::ExtraField,
                );
            }
        }

        // Type-check field values. Use `check_expr` against the field's
        // declared type when known so check-mode coercions (empty
        // `Vec[]` / `Set[]` / `Array[]`, `Into` / `TryInto`, closure
        // pushdown, etc.) fire at struct-field initializer positions.
        // Fall back to synthesis when the field is not declared on the
        // struct (already diagnosed above as an extra field).
        for f in fields {
            if let Some((_, expected_ty, _)) =
                struct_info.fields.iter().find(|(n, _, _)| n == &f.name)
            {
                self.check_expr(&f.value, &expected_ty.clone());
            } else {
                self.infer_expr(&f.value);
            }
        }

        // Shared-struct literals lower to Type::Shared so the literal's
        // type matches an annotated `let s: S = S { ... }` shape and the
        // method-resolution deref step (sub-item 3a) sees a consistent
        // receiver type. Sub-item 2's `lower_path_type` intercept handles
        // the annotation side; this is its construction-site twin.
        if struct_info.is_shared {
            Type::Shared(struct_name)
        } else {
            Type::Named {
                name: struct_name,
                args: Vec::new(),
            }
        }
    }

    // ── Match ───────────────────────────────────────────────────
}

#[cfg(test)]
mod once_function_carrier_tests {
    use super::*;

    fn fn_i32_to_i32() -> Type {
        Type::Function {
            params: vec![Type::Int(IntSize::I32)],
            return_type: Box::new(Type::Int(IntSize::I32)),
        }
    }

    fn once_fn_i32_to_i32() -> Type {
        Type::OnceFunction {
            params: vec![Type::Int(IntSize::I32)],
            return_type: Box::new(Type::Int(IntSize::I32)),
        }
    }

    #[test]
    fn type_display_prints_oncefn() {
        assert_eq!(type_display(&once_fn_i32_to_i32()), "OnceFn(i32) -> i32");
        assert_eq!(type_display(&fn_i32_to_i32()), "Fn(i32) -> i32");
    }

    #[test]
    fn type_display_oncefn_unit_return_omits_arrow() {
        let no_arg_unit = Type::OnceFunction {
            params: vec![],
            return_type: Box::new(Type::Unit),
        };
        assert_eq!(type_display(&no_arg_unit), "OnceFn()");
    }

    #[test]
    fn type_display_oncefn_multi_param() {
        let multi = Type::OnceFunction {
            params: vec![Type::Int(IntSize::I32), Type::Bool],
            return_type: Box::new(Type::Float(FloatSize::F64)),
        };
        assert_eq!(type_display(&multi), "OnceFn(i32, bool) -> f64");
    }

    #[test]
    fn types_compatible_oncefn_identity() {
        assert!(types_compatible(
            &once_fn_i32_to_i32(),
            &once_fn_i32_to_i32()
        ));
    }

    #[test]
    fn types_compatible_oncefn_rejects_fn_in_either_direction() {
        assert!(!types_compatible(&once_fn_i32_to_i32(), &fn_i32_to_i32()));
        assert!(!types_compatible(&fn_i32_to_i32(), &once_fn_i32_to_i32()));
    }

    #[test]
    fn types_compatible_oncefn_param_arity_mismatch() {
        let one = once_fn_i32_to_i32();
        let two = Type::OnceFunction {
            params: vec![Type::Int(IntSize::I32), Type::Int(IntSize::I32)],
            return_type: Box::new(Type::Int(IntSize::I32)),
        };
        assert!(!types_compatible(&one, &two));
    }

    #[test]
    fn numeric_trait_arms_reject_oncefn() {
        // The trait-bound queries (`type_supports_*`) live on `TypeChecker`, so
        // we build a minimal one against an empty parsed program. With no impls
        // registered, the function-shape arms (now extended with `OnceFunction`)
        // are the ones exercised — verifying we widened the catch-all "false"
        // patterns rather than silently letting `OnceFunction` fall through to
        // permissive arms.
        let parsed = crate::parse("");
        let resolved = crate::resolve(&parsed.program);
        let tc = TypeChecker::new(&parsed.program, &resolved);
        let oncefn = once_fn_i32_to_i32();
        assert!(!tc.type_supports_partial_eq(&oncefn));
        assert!(!tc.type_supports_eq(&oncefn));
        assert!(!tc.type_supports_hash(&oncefn));
        assert!(!tc.type_supports_ord(&oncefn));
        assert!(!tc.type_supports_display(&oncefn));
        assert!(!tc.type_supports_partial_ord(&oncefn));
    }

    #[test]
    fn substitute_type_params_preserves_once() {
        let t_to_t = Type::OnceFunction {
            params: vec![Type::TypeParam("T".to_string())],
            return_type: Box::new(Type::TypeParam("T".to_string())),
        };
        let mut subs = HashMap::new();
        subs.insert("T".to_string(), SubstValue::Type(Type::Bool));
        let resolved = substitute_type_params(&t_to_t, &subs);
        assert_eq!(
            resolved,
            Type::OnceFunction {
                params: vec![Type::Bool],
                return_type: Box::new(Type::Bool),
            }
        );
    }

    #[test]
    fn contains_type_param_handles_oncefn() {
        let with_param = Type::OnceFunction {
            params: vec![Type::TypeParam("T".to_string())],
            return_type: Box::new(Type::Int(IntSize::I32)),
        };
        assert!(contains_type_param(&with_param));

        let no_param = once_fn_i32_to_i32();
        assert!(!contains_type_param(&no_param));
    }

    // ── Type::Shared / Type::Rc / Type::Arc variants ──

    #[test]
    fn test_type_display_shared_rc_arc_variants() {
        let shared = Type::Shared("S".to_string());
        assert_eq!(type_display(&shared), "S");

        let rc_i64 = Type::Rc(Box::new(Type::Int(IntSize::I64)));
        assert_eq!(type_display(&rc_i64), "Rc[i64]");

        let arc_str = Type::Arc(Box::new(Type::Str));
        assert_eq!(type_display(&arc_str), "Arc[String]");
    }

    #[test]
    fn test_types_compatible_rc_not_assignable_to_arc() {
        let rc_i64 = Type::Rc(Box::new(Type::Int(IntSize::I64)));
        let arc_i64 = Type::Arc(Box::new(Type::Int(IntSize::I64)));
        assert!(!types_compatible(&rc_i64, &arc_i64));
        assert!(!types_compatible(&arc_i64, &rc_i64));

        // The legacy structural form `Type::Named { name: "Rc", … }` is
        // a different type now — variants are distinct, even though
        // sub-item 2 hasn't yet migrated callers to construct them.
        let legacy_rc = Type::Named {
            name: "Rc".to_string(),
            args: vec![Type::Int(IntSize::I64)],
        };
        assert!(!types_compatible(&rc_i64, &legacy_rc));
        assert!(!types_compatible(&legacy_rc, &rc_i64));
    }

    #[test]
    fn test_types_compatible_shared_struct_name_match() {
        let shared_s = Type::Shared("S".to_string());
        let shared_s2 = Type::Shared("S".to_string());
        assert!(types_compatible(&shared_s, &shared_s2));

        let shared_t = Type::Shared("T".to_string());
        assert!(!types_compatible(&shared_s, &shared_t));

        // Distinct from the legacy `Type::Named { name: "S", args: [] }`.
        let legacy_s = Type::Named {
            name: "S".to_string(),
            args: vec![],
        };
        assert!(!types_compatible(&shared_s, &legacy_s));
        assert!(!types_compatible(&legacy_s, &shared_s));
    }

    // ── lower_path_type produces Rc / Arc / Shared variants (sub-item 2) ──

    fn build_typechecker(src: &str) -> TypeChecker<'static> {
        // Leak the parsed/resolved data so the TypeChecker borrow is 'static
        // for the duration of the test — fine; the lifetime ends with the
        // test process.
        let parsed: &'static _ = Box::leak(Box::new(crate::parse(src)));
        let resolved: &'static _ = Box::leak(Box::new(crate::resolve(&parsed.program)));
        let mut tc = TypeChecker::new(&parsed.program, resolved);
        tc.build_type_env();
        tc
    }

    fn path_with_args(name: &str, args: Vec<crate::ast::TypeExpr>) -> crate::ast::PathExpr {
        use crate::ast::GenericArg;
        crate::ast::PathExpr {
            segments: vec![name.to_string()],
            generic_args: if args.is_empty() {
                None
            } else {
                Some(args.into_iter().map(GenericArg::Type).collect())
            },
            span: Span::default(),
        }
    }

    fn type_path(name: &str) -> crate::ast::TypeExpr {
        crate::ast::TypeExpr {
            kind: crate::ast::TypeKind::Path(path_with_args(name, vec![])),
            span: Span::default(),
        }
    }

    #[test]
    fn test_lower_rc_path_type_produces_rc_variant() {
        let mut tc = build_typechecker("");
        let path = path_with_args("Rc", vec![type_path("i64")]);
        let lowered = tc.lower_path_type(&path, &[]);
        assert_eq!(lowered, Type::Rc(Box::new(Type::Int(IntSize::I64))));
    }

    #[test]
    fn test_lower_arc_path_type_produces_arc_variant() {
        let mut tc = build_typechecker("");
        let path = path_with_args("Arc", vec![type_path("String")]);
        let lowered = tc.lower_path_type(&path, &[]);
        assert_eq!(lowered, Type::Arc(Box::new(Type::Str)));
    }

    #[test]
    fn test_lower_shared_struct_path_type_produces_shared_variant() {
        let mut tc = build_typechecker("shared struct S { val: i64 }");
        let path = path_with_args("S", vec![]);
        let lowered = tc.lower_path_type(&path, &[]);
        assert_eq!(lowered, Type::Shared("S".to_string()));
    }

    #[test]
    fn test_lower_nonshared_struct_path_type_stays_named() {
        // Cross-check: the shared-struct intercept must not fire for plain
        // structs — sub-item 2's behavior-preserving promise hinges on this.
        let mut tc = build_typechecker("struct P { val: i64 }");
        let path = path_with_args("P", vec![]);
        let lowered = tc.lower_path_type(&path, &[]);
        assert_eq!(
            lowered,
            Type::Named {
                name: "P".to_string(),
                args: vec![],
            }
        );
    }

    // ── Method resolution: receiver_for_method_lookup deref step (sub-item 3a) ──

    #[test]
    fn test_receiver_for_lookup_strips_ref_wrappers() {
        let foo = Type::Named {
            name: "Foo".to_string(),
            args: vec![],
        };
        // `ref Foo` and `mut ref Foo` deref to `Foo` per design.md
        // § Method Resolution Step 1 — same as before sub-item 3a.
        assert_eq!(
            receiver_for_method_lookup(&Type::Ref(Box::new(foo.clone()))),
            foo
        );
        assert_eq!(
            receiver_for_method_lookup(&Type::MutRef(Box::new(foo.clone()))),
            foo
        );
    }

    #[test]
    fn test_receiver_for_lookup_shared_lowers_to_named() {
        // `Type::Shared(S)` lowers to `Type::Named { name: "S", args: [] }`
        // so the candidate-list lookup feeds into the existing
        // user-defined-struct method-resolution path verbatim.
        let shared = Type::Shared("S".to_string());
        assert_eq!(
            receiver_for_method_lookup(&shared),
            Type::Named {
                name: "S".to_string(),
                args: vec![],
            }
        );
    }

    #[test]
    fn test_receiver_for_lookup_rc_arc_deref_to_inner() {
        // `Rc[Foo]` and `Arc[Foo]` strip the wrapper so the inner type's
        // methods become reachable. Args carry through (e.g.
        // `Rc[Vec[i64]]` → `Vec[i64]`).
        let foo = Type::Named {
            name: "Foo".to_string(),
            args: vec![],
        };
        assert_eq!(
            receiver_for_method_lookup(&Type::Rc(Box::new(foo.clone()))),
            foo
        );
        assert_eq!(
            receiver_for_method_lookup(&Type::Arc(Box::new(foo.clone()))),
            foo
        );

        let vec_i64 = Type::Named {
            name: "Vec".to_string(),
            args: vec![Type::Int(IntSize::I64)],
        };
        assert_eq!(
            receiver_for_method_lookup(&Type::Rc(Box::new(vec_i64.clone()))),
            vec_i64
        );
    }

    #[test]
    fn test_receiver_for_lookup_passthrough_for_other_types() {
        // No-op for types without an outer wrapper — TypeParam, primitive,
        // etc. — so the existing arms in `infer_method_call` (TypeParam
        // dispatch, fallthrough) still receive the original shape.
        let tp = Type::TypeParam("T".to_string());
        assert_eq!(receiver_for_method_lookup(&tp), tp);

        let prim = Type::Int(IntSize::I64);
        assert_eq!(receiver_for_method_lookup(&prim), prim);
    }
}

#[cfg(test)]
mod closure_once_callability_inference_tests {
    //! Round 12.44 (Step 2) — closure-expression once-callability
    //! inference at construction. Verifies the typechecker assigns
    //! `Type::OnceFunction` to closures whose body consumes a captured
    //! outer non-Copy binding, `Type::Function` otherwise (capture-free
    //! / read-only-capture / explicit `ref ||` / `mut ref ||` prefix).
    use super::*;

    /// Type-check `src`, then return the inferred type of the first
    /// `Function` or `OnceFunction` value in `expr_types` — i.e., the
    /// closure expression's recorded type. Closure expressions are the
    /// only places these variants appear in user programs (no surface
    /// `Fn(...)` / `OnceFn(...)` annotation lower path yet).
    fn first_closure_type(src: &str) -> Type {
        let parsed = crate::parse(src);
        let resolved = crate::resolve(&parsed.program);
        let tc = crate::typecheck(&parsed.program, &resolved);
        for ty in tc.expr_types.values() {
            if matches!(ty, Type::Function { .. } | Type::OnceFunction { .. }) {
                return ty.clone();
            }
        }
        panic!(
            "expected a Function/OnceFunction-typed closure expression in `expr_types`; \
             expr_types: {:?}",
            tc.expr_types
        );
    }

    #[test]
    fn closure_captures_and_consumes_infers_oncefn() {
        // `apply(cfg)`: `apply` takes owned non-Copy `Cfg`, so the
        // capture-position `cfg` is in Consuming mode → outer non-Copy
        // → closure is once-callable → `Type::OnceFunction`.
        let src = "struct Cfg { name: i64 }\n\
                   fn apply(c: Cfg) { }\n\
                   fn make(cfg: Cfg) {\n\
                       let h = || apply(cfg);\n\
                       let _ = h;\n\
                   }";
        let ty = first_closure_type(src);
        assert!(
            matches!(ty, Type::OnceFunction { .. }),
            "expected OnceFunction; got {}",
            type_display(&ty)
        );
    }

    #[test]
    fn closure_only_reads_capture_infers_fn() {
        // `cfg.name` is a FieldAccess walked in Reading mode at the
        // closure body's top level → the `cfg` identifier-leaf inside
        // is Reading → no consume → repeatable closure → `Function`.
        let src = "struct Cfg { name: i64 }\n\
                   fn make(cfg: Cfg) -> i64 {\n\
                       let h = || cfg.name;\n\
                       cfg.name\n\
                   }";
        let ty = first_closure_type(src);
        assert!(
            matches!(ty, Type::Function { .. }),
            "expected Function; got {}",
            type_display(&ty)
        );
    }

    #[test]
    fn capture_free_closure_infers_fn() {
        // No outer references → no captures → trivially repeatable.
        let src = "fn main() {\n\
                       let h = || 42;\n\
                       let _ = h;\n\
                   }";
        let ty = first_closure_type(src);
        assert!(
            matches!(ty, Type::Function { .. }),
            "expected Function; got {}",
            type_display(&ty)
        );
    }

    #[test]
    fn explicit_ref_prefix_forces_fn_even_when_body_would_consume() {
        // `ref ||` declares the captures as borrows; the round-12.6
        // repeatable-closure rule says these are NOT once-callable
        // regardless of body shape. The body here would otherwise look
        // consume-y to the walker (call with own param slot), but the
        // explicit prefix short-circuits the walk to `Function`.
        let src = "struct Cfg { name: i64 }\n\
                   fn apply(c: Cfg) { }\n\
                   fn make(cfg: Cfg) {\n\
                       let h = ref || apply(cfg);\n\
                       let _ = h;\n\
                   }";
        let ty = first_closure_type(src);
        assert!(
            matches!(ty, Type::Function { .. }),
            "expected Function (ref prefix forces repeatable); got {}",
            type_display(&ty)
        );
    }

    #[test]
    fn explicit_mut_ref_prefix_forces_fn_even_when_body_would_consume() {
        // `mut ref ||` declares the captures as mutable borrows; same
        // round-12.6 rule. Body shape that would otherwise infer
        // OnceFn must produce `Function` here.
        let src = "struct Cfg { name: i64 }\n\
                   fn apply(c: Cfg) { }\n\
                   fn make(cfg: Cfg) {\n\
                       let h = mut ref || apply(cfg);\n\
                       let _ = h;\n\
                   }";
        let ty = first_closure_type(src);
        assert!(
            matches!(ty, Type::Function { .. }),
            "expected Function (mut ref prefix forces repeatable); got {}",
            type_display(&ty)
        );
    }

    #[test]
    fn closure_consuming_copy_capture_infers_fn() {
        // `apply(n)` where `n` is `i64` (Copy). Even though `n` is in
        // Consuming mode, Copy types never trigger once-callability —
        // a Copy capture is duplicated, not moved, on every invocation.
        let src = "fn apply(x: i64) { }\n\
                   fn make() {\n\
                       let n: i64 = 42;\n\
                       let h = || apply(n);\n\
                       let _ = h;\n\
                   }";
        let ty = first_closure_type(src);
        assert!(
            matches!(ty, Type::Function { .. }),
            "expected Function (Copy capture, not once-callable); got {}",
            type_display(&ty)
        );
    }

    #[test]
    fn closure_param_shadowing_outer_non_copy_does_not_capture() {
        // The closure's `cfg` parameter shadows the outer `cfg`, so
        // the body's `apply(cfg)` consumes the PARAM, not a capture.
        // No outer non-Copy is consumed → repeatable.
        let src = "struct Cfg { name: i64 }\n\
                   fn apply(c: Cfg) { }\n\
                   fn make(cfg: Cfg) {\n\
                       let h = |cfg: Cfg| apply(cfg);\n\
                       let _ = h;\n\
                   }";
        let ty = first_closure_type(src);
        assert!(
            matches!(ty, Type::Function { .. }),
            "expected Function (param shadows outer); got {}",
            type_display(&ty)
        );
    }

    #[test]
    fn closure_body_local_let_shadows_outer_non_copy_capture() {
        // A `let cfg = ...` inside the closure body shadows the outer
        // `cfg`. Subsequent `apply(cfg)` inside the body consumes the
        // body-local, not the capture → repeatable.
        let src = "struct Cfg { name: i64 }\n\
                   fn apply(c: Cfg) { }\n\
                   fn make(cfg: Cfg) {\n\
                       let h = || {\n\
                           let cfg = Cfg { name: 7 };\n\
                           apply(cfg)\n\
                       };\n\
                       let _ = h;\n\
                   }";
        let ty = first_closure_type(src);
        assert!(
            matches!(ty, Type::Function { .. }),
            "expected Function (body let shadows capture); got {}",
            type_display(&ty)
        );
    }
}

#[cfg(test)]
mod once_fn_slot_rejection_tests {
    //! Round 12.45 (Step 3) — caller-side rejection of `OnceFn` arguments at
    //! `Fn(...)` and `ref Fn(...)` parameter slots. The slot promises
    //! repeatable invocation; an `OnceFn` value violates that promise. The
    //! diagnostic kind is `OnceFnIntoFnSlot` (E0235); when the argument is
    //! a closure literal that the typechecker has already classified as
    //! once-callable (Step 2), the message also names the consumed capture.
    use super::*;

    fn typecheck_src(src: &str) -> TypeCheckResult {
        let parsed = crate::parse(src);
        let resolved = crate::resolve(&parsed.program);
        crate::typecheck(&parsed.program, &resolved)
    }

    fn errors_of_kind(result: &TypeCheckResult, kind: &TypeErrorKind) -> Vec<TypeError> {
        result
            .errors
            .iter()
            .filter(|e| std::mem::discriminant(&e.kind) == std::mem::discriminant(kind))
            .cloned()
            .collect()
    }

    #[test]
    fn own_fn_slot_rejects_oncefn_closure_literal() {
        // `take(f: Fn())`: owned `Fn()` slot — promises the callee can call
        // `f` any number of times. The closure `|| apply(cfg)` is once-
        // callable (consumes captured non-Copy `cfg`). Step 3 must reject.
        let src = "struct Cfg { name: i64 }\n\
                   fn apply(c: Cfg) { }\n\
                   fn take(f: Fn()) { f() }\n\
                   fn main() {\n\
                       let cfg = Cfg { name: 7 };\n\
                       take(|| apply(cfg));\n\
                   }";
        let result = typecheck_src(src);
        let hits = errors_of_kind(&result, &TypeErrorKind::OnceFnIntoFnSlot);
        assert_eq!(
            hits.len(),
            1,
            "expected exactly one OnceFnIntoFnSlot error; all errors: {:?}",
            result.errors
        );
        assert!(
            hits[0].message.contains("once-callable"),
            "expected message to mention 'once-callable'; got '{}'",
            hits[0].message
        );
        assert!(
            hits[0].message.contains("'cfg'") || hits[0].message.contains("captured binding"),
            "expected message to name the consumed capture 'cfg'; got '{}'",
            hits[0].message
        );
    }

    #[test]
    fn own_fn_slot_accepts_repeatable_closure() {
        // Capture-free closure → `Type::Function` → fits an own `Fn()` slot.
        let src = "fn take(f: Fn()) { f() }\n\
                   fn main() {\n\
                       take(|| { });\n\
                   }";
        let result = typecheck_src(src);
        let hits = errors_of_kind(&result, &TypeErrorKind::OnceFnIntoFnSlot);
        assert!(
            hits.is_empty(),
            "expected no OnceFnIntoFnSlot error for repeatable closure; got: {:?}",
            hits
        );
    }

    #[test]
    fn own_fn_slot_accepts_explicit_ref_prefix_closure_via_binding() {
        // `ref ||` forces repeatable per round 12.6 even when the body
        // would otherwise look consume-y. `ref` is not legal at call
        // sites (parser rejects), so the closure must be let-bound first;
        // the binding gets `Type::Function`, which the own-Fn slot accepts.
        let src = "struct Cfg { name: i64 }\n\
                   fn apply(c: Cfg) { }\n\
                   fn take(f: Fn()) { f() }\n\
                   fn main() {\n\
                       let cfg = Cfg { name: 7 };\n\
                       let h = ref || apply(cfg);\n\
                       take(h);\n\
                   }";
        let result = typecheck_src(src);
        let hits = errors_of_kind(&result, &TypeErrorKind::OnceFnIntoFnSlot);
        assert!(
            hits.is_empty(),
            "expected no OnceFnIntoFnSlot error for ref-prefix-bound closure; got: {:?}",
            hits
        );
    }

    #[test]
    fn ref_fn_slot_rejects_oncefn_closure_literal() {
        // `ref Fn()` slot — same once-callability constraint; the callee
        // can dispatch through the ref repeatedly, so a once-callable
        // closure value must be rejected. The closure literal types as
        // bare `OnceFn()`, the slot is `ref Fn()`; the unwrapped shape
        // (Fn vs OnceFn) flags the once-callability violation rather than
        // the ref-vs-bare regular mismatch.
        let src = "struct Cfg { name: i64 }\n\
                   fn apply(c: Cfg) { }\n\
                   fn take(f: ref Fn()) { }\n\
                   fn main() {\n\
                       let cfg = Cfg { name: 7 };\n\
                       take(|| apply(cfg));\n\
                   }";
        let result = typecheck_src(src);
        let hits = errors_of_kind(&result, &TypeErrorKind::OnceFnIntoFnSlot);
        assert!(
            !hits.is_empty(),
            "expected OnceFnIntoFnSlot error for ref-Fn slot rejection; all errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn cross_call_oncefn_through_fn_slot_rejects_at_inner_site() {
        // Inner `inner(cb: Fn())` — a Fn slot. Outer `forward(cb: Fn())`
        // forwards `cb` to inner. Caller passes a once-callable closure to
        // forward — already a Step-3 violation at the OUTER call site. The
        // test pins that the diagnostic kind fires at the user-visible
        // call site (forward(...)), regardless of how many forwarding
        // hops the typechecker would chase.
        let src = "struct Cfg { name: i64 }\n\
                   fn apply(c: Cfg) { }\n\
                   fn inner(cb: Fn()) { cb() }\n\
                   fn forward(cb: Fn()) { inner(cb) }\n\
                   fn main() {\n\
                       let cfg = Cfg { name: 7 };\n\
                       forward(|| apply(cfg));\n\
                   }";
        let result = typecheck_src(src);
        let hits = errors_of_kind(&result, &TypeErrorKind::OnceFnIntoFnSlot);
        assert!(
            !hits.is_empty(),
            "expected at least one OnceFnIntoFnSlot error in cross-call forwarding; \
             all errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn method_call_fn_slot_rejects_oncefn_closure_literal() {
        // Method-call slot rejection — the same `Fn()` rule applies to
        // method parameter slots, since the dispatch site routes through
        // `check_call_args_with_substitution` and ultimately
        // `check_assignable`.
        let src = "struct Cfg { name: i64 }\n\
                   fn apply(c: Cfg) { }\n\
                   struct Runner { }\n\
                   impl Runner {\n\
                       fn drive(self, f: Fn()) { f() }\n\
                   }\n\
                   fn main() {\n\
                       let cfg = Cfg { name: 7 };\n\
                       let r = Runner { };\n\
                       r.drive(|| apply(cfg));\n\
                   }";
        let result = typecheck_src(src);
        let hits = errors_of_kind(&result, &TypeErrorKind::OnceFnIntoFnSlot);
        assert!(
            !hits.is_empty(),
            "expected OnceFnIntoFnSlot error for method-call Fn-slot rejection; \
             all errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn no_typemismatch_double_report_when_oncefn_slot_violation_fires() {
        // The OnceFnIntoFnSlot kind replaces the generic TypeMismatch for
        // this specific shape — emitting both would double-report. The
        // single-error invariant is what makes the new diagnostic useful;
        // this test pins it.
        let src = "struct Cfg { name: i64 }\n\
                   fn apply(c: Cfg) { }\n\
                   fn take(f: Fn()) { f() }\n\
                   fn main() {\n\
                       let cfg = Cfg { name: 7 };\n\
                       take(|| apply(cfg));\n\
                   }";
        let result = typecheck_src(src);
        let once_hits = errors_of_kind(&result, &TypeErrorKind::OnceFnIntoFnSlot);
        let mismatch_hits = errors_of_kind(&result, &TypeErrorKind::TypeMismatch);
        assert_eq!(once_hits.len(), 1);
        // The TypeMismatch kind may still appear for unrelated reasons,
        // but not for the same span as the OnceFn slot violation.
        let once_span = once_hits[0].span.clone();
        for tm in &mismatch_hits {
            assert!(
                tm.span != once_span,
                "TypeMismatch double-reported at OnceFn slot violation span: {:?}",
                tm
            );
        }
    }

    #[test]
    fn diagnostic_includes_three_concrete_fix_hints() {
        // Round 12.47 (Step 5a) — diagnostic polish. The OnceFnIntoFnSlot
        // message must offer the three concrete fixes documented in the
        // implementation checklist: clone the consumed capture, restructure
        // to keep the closure local, or change the slot type to `OnceFn`.
        // Pin each phrase so future edits to the message body don't silently
        // drop a fix hint.
        let src = "struct Cfg { name: i64 }\n\
                   fn apply(c: Cfg) { }\n\
                   fn take(f: Fn()) { f() }\n\
                   fn main() {\n\
                       let cfg = Cfg { name: 7 };\n\
                       take(|| apply(cfg));\n\
                   }";
        let result = typecheck_src(src);
        let hits = errors_of_kind(&result, &TypeErrorKind::OnceFnIntoFnSlot);
        assert_eq!(hits.len(), 1, "all errors: {:?}", result.errors);
        let msg = &hits[0].message;
        assert!(
            msg.contains("clone the captured value"),
            "missing clone hint; got '{}'",
            msg
        );
        assert!(
            msg.contains("invoke the closure locally") || msg.contains("restructure"),
            "missing restructure-locally hint; got '{}'",
            msg
        );
        assert!(
            msg.contains("`OnceFn(...)`") || msg.contains("OnceFn(...)"),
            "missing OnceFn-slot-change hint; got '{}'",
            msg
        );
    }
}

#[cfg(test)]
mod once_fn_container_slot_tests {
    //! Round 12.46 (Step 4) — once-callability rejection at container element
    //! slots, plus surface `OnceFn(...)` annotation acceptance and for-loop
    //! iteration parity over `Vec[Fn]` and `Vec[OnceFn]`. The active rejection
    //! is at the *insert* (`.push`); iteration falls out for free because
    //! `for f in vec` types `f` as the element type, and Step 1's `Call`
    //! dispatch already accepts both `Function` and `OnceFunction` callees.
    use super::*;

    fn typecheck_src(src: &str) -> TypeCheckResult {
        let parsed = crate::parse(src);
        let resolved = crate::resolve(&parsed.program);
        crate::typecheck(&parsed.program, &resolved)
    }

    fn errors_of_kind(result: &TypeCheckResult, kind: &TypeErrorKind) -> Vec<TypeError> {
        result
            .errors
            .iter()
            .filter(|e| std::mem::discriminant(&e.kind) == std::mem::discriminant(kind))
            .cloned()
            .collect()
    }

    #[test]
    fn vec_fn_push_rejects_oncefn_closure_literal() {
        // `Vec[Fn()]` element slot — pushing a once-callable closure must
        // reject at the call site of `.push` because the slot promises
        // repeatable invocation. Routes through the new Vec.push slot
        // dispatch into `check_assignable`, which fires `OnceFnIntoFnSlot`
        // (E0235) via Step 3's logic.
        let src = "struct Cfg { name: i64 }\n\
                   fn apply(c: Cfg) { }\n\
                   fn main() {\n\
                       let cfg = Cfg { name: 7 };\n\
                       let mut v: Vec[Fn()] = Vec.new();\n\
                       v.push(|| apply(cfg));\n\
                   }";
        let result = typecheck_src(src);
        let hits = errors_of_kind(&result, &TypeErrorKind::OnceFnIntoFnSlot);
        assert_eq!(
            hits.len(),
            1,
            "expected exactly one OnceFnIntoFnSlot error at Vec[Fn].push site; \
             all errors: {:?}",
            result.errors
        );
        assert!(
            hits[0].message.contains("once-callable"),
            "expected 'once-callable' in message; got '{}'",
            hits[0].message
        );
        assert!(
            hits[0].message.contains("'cfg'") || hits[0].message.contains("captured binding"),
            "expected consumed-capture name 'cfg' in message; got '{}'",
            hits[0].message
        );
    }

    #[test]
    fn vec_fn_push_accepts_repeatable_closure() {
        // Capture-free closure → `Type::Function` → fits `Vec[Fn()]` element.
        let src = "fn main() {\n\
                       let mut v: Vec[Fn()] = Vec.new();\n\
                       v.push(|| { });\n\
                   }";
        let result = typecheck_src(src);
        let hits = errors_of_kind(&result, &TypeErrorKind::OnceFnIntoFnSlot);
        assert!(
            hits.is_empty(),
            "expected no OnceFnIntoFnSlot for repeatable closure push; got: {:?}",
            hits
        );
        // Also confirm no TypeMismatch crept in for the push arg.
        let mismatch = errors_of_kind(&result, &TypeErrorKind::TypeMismatch);
        assert!(
            mismatch.is_empty(),
            "expected no TypeMismatch errors; got: {:?}",
            mismatch
        );
    }

    #[test]
    fn vec_oncefn_push_accepts_once_callable_closure() {
        // Surface `OnceFn(...)` annotation (round 12.46 Step 4) lets the
        // user opt into a Vec whose element slot accepts once-callable
        // closures. Pushing a closure that consumes a captured non-Copy
        // binding now fits the slot — `OnceFunction` ⇄ `OnceFunction`.
        let src = "struct Cfg { name: i64 }\n\
                   fn apply(c: Cfg) { }\n\
                   fn main() {\n\
                       let cfg = Cfg { name: 7 };\n\
                       let mut v: Vec[OnceFn()] = Vec.new();\n\
                       v.push(|| apply(cfg));\n\
                   }";
        let result = typecheck_src(src);
        let hits = errors_of_kind(&result, &TypeErrorKind::OnceFnIntoFnSlot);
        assert!(
            hits.is_empty(),
            "expected no OnceFnIntoFnSlot for OnceFn-into-OnceFn slot; got: {:?}",
            hits
        );
        let mismatch = errors_of_kind(&result, &TypeErrorKind::TypeMismatch);
        assert!(
            mismatch.is_empty(),
            "expected no TypeMismatch for OnceFn-into-OnceFn slot; got: {:?}",
            mismatch
        );
    }

    #[test]
    fn vec_oncefn_slot_accepts_function_closure_via_subsumption() {
        // Item 131 sub-step 3 (bidirectional subsumption): a Function-typed
        // closure (repeatable) flows into a Vec[OnceFn] slot. Fn is a subtype
        // of OnceFn — a repeatable callable trivially satisfies the
        // callable-once contract. `is_subtype(OnceFunction, Function)` returns
        // true at check_assignable, so neither TypeMismatch nor
        // OnceFnIntoFnSlot fires.
        //
        // Pre-sub-step-3 this fired TypeMismatch (the old test name was
        // `vec_oncefn_annotation_lowers_to_once_function_type` — which
        // observed the rejection as a side effect of the symmetric
        // types_compatible cross-pair rejection). The annotation is still
        // correctly lowered to OnceFunction; what changed is that the upward
        // direction is now admitted at the slot.
        let src = "fn main() {\n\
                       let mut v: Vec[OnceFn() -> i64] = Vec.new();\n\
                       v.push(|| 7);\n\
                   }";
        let result = typecheck_src(src);
        let mismatch = errors_of_kind(&result, &TypeErrorKind::TypeMismatch);
        assert!(
            mismatch.is_empty(),
            "Function → OnceFn slot is admitted by sub-step 3 subsumption; \
             expected no TypeMismatch but got: {:?}",
            mismatch
        );
        let once_hits = errors_of_kind(&result, &TypeErrorKind::OnceFnIntoFnSlot);
        assert!(
            once_hits.is_empty(),
            "OnceFnIntoFnSlot must not fire for Function → OnceFn (only the \
             reverse direction is the round-12.45 case); got: {:?}",
            once_hits
        );
    }

    #[test]
    fn for_loop_over_vec_fn_invokes_repeatedly() {
        // Iteration over `Vec[Fn()]` yields `f: Fn()` per iteration. The
        // body's `f()` call dispatches against `Type::Function`, which
        // Step 1 made first-class for callee dispatch. No OnceFn ever
        // appears in this path because the slot at insert time was Fn.
        let src = "fn main() {\n\
                       let mut v: Vec[Fn()] = Vec.new();\n\
                       v.push(|| { });\n\
                       v.push(|| { });\n\
                       for f in v {\n\
                           f();\n\
                       }\n\
                   }";
        let result = typecheck_src(src);
        assert!(
            result.errors.is_empty(),
            "expected clean typecheck; got errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn for_loop_over_vec_oncefn_invokes_each_element() {
        // Iteration over `Vec[OnceFn()]` yields `f: OnceFn()` per
        // iteration. The typechecker's Call dispatch matches
        // `Function | OnceFunction`, so the body's `f()` succeeds. Each
        // iteration owns its element (move semantics) so calling once is
        // fine; the body invokes f exactly once.
        let src = "struct Cfg { name: i64 }\n\
                   fn apply(c: Cfg) { }\n\
                   fn main() {\n\
                       let cfg1 = Cfg { name: 1 };\n\
                       let cfg2 = Cfg { name: 2 };\n\
                       let mut v: Vec[OnceFn()] = Vec.new();\n\
                       v.push(|| apply(cfg1));\n\
                       v.push(|| apply(cfg2));\n\
                       for f in v {\n\
                           f();\n\
                       }\n\
                   }";
        let result = typecheck_src(src);
        assert!(
            result.errors.is_empty(),
            "expected clean typecheck for Vec[OnceFn] iter+invoke; got: {:?}",
            result.errors
        );
    }

    #[test]
    fn vec_fn_push_oncefn_through_intermediate_binding_still_rejects() {
        // The closure is bound to a let first, then pushed. The let's
        // binding type infers to OnceFunction (Step 2) and the push slot
        // check sees OnceFunction → Function and fires E0235. This pins
        // that the Vec.push slot check does not depend on the argument
        // being a closure literal — any once-callable value flowing into
        // the slot rejects.
        let src = "struct Cfg { name: i64 }\n\
                   fn apply(c: Cfg) { }\n\
                   fn main() {\n\
                       let cfg = Cfg { name: 7 };\n\
                       let h = || apply(cfg);\n\
                       let mut v: Vec[Fn()] = Vec.new();\n\
                       v.push(h);\n\
                   }";
        let result = typecheck_src(src);
        let hits = errors_of_kind(&result, &TypeErrorKind::OnceFnIntoFnSlot);
        assert!(
            !hits.is_empty(),
            "expected OnceFnIntoFnSlot when pushing a let-bound once-callable \
             closure into Vec[Fn]; all errors: {:?}",
            result.errors
        );
    }
}
