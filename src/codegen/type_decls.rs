//! Type-declaration tables: what the program's nominal types LOOK like.
//!
//! The per-type-name registries `declare_structs` and its siblings seed and
//! every downstream phase reads: struct/shared/union LLVM types and their
//! field name/type tables, generic params, enum layouts and unit-variant
//! sets, the per-instantiation enum tables, `Ord`-orderability, moved-field
//! drop bodies, and the prelude-shadowing set. Keyed by TYPE name (plus
//! mangled instantiation keys for the enum-inst pair) — program-wide facts
//! from declarations, not per-function state. Extracted from `Codegen` as a
//! cluster-15 sub-slice of the state-decomposition spike; see
//! `docs/spikes/state-decomposition-codegen-methodcall.md`.

use std::collections::{HashMap, HashSet};

use inkwell::types::{BasicTypeEnum, StructType};

use super::state::{EnumLayout, SharedTypeInfo};
use crate::ast::TypeExpr;

pub(crate) struct TypeDecls<'ctx> {
    /// LLVM struct types for Kāra structs (struct name → LLVM type).
    pub(crate) struct_types: HashMap<String, StructType<'ctx>>,
    /// Every `shared` / `par` type name the program declares — NAME ONLY, and
    /// collected before any layout pass runs. Read by `llvm_type_for_name`
    /// alongside `shared_types`, to cover the window where a shared type's own
    /// layout is queried while it is being registered. See the comment at that
    /// read site.
    pub(crate) shared_type_names: std::collections::HashSet<String>,
    /// Every generic PARAMETER name the program declares, anywhere. Read only
    /// by the `KARAC_STRICT_TYPE_LOWERING` lever, to tell "a type with no LLVM
    /// layout" (what the lever hunts) from "a type parameter with no active
    /// substitution" (which legitimately reaches the `i64` default all the
    /// time — `T` alone fires on `hello world` without this).
    pub(crate) declared_generic_param_names: std::collections::HashSet<String>,
    /// Field names in declaration order (struct name → field names).
    pub(crate) struct_field_names: HashMap<String, Vec<String>>,
    /// Field type-names in declaration order (struct name → per-field
    /// user-type name, or `None` if the field's declared type isn't a
    /// path / isn't a known user struct). Used to recover the inner
    /// type of chained field accesses (`o.inner.name` requires knowing
    /// the type of `o.inner` to resolve `name`'s field index in
    /// `compile_field_access` / `field_index_for`).
    pub(crate) struct_field_type_names: HashMap<String, Vec<Option<String>>>,
    /// Full per-field `TypeExpr` in declaration order (struct name →
    /// field TypeExprs). Carries the generic args that
    /// `struct_field_type_names` discards (`Vec[Node]` vs just `"Vec"`),
    /// which the field-receiver method dispatch path needs to populate
    /// the synth's element-type side-tables via
    /// `register_var_from_type_expr`. Populated alongside
    /// `struct_field_type_names` in `declare_structs`.
    pub(crate) struct_field_type_exprs: HashMap<String, Vec<crate::ast::TypeExpr>>,
    /// User struct/enum type names that opt into ordering — `#[derive(Ord)]` /
    /// `#[derive(PartialOrd)]` or a user `impl Ord`/`impl PartialOrd`. The
    /// `karac_cmp_<T>` family (and thus `Vec[T].sort()` + the `<`/`>` operator
    /// lowering) is emitted ONLY for these; a struct/enum that declares no order
    /// still errors loudly at the sort site (matching the typechecker's Ord
    /// gate). Populated in `declare_structs` / enum registration from the def's
    /// attributes + impl scan.
    pub(crate) ord_orderable_types: std::collections::HashSet<String>,
    /// The subset of [`Self::ord_orderable_types`] that is orderable because it
    /// DERIVES `Ord`/`PartialOrd`, excluding types that only hand-write an
    /// `impl Ord` / `impl PartialOrd`.
    ///
    /// The distinction matters because the comparator both are routed to
    /// (`karac_cmp_<T>`) implements DECLARATION-ORDER lexicographic ordering —
    /// which is what a derive means, and is NOT necessarily what a hand-written
    /// `cmp` body says. Sites that merely need "this type has an order"
    /// (`Vec.sort()`) keep using the wider set; the `<`/`>` operator dispatch
    /// uses this one, so a hand-written `impl Ord` whose body disagrees with
    /// declaration order fails loudly instead of being silently overruled
    /// (B-2026-08-25-35).
    pub(crate) ord_derived_types: std::collections::HashSet<String>,
    /// Declared generic-param names of each OWNED (non-shared) struct, recorded
    /// by `register_struct_metadata`. Empty vec for a non-generic struct. Lets
    /// the generic-struct monomorphization path (`mono_struct_type`) zip a
    /// concrete `Named { name, args }` instantiation against the struct's params
    /// to substitute the field TypeExprs — so `Box[f64]` lays its field out as
    /// `double`, not the default `i64` (B-2026-07-03-23).
    pub(crate) struct_generic_params: HashMap<String, Vec<String>>,
    /// Names of all `shared` / `par` struct AND enum types, recorded by
    /// `register_struct_metadata` — i.e. BEFORE the `shared_types` heap-layout
    /// map is populated (that fills in during `declare_enums` / struct LLVM
    /// build). `enum_drop_kind_for_type_expr` runs inside `declare_enums` and
    /// must know whether a struct field's type is shared (`struct BinOp {
    /// left: Expr }`, `Expr` a shared enum) before `shared_types` has the
    /// `Expr` entry — B-2026-06-14-28. Name-only; the heap layout still comes
    /// from `shared_types` at emit time, once populated.
    pub(crate) shared_type_decl_names: std::collections::HashSet<String>,
    /// FFI union storage types (union name → LLVM struct type used as
    /// the storage blob). Phase 5 slice 4. The storage struct is sized
    /// to `max(field_sizes)` and aligned to `max(field_aligns)` per the
    /// `#[repr(C)] union Foo { ... }` lowering rule: its single LLVM
    /// field is the union-field with the largest alignment (tie-break
    /// preferring the largest size), followed by a `[k x i8]` padding
    /// tail when that field's size is smaller than the full union size.
    /// Populated by `declare_unions` after `declare_structs`. Read by
    /// `llvm_type_for_name` (so `size_of[Foo]` / `align_of[Foo]` work
    /// for free) and by the union-literal / union-field-access codegen
    /// in `compile_struct_init` / `compile_field_access`.
    pub(crate) union_types: HashMap<String, inkwell::types::StructType<'ctx>>,
    /// Per-union field declarations in source order (union name →
    /// (field_name, field_llvm_type)). Used by union-literal codegen
    /// to look up the destination LLVM type when storing through the
    /// alloca, and by union-field-access codegen to bitcast the read
    /// pointer to the field's LLVM type before loading. Populated
    /// alongside `union_types`.
    pub(crate) union_field_types: HashMap<String, Vec<(String, BasicTypeEnum<'ctx>)>>,
    /// Enum layouts for tagged-union codegen (enum name → layout).
    pub(crate) enum_layouts: HashMap<String, EnumLayout<'ctx>>,
    /// All-unit (no payload), non-shared user enums → variant names in tag
    /// order. Drives codegen `Display` for enums (subtask 5): such an enum
    /// renders as the bare variant name, selected on the tag. Payload-bearing
    /// enums are absent (their Display codegen is a tracked follow-on).
    pub(crate) enum_unit_variants: HashMap<String, Vec<String>>,
    /// C-like enum name → (repr type name, `(variant, declared value)` in
    /// declaration order). Copied from `Program.enum_discriminants`, which the
    /// lowering pass fills from the typechecker's folded table — codegen never
    /// folds a declared discriminant itself, so it cannot disagree with the
    /// interpreter about what `.discriminant()` answers (B-2026-08-21-10).
    pub(crate) enum_discriminants: crate::ast::EnumDiscriminantTable,
    /// Names of enums seeded by `seed_builtin_enum_layouts` (`Option`,
    /// `Result`, `Json`, `TcpError`, …) — used by the variant-name →
    /// enum-name disambiguation in `try_compile_enum_variant` /
    /// `infer_enum_from_value` to prefer user-declared enums when a
    /// variant name appears in both. Without this set, HashMap iteration
    /// order non-deterministically picks a seeded layout for a
    /// user-defined variant with the same name (e.g. `MyIoErr.Other`
    /// vs `TcpError.Other`), producing a wrong-shape value at the
    /// constructor site and emitting `unreachable` for downstream
    /// dispatch — surfaced 2026-05-25 by the codegen suite's
    /// intermittent hang investigation.
    pub(crate) seeded_enum_names: HashSet<String>,
    // ── Shared types (RC) ─────────────────────────────────────────
    /// Shared type metadata (struct/enum name → heap layout info).
    pub(crate) shared_types: HashMap<String, SharedTypeInfo<'ctx>>,
    /// Fully-instantiated surface `TypeExpr` per *generic* `Named`
    /// instantiation expression (`Option[String]`, `Result[i64, AllocError]`,
    /// generic user enums) — populated from `Program.enum_inst_type_exprs`
    /// (set by the lowering pass from `TypeCheckResult.expr_types`). Keyed by
    /// the expression's `(span.offset, span.length)`. `compile_enum_eq` uses
    /// it to recover the concrete type argument a generic enum's variant
    /// payload was instantiated with (the `[String]` that `var_type_names`'
    /// bare `"Option"` loses), so a `Some(String)` payload compares by content
    /// rather than by pointer word. A missing entry degrades to the word-wise
    /// path (sound for scalar/unit enums), never a miscompile.
    pub(crate) enum_inst_type_exprs: HashMap<(usize, usize), TypeExpr>,
    /// Instantiated generic-enum type per *local variable / parameter* name
    /// (`opt` → `Option[String]`). Populated during codegen traversal at let
    /// and parameter binding sites (cleared per function, like
    /// `var_type_names`), so heap-payload enum `==` (`compile_enum_eq`) can
    /// resolve a variable operand's type argument by **name** — collision-free,
    /// unlike `enum_inst_type_exprs`, whose span keys collide across f-string
    /// interpolations (every interp expr is re-parsed under a fixed-length
    /// `fn __interp__() { … }` wrapper). The span-keyed table remains the source
    /// at the reliable, absolute-spanned binding sites; this name map is the
    /// reliable lookup at use sites.
    pub(crate) enum_inst_var_types: HashMap<String, TypeExpr>,
    /// Struct FIELD indices moved out of a let-bound struct (`let x = h.o`),
    /// per variable — the field-index sibling of `tuple_moved_elem_bodies`. The
    /// field's body belongs to the destination, so the struct's
    /// `__karac_dropbodies_*` walk is re-registered with it masked. Cleared per
    /// function alongside that map (B-2026-08-03-8).
    pub(crate) struct_moved_field_bodies: HashMap<String, std::collections::HashSet<usize>>,
    /// B-2026-08-29-33 — struct FIELD indices whose ENUM PAYLOAD bodies were
    /// taken by a consuming `match` / `if let` arm over `<var>.<field>`, per
    /// variable. Held apart from `struct_moved_field_bodies` because the mask is
    /// finer: the field's OWN `impl Drop` body still runs (the enum object did
    /// not move, only its payload), so masking it through that map would lose
    /// `dE`. Accumulates across arms and is cleared per function alongside its
    /// sibling.
    /// B-2026-08-29-36 — a PATH of field indices, not a single index. A
    /// one-element path is the original one-hop case (`match s.e { .. }`);
    /// a longer one records a deeper projection scrutinee (`match w.s.e`),
    /// whose mask has to be threaded down through `FieldSkipTree::nested`
    /// against each intermediate field's own walker.
    pub(crate) struct_moved_field_payload_bodies:
        HashMap<String, std::collections::HashSet<Vec<usize>>>,
    /// B-2026-08-31-30 — the NESTED sibling of `struct_moved_field_bodies`:
    /// outer field index -> the INNER field indices a nested struct sub-pattern
    /// moved out (`match q { Q { h: H { r, .. } } => … }`). The outer field
    /// itself did not move, so masking it through the flat map would lose every
    /// other body `H` owes; this feeds `FieldSkipTree::nested`, which masks
    /// inside the surviving field's own walker. Accumulates across arms and is
    /// cleared per function alongside its siblings.
    /// B-2026-09-03-11 — keyed by a PATH of outer field indices, not a single
    /// one. A one-element path is the original shape (`match q { Q { h: H { r,
    /// .. } } => … }` and a one-hop tuple-field destructure); a longer one
    /// records a deeper projection (`let (r, k) = g.h.pe;`), whose mask has to
    /// land on the walker of the struct that actually owns the field — the same
    /// re-keying `struct_moved_field_payload_bodies` got in B-2026-08-29-36,
    /// and `FieldSkipTree::nested` already threads it.
    pub(crate) struct_moved_nested_field_bodies:
        HashMap<String, std::collections::BTreeMap<Vec<usize>, std::collections::BTreeSet<usize>>>,
    /// B-2026-08-02-7 / B-2026-08-02-13 — prelude type names a user program
    /// re-declared, shadowing the stdlib type of the same name.
    ///
    /// Stdlib and user types share ONE FLAT NAMESPACE (`prelude::PRELUDE_TYPES`
    /// is injected unqualified) and every backend dispatches on the bare
    /// string, so a user `struct Response` / `HttpError` / `Match` silently
    /// takes over that type's codegen identity. The consequences measured so
    /// far span two subsystems and two failure modes: a user `Response` or
    /// `HttpError` makes the HTTP client double-free a `String` field, and a
    /// user `Match` crashes codegen outright on the regex path.
    ///
    /// Only shadowing by a struct that OWNS HEAP is dangerous today — the
    /// damage is done by the user type's drop glue running over a builtin
    /// value — but the set records every shadow, because the layout confusion
    /// is not limited to drops and a scalar-only shadow becoming heap-owning
    /// is a one-word edit.
    ///
    /// Declaring these names stays legal: `Response` is the documented
    /// `Server.serve` handler-return type, so every serving program has one.
    /// The set is consulted only where a BUILTIN PATH would consume the
    /// shadowed type, so a program that never touches that path is unaffected
    /// (`examples/shortener` declares `Response` and never calls `Client`).
    ///
    /// The real fix is module-qualified stdlib types (`http.Response` as a
    /// distinct type); until then this converts silent corruption into an
    /// actionable message. Tracked as B-2026-08-02-13.
    pub(crate) user_shadowed_prelude_types: std::collections::HashSet<String>,
}
