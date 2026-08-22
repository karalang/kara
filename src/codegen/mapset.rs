//! `Map` / `Set` / `VecDeque` element- and key-type state.
//!
//! Eighth slice of the Phase-2 `Codegen` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Groups the
//! per-variable type tables the associative-container lowering needs,
//! which cannot be recovered from the LLVM type alone — a `Map` handle is
//! an opaque pointer, so K and V have to be carried alongside:
//!
//! - `Map` — key/value LLVM types, key type name and `TypeExpr`, value
//!   body `TypeExpr`s, and the monomorphized method set per handle;
//! - `Set` — element LLVM type, type name and `TypeExpr`, plus the
//!   sorted-collection variable set;
//! - `VecDeque` — the head locals and their slots;
//! - the fresh-temp receiver map/set types, and the two `Map`-lowering
//!   flags (`map_tag_override`, `map_lookup_probe`,
//!   `pending_map_insert_old_dec`).
//!
//! Named `mapset` to avoid the sibling `maps.rs`, which holds the
//! behaviour this data feeds.
//!
//! Accessed as `self.mapset.<name>` from the sibling `impl Codegen`
//! modules.

use std::collections::{HashMap, HashSet};

use inkwell::types::BasicTypeEnum;
use inkwell::values::PointerValue;

use super::mono;
use super::state::MapMonoMethods;
use crate::ast::TypeExpr;

