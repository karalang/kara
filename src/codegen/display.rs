//! `Display` lowering state.
//!
//! Fourth slice of the Phase-2 `Codegen` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Groups the
//! tables the `Display` / interpolation lowering reads: the per-call-site
//! payload types recorded by the typechecker for `Option` / `Result`,
//! tuple, `Vec`, `Map` and `Set` operands, the sorted-collection call
//! sites, the set of enums with a baked `#[derive(Display)]` impl, and the
//! per-type cache of emitted display functions.
//!
//! Field names keep their `display_` prefix even though the sub-struct
//! already says it — this slice is pure motion, and renaming them would
//! touch every access site for cosmetic gain.
//!
//! Accessed as `self.display.<name>` from the sibling `impl Codegen`
//! modules.

use std::collections::{HashMap, HashSet};

use inkwell::values::FunctionValue;

use crate::ast::TypeExpr;

/// Tables consumed by the `Display` / string-interpolation lowering.
pub(crate) struct Display<'ctx> {
    /// Names of seeded baked-stdlib enums that carry `#[derive(Display)]`
    /// (`IoError`, `VarError`) and so must render through the generic
    /// value-driven Display path (`emit_enum_display_fn`) like a user enum —
    /// the f-string / `println` / `to_string` dispatch in
    /// `expr_user_enum_name_any` excludes `seeded_enum_names` (the other
    /// seeded enums route through bespoke paths), so this set re-admits the
    /// Display-deriving ones. Populated once from `STDLIB_PROGRAMS` in
    /// `seed_builtin_enum_layouts`.
    pub(crate) baked_display_enum_names: HashSet<String>,
    /// Full `Option[T]` / `Result[T, E]` `TypeExpr` of every such-typed
    /// expression, keyed by span — populated from
    /// `Program.display_option_result_types`. Lets `try_compile_option_result_display`
    /// render an Option/Result *call result* (`f"{cache.get(1)}"`,
    /// `println(opt_fn())`) via its concrete per-payload Display fn; the
    /// variable case keys off `var_option_payload_te` instead. Call-result
    /// half of B-2026-07-08-9.
    pub(crate) display_option_result_types: HashMap<(usize, usize), TypeExpr>,
    /// Full anonymous-tuple `TypeExpr` of every tuple-typed expression, keyed
    /// by span — populated from `Program.display_tuple_types`. Lets
    /// `try_compile_tuple_display` render a WHOLE tuple value in an f-string /
    /// `println` (`f"{t}"`, `println(pair)`) via `emit_tuple_display_fn`,
    /// matching the interpreter's `(a, b)` format (B-2026-07-18-14). Covers both
    /// a tuple variable and a tuple call-result uniformly.
    pub(crate) display_tuple_types: HashMap<(usize, usize), TypeExpr>,
    /// ELEMENT `TypeExpr` of every `Vec[T]`-typed expression, keyed by span —
    /// populated from `Program.display_vec_types`. Lets
    /// `try_compile_vec_display` render a Vec with no variable name to key on
    /// (a fresh literal, a call result) through the same per-element Display fn
    /// the identifier path uses, instead of falling through to the value-kind
    /// arms where a Vec is indistinguishable from a String (B-2026-07-28-12).
    pub(crate) display_vec_types: HashMap<(usize, usize), TypeExpr>,
    /// B-2026-08-14-31 — key/value types of every `Map`/`SortedMap` expression
    /// and element types of every `Set`/`SortedSet` one, keyed by span
    /// (`Program.display_map_types` / `display_set_types`). The Map/Set
    /// siblings of `display_vec_types`, letting a non-identifier collection
    /// render like a bound one instead of printing its control pointer.
    pub(crate) display_map_types: HashMap<(usize, usize), (TypeExpr, TypeExpr)>,
    pub(crate) display_set_types: HashMap<(usize, usize), TypeExpr>,
    /// B-2026-08-14-35 — the subset of the two tables above whose surface type
    /// was `SortedMap` / `SortedSet`. Selects the ascending-order renderer and
    /// the `SortedMap{` / `SortedSet{` prefix.
    pub(crate) display_sorted_collection_spans: std::collections::HashSet<(usize, usize)>,
    /// Per-type Display function cache. Keyed by the canonical type name
    /// (e.g. `"i64"`, `"String"`, `"Vec_i64"`, `"Map_String_i64"`). Each
    /// emitted fn has signature `void karac_display_<typename>(ptr)` and
    /// writes characters to stdout via `printf` with no trailing newline.
    /// The pointer-by-reference convention is uniform across every type so
    /// callers don't need per-type calling conventions; primitives load the
    /// value, structs extract fields, opaque ptrs load the handle.
    ///
    /// `dead_code` is allowed because subtasks 1+2 of the Display canonical
    /// bullet ship the machinery + primitive Display fns ahead of subtasks
    /// 3-7 which add the callers (Vec/Map/Set/Tuple Display fns + the
    /// `compile_print` integration). Remove the allow when subtask 7 lands.
    #[allow(dead_code)]
    pub(crate) display_fn_cache: HashMap<String, FunctionValue<'ctx>>,
}
