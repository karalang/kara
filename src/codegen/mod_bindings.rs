//! Module-level (top-level) binding state.
//!
//! The program's top-level `let` and `const` surface: `consts` (named
//! compile-time constants), `module_bindings` (top-level bindings and
//! their global slots), `module_binding_types`, and the three init
//! queues the synthesized module-init function drains (`map_set`, `once`,
//! and computed initializers). Named `mod_bindings` because
//! `module_bindings.rs` is an existing behaviour module (the same
//! collision class the provider cluster hit). Extracted from `Codegen`
//! as a cluster-15 sub-slice of the state-decomposition spike; see
//! `docs/spikes/state-decomposition-codegen-methodcall.md`.

use std::collections::HashMap;

use super::module_bindings;
use crate::ast::Expr;

pub(crate) struct ModBindings<'ctx> {
    /// Top-level `const NAME: T = value` declarations, populated by
    /// `compile_program` from `Item::ConstDecl` items before any function
    /// body is compiled. Key: const name. Value: the const's value
    /// expression. References to a const inside function bodies (parsed as
    /// `ExprKind::Identifier(name)` for bare uses) re-compile this stored
    /// expression at the use site, leaving constant folding to LLVM.
    /// Cycles are precluded upstream by the typechecker's const-evaluation
    /// pass (`check_const_decl`).
    pub(crate) consts: HashMap<String, Expr>,
    /// Module-level `let` / `let mut` bindings — slice 9 of the
    /// phase-8 module-let work (design.md §1278-1330). Populated by
    /// `declare_module_bindings` before any function body is
    /// compiled. Identifier loads in function bodies short-circuit
    /// to a real LLVM `load` from the global via
    /// `try_load_module_binding`; assignments / compound-assigns
    /// route through `try_store_module_binding`. Distinct from
    /// `consts`, which inlines the value expression at each use site
    /// — module bindings need real LLVM globals so `let mut`
    /// mutation is observable across functions and `#[thread_local]`
    /// gets the per-task disjoint instance.
    pub(crate) module_bindings: HashMap<String, module_bindings::ModuleBindingInfo<'ctx>>,
    /// Module bindings initialised by `Map.new()` / `Set.new()`, in
    /// declaration order. Their globals are emitted as a placeholder
    /// `null` `ptr` (the empty Map/Set is NOT a zero-shaped constant —
    /// `karac_map_new` installs hash seeds + a vtable), and filled by
    /// the `__karac_static_init` prologue that runs before `main`'s
    /// body. `bool` is `true` for `Set.new()` (val_size = 0). Populated
    /// by `declare_module_bindings`; consumed by
    /// `finalize_module_binding_static_init`.
    pub(crate) map_set_module_inits: Vec<(String, bool)>,
    /// Module-level `OnceLock[T]` bindings (`let CONFIG: OnceLock[T] =
    /// OnceLock.new()`) — the canonical late-bound global (set once in startup,
    /// read everywhere). Like the Map/Set entries these need a runtime handle
    /// (`karac_runtime_once_new`), so they take the placeholder-null-ptr-global
    /// plus static-init-prologue path. Never freed — a module binding lives for
    /// the whole process (reachable through the global at exit; LSan-clean). Only
    /// `OnceLock` reaches here: `OnceCell` is rejected at module scope by the
    /// typechecker (`E_ONCE_CELL_AT_MODULE_SCOPE`).
    pub(crate) once_module_inits: Vec<String>,
    /// Module bindings whose initializer is a COMPUTED / cross-referencing
    /// expression (`let DOUBLED: i64 = COUNT * 2;`, referencing another module
    /// binding, or any arithmetic the const-shape path can't fold) — the shapes
    /// `module_binding_init` returns `None` for. Like the Map/Set entries, the
    /// global is declared as a zero placeholder and the real value is computed
    /// in `__karac_static_init` (before `main`) by `compile_expr`-ing the stored
    /// initializer and storing the result — which handles `Identifier`→load the
    /// referenced global and `Binary`→arithmetic. Declaration order is preserved
    /// so a binding can reference an earlier one (B-2026-07-11-16).
    pub(crate) computed_module_inits: Vec<(String, crate::ast::Expr)>,
    /// Inferred type of each module-binding value expr, keyed by binding name
    /// (from `program.module_binding_types`, the typechecker's `expr_types`).
    /// Sizes the placeholder global for a COMPUTED, un-annotated binding
    /// (`let DOUBLED = COUNT * 2;`) when there is no `: TYPE` to use.
    pub(crate) module_binding_types: std::collections::HashMap<String, crate::ast::TypeExpr>,
}