/// Per-variable `Map` / `Set` / `VecDeque` type tables.
pub(crate) struct MapSet<'ctx> {
    /// Staging slot — set by `compile_stmt`'s Let / Expr arms when the
    /// surrounding statement discards the `Option[V]` result of a
    /// `Map.insert(k, v)` call (i.e. `let _ = m.insert(...)` or a bare
    /// `m.insert(...)` statement). `compile_map_method`'s `insert` arm
    /// reads + clears this flag to decide whether to emit a follow-up
    /// `rc_dec` on the displaced shared value (the `Some(old)` payload
    /// that no one will hold the +1 of). Without the dec the prior
    /// bucket value's refcount stays >0 on every overwrite and the
    /// shared object leaks. When the result *is* bound (`let prev =
    /// m.insert(...)`), the caller's scope-exit cleanup on `prev`
    /// handles the +1; the discard path is the only one that needs
    /// the receive-site dec.
    pub(crate) pending_map_insert_old_dec: bool,
    /// B-2026-08-05-5 override for the mono probe loops' control-byte test.
    /// `None` (unset) uses the measured per-site policy in
    /// [`Codegen::map_tag_compare`]; `KARAC_MAP_TAG=0` forces the tag OFF at
    /// every site and `=1` forces it ON at every site, the latter restoring
    /// 58412d9f's pre-fix behaviour exactly.
    ///
    /// Kept as an A/B lever rather than deleted with the fix: it is the ONLY
    /// instrument that isolates the tag compare. A commit-to-commit comparison
    /// against 58412d9f does NOT, because that commit also restructured
    /// `mono.rs` and changed the `keys()` walk — measuring the tag that way
    /// produced a 6.1%-faster reading against this lever's 1.9%-slower one on
    /// the same host and kata, and every fix-sizing estimate taken from the
    /// commit pair was wrong as a result. Size the tag with the lever.
    ///
    /// NOTE the override is deliberately blunt (all sites, both directions) so
    /// an A/B measures one variable. The shipped policy is per-site.
    pub(crate) map_tag_override: Option<bool>,
    /// B-2026-08-07-16: which cursor form the three mono LOOKUP probes use.
    /// `KARAC_MAP_PROBE=unbounded` drops their `i >= cap` test and `=slotwalk`
    /// additionally walks the bucket index itself; anything else (including
    /// unset) is [`MapLookupProbe::Bounded`], the shipped form.
    ///
    /// Kept as an A/B lever for the same reason `map_tag_override` is: this is
    /// the ONLY instrument that isolates the probe form. A commit-to-commit
    /// comparison does not — that is exactly the mistake this bug's first
    /// measurement made, comparing across 11 unrelated upstream commits and
    /// producing a number that meant nothing. One compiler binary, three
    /// forms, same tree.
    pub(crate) map_lookup_probe: mono::MapLookupProbe,
    /// Per-fresh-temp `Map`/`Set` receiver read-method MethodCall →
    /// `Map[K,V]` / `Set[T]` `TypeExpr` side-table — populated from
    /// `Program.temp_recv_mapset_types`. Codegen materializes the handle,
    /// registers K/V (or elem), drop-tracks the handle (`FreeMapHandle`), and
    /// re-dispatches through `compile_map_method` / `compile_set_method`
    /// (general-owned-temp-tracking spike, slice 3d).
    pub(crate) temp_recv_mapset_types: HashMap<(usize, usize), TypeExpr>,
    /// `Map[K, V]` instantiation per let-bound variable whose VALUE-bodies
    /// walk registered (`__karac_dropelems_map_*`) — the rebind fallback in
    /// the shared static chain, exactly like `optres_var_payload_tes`.
    pub(crate) map_val_bodies_tes: HashMap<String, TypeExpr>,
    /// Per-function deque locals eligible for the O(1) `pop_front` head-index
    /// lowering (`crate::deque_head`, B-2026-07-30-5). For a name in this set
    /// the `{ptr, len, cap}` header is REINTERPRETED: `len` is the end index,
    /// not the count, and the live range is `data[head..len]` where `head`
    /// lives in `deque_head_slots`. The in-memory layout is unchanged, which
    /// is what keeps every generic Vec path (drop, clone, par-copy) correct —
    /// the analysis only admits locals no such path can reach.
    pub(crate) deque_head_locals: HashMap<String, HashSet<String>>,
    /// Entry-block `i64` alloca holding `head` for each eligible deque local of
    /// the function being compiled. Cleared per function; a name present here
    /// is one the head-aware method arms must use.
    pub(crate) deque_head_slots: HashMap<String, PointerValue<'ctx>>,
    /// Per-variable Map key LLVM type (variable name → K LLVM type).
    pub(crate) map_key_types: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Per-variable Map value LLVM type (variable name → V LLVM type).
    pub(crate) map_val_types: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Per-variable Map key type name string (e.g. "i64", "String") for hash/eq fn selection.
    pub(crate) map_key_type_names: HashMap<String, String>,
    /// Per-Map-variable key-`TypeExpr` side-table (parallels
    /// `var_elem_type_exprs` for the key slot). Used by `compile_for_map_var`
    /// to register the per-iteration `k` binding when iterating with a tuple
    /// pattern `for (k, v) in m`.
    pub(crate) map_key_type_exprs: HashMap<String, TypeExpr>,
    /// Which hasher each `Map` / `Set` binding was DECLARED with — the
    /// `Map[K, V, H]` / `Set[T, H]` selector (B-2026-08-21-6). Recorded
    /// alongside `map_key_type_exprs` from the same container `TypeExpr`, and
    /// read at `karac_map_new` time to pick which per-key-type hash function
    /// goes into the control block.
    ///
    /// Written for EVERY map/set binding, including the ones that take the
    /// default: an entry that is merely absent would let an outer
    /// `Map[K, V, FxBuildHasher]` leak its hasher into an inner shadowing
    /// binding of the same name that asked for the default.
    pub(crate) map_hashers: HashMap<String, crate::hasher_kind::HasherKind>,
    /// Per-variable Set element LLVM type (variable name → T LLVM type).
    /// Mirrors `map_key_types` — `Set[T]` lowers to `Map[T, ()]` at codegen,
    /// reusing the `karac_map_*` C runtime, but the surface type identity is
    /// kept distinct so codegen can pick the right method dispatch and the
    /// Display fn can pick the `Set{...}` brace style.
    pub(crate) set_elem_types: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Variable names bound to a `SortedSet[T]` / `SortedMap[K, V]`. These share
    /// `Set`/`Map`'s `KaracMap`-backed storage (so they live in
    /// `set_elem_types` / `map_key_types` and reuse `compile_set_method` /
    /// `compile_map_method` for all order-independent ops), but must observe
    /// their keys in ASCENDING order at iteration (`for`, `keys`/`values`/
    /// `entries`) and at `min`/`max`. Codegen consults this set at those
    /// observation points to inject a sort; empty for plain `Set`/`Map`.
    pub(crate) sorted_collection_vars: std::collections::HashSet<String>,
    /// Per-variable Set element type name string (e.g. `"i64"`, `"String"`)
    /// for hash/eq fn selection. Mirrors `map_key_type_names`.
    pub(crate) set_elem_type_names: HashMap<String, String>,
    /// Per-variable Set element-`TypeExpr` side-table. Mirrors
    /// `map_key_type_exprs` and is consulted alongside it by Set-aware paths
    /// (`compile_for_set_var`, Set Display fn) so compound element types
    /// (`Set[(i64, String)]`, `Set[Vec[T]]`) compose through the
    /// TypeExpr-aware hash/eq/Display paths.
    pub(crate) set_elem_type_exprs: HashMap<String, TypeExpr>,
    /// Per-(K, V) cache of monomorphized `Map[K, V]` method symbols.
    /// Keyed by the mangled `"{key_mangle}_{val_mangle}"` token (e.g.
    /// `"i64_i64"`) produced by `mono_map_cache_key`. Lazily populated
    /// by `get_or_emit_map_mono_methods` on the first request for a
    /// given K/V tuple. Per-method `FunctionValue`s have `LinkOnceODR`
    /// linkage so cross-crate / cross-TU duplicates collapse at link
    /// time (locked design § 3.2). Slice 1 ships `Map[i64, i64]` only;
    /// the gating predicate `should_use_mono_map_for` returns `false`
    /// for every other K/V tuple, leaving them on the erased fallback
    /// per § 3.6.
    pub(crate) map_mono_methods: HashMap<String, MapMonoMethods<'ctx>>,
}
