//! Contract, refinement, distinct-type and secret-type state.
//!
//! Sixth slice of the Phase-2 `Codegen` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Groups the
//! state behind design.md § Contracts and its neighbours:
//!
//! - refinement types — base type, generic params and the predicate `Expr`
//!   per refinement name;
//! - distinct types — the base type per distinct name;
//! - the contract frame for the function being compiled: its `ensures`
//!   clauses, the result type they refer to, the `old(..)` snapshot values
//!   keyed by call span, the method invariants in force, and the self type
//!   when compiling a constructor's invariant check;
//! - `strip_contracts`, the release-build gate;
//! - secret types — whether the stdlib `Secret` is in play and the inner
//!   type per call site.
//!
//! The four `current_*` fields are per-function, but they are *contract*
//! state rather than LLVM frame state, so they stay with their subsystem
//! rather than going to cluster 14 (`FnCtx`) — the opposite call from the
//! one made for `current_fn_sret_param` in `target_abi.rs`, and for the
//! same reason: keep a subsystem's state in one place.
//!
//! The code that consumes all of this lives in the sibling `contracts.rs`,
//! which is why the data module is named `contract_state`.
//!
//! Accessed as `self.contract_state.<name>` from the sibling
//! `impl Codegen` modules.

use std::collections::HashMap;

use crate::ast::TypeExpr;
use crate::resolver::SpanKey;

/// Contract / refinement / distinct / secret state.
pub(crate) struct ContractState<'ctx> {
    /// Refinement type alias name → its base `TypeExpr` (`type Email =
    /// String where …` → the `String` type expr). Populated from the
    /// program's `Item::TypeAlias`es that carry a `where` predicate.
    /// Consulted by `llvm_type_for_type_expr` / `llvm_type_for_name` so a
    /// refinement lowers to its *base*'s layout — without this a refinement
    /// over a non-`i64` base would hit the `i64` fall-through default and
    /// silently mis-size the slot (phase-9 step 4). A refinement is
    /// layout-identical to its base (no runtime tag), so this is a pure
    /// alias resolution.
    pub(crate) refinement_bases: HashMap<String, crate::ast::TypeExpr>,
    /// Refinement type alias name → the ordered names of its generic
    /// parameters (`type NonEmpty[T] = Vec[T] where …` → `["T"]`). Parallel
    /// to `refinement_bases`, which stores only the *uninstantiated* base
    /// (`Vec[T]`). When a refinement alias is used at a concrete arity
    /// (`NonEmpty[EnrichedRow]`), `resolve_type_alias_te` zips these
    /// param names against the use-site generic args and substitutes them
    /// into the base so the binding registers as `Vec[EnrichedRow]` (correct
    /// element type), not `Vec[T]` (which would mis-size the element as the
    /// `i64` unknown-name fall-through). Empty for non-generic refinements.
    pub(crate) refinement_generic_params: HashMap<String, Vec<String>>,
    /// Distinct-type name → its base `TypeExpr` (`distinct type UserId = i64`
    /// → the `i64` type expression). A distinct type is layout-identical to
    /// its base (zero-cost wrapper, no runtime tag), so codegen lowers it to
    /// the base's LLVM layout — consulted ONLY at the pure-layout sites
    /// (`llvm_type_for_type_expr`, `llvm_type_for_name`), NOT in
    /// `type_alias_base_name`: unlike a refinement, a distinct type keeps its
    /// own name for value-level method dispatch (no base-method deref).
    /// Populated from `Item::DistinctType`. design.md § Distinct Types.
    pub(crate) distinct_bases: HashMap<String, crate::ast::TypeExpr>,
    /// Refinement name → its predicate `Expr` (`type Even = i64 where
    /// self % 2 == 0` → the `self % 2 == 0` expression). Populated from
    /// `Item::TypeAlias.refinement`, parallel to `refinement_bases`. Drives
    /// the runtime predicate check emitted at `x as Refined` cast sites and
    /// `Refined.try_from(x)` calls (phase-9 step 5c): the predicate is
    /// compiled with `self` bound to the candidate value, then branched on.
    pub(crate) refinement_predicates: HashMap<String, crate::ast::Expr>,
    /// The `ensures` clauses of the function currently being compiled
    /// (design.md § Contracts). Set at `compile_function` entry, cleared at
    /// exit; consumed by `emit_ensures_checks`, which is emitted inline
    /// before each `ret` (the tail return + every explicit `return`).
    pub(crate) current_contract_ensures: Vec<crate::ast::EnsuresClause>,
    /// The return `TypeExpr` of the function currently being compiled, set
    /// alongside `current_contract_ensures`. `emit_ensures_checks` uses it to
    /// register the `result` binding's type (via `register_var_from_type_expr`)
    /// so a `result.field` access inside an `ensures` clause resolves the
    /// struct field index — without it, field access on `result` can't find
    /// the struct name and reads the wrong slot (the `ensures(result)
    /// result.q == old(...)` codegen bug surfaced by the Weave dogfood).
    /// `None` for a `()`-returning function or when contracts are stripped.
    pub(crate) current_contract_result_type: Option<crate::ast::TypeExpr>,
    /// `old(arg)` pre-state snapshots for the current function, captured at
    /// entry and keyed by the arg expression's span. Read back by the
    /// `old(...)` interception in `compile_call` when emitting the
    /// postcondition (design.md § Contracts rule 4).
    pub(crate) contract_old_snapshots: HashMap<SpanKey, inkwell::values::BasicValueEnum<'ctx>>,
    /// Struct/impl `invariant` predicates that must hold at every exit of the
    /// method currently being compiled (design.md § Contracts rule 3). Set at
    /// `compile_function` entry for impl-method functions — `impl invariant`
    /// always, plain `invariant` only when the method is `pub` — and cleared at
    /// exit. Consumed by `emit_invariant_checks`, emitted inline before each
    /// `ret` (same exit points as `ensures`), with `self` already bound as the
    /// method's first parameter. Empty for free functions and non-pub methods
    /// of invariant-free structs.
    pub(crate) current_method_invariants: Vec<crate::ast::Expr>,
    /// `Some(type_name)` when the function currently being compiled is a
    /// *constructor* — a `pub` associated function returning `Self`/the type,
    /// which has no `self` parameter (design.md § Contracts: "Constructors ...
    /// also check the invariant at their return point"). When set,
    /// `emit_invariant_checks` binds the *return value* as `self` before
    /// evaluating `current_method_invariants`, since the construction boundary
    /// has no receiver. `None` for methods (where `self` is parameter 0) and
    /// for free / non-constructor associated functions. Set at
    /// `compile_function` entry, cleared at exit.
    pub(crate) constructor_invariant_self_type: Option<String>,
    /// When `true`, all contract machinery is elided from the emitted module
    /// (design.md § Contracts: "stripped in release"): `requires` / `ensures`
    /// checks, `old(...)` pre-state capture, and struct/impl `invariant`
    /// checks are not emitted, paying zero runtime cost. Defaults from
    /// `read_strip_contracts_env` (`KARAC_STRIP_CONTRACTS`) at construction;
    /// `set_strip_contracts` overrides it (used by the release-build path and
    /// by IR tests that must force the decision without touching global env).
    /// The gate lives at the three contract *setup* sites in
    /// `compile_function` — suppressing setup makes every downstream emit site
    /// a natural no-op, and `old(...)` (which lives only inside `ensures`
    /// bodies) is never reached because those bodies aren't compiled.
    pub(crate) strip_contracts: bool,
    /// True when the `std.secret` `Secret[T]` wrapper is in scope for this
    /// compilation — i.e. a `StructDef` named `Secret` carrying
    /// `stdlib_origin` was registered (the gated `import std.secret.{Secret};`
    /// path). Read by the derived-Display field walk (`build_struct_display_parts`)
    /// to emit `Secret`-typed fields as the literal `<redacted>` instead of
    /// leaking the wrapped value. Scoped to the stdlib type so a user's
    /// unrelated `struct Secret` renders normally (a `Secret` import and a
    /// user `struct Secret` cannot coexist — the resolver rejects the
    /// duplicate name — so the flag is unambiguous within one program).
    pub(crate) secret_type_is_stdlib: bool,
    /// Inner `T` of every `Secret[T]`-typed expression, keyed by span —
    /// populated from `Program.secret_inner_types`. Read by the `ct_eq`
    /// intercept (`compile_method_call`) to resolve a `Secret[T]` receiver's
    /// inner type (its `bool` result has no `ref_return_inner_types` entry) and
    /// gate the constant-time compare to the `Secret[String]` inner v1 supports.
    pub(crate) secret_inner_types: HashMap<(usize, usize), TypeExpr>,
}
