//! Shared registry for the fallible-allocation `try_*` companion methods
//! (phase-8-stdlib-floor item 2 — design.md § Fallible Allocation API and OOM
//! Handling). A `try_<base>` companion types identically to its panicking
//! `<base>` counterpart but returns `Result[<base-ret>, AllocError]`; the only
//! difference is the return type. This module is pure data + tiny predicates so
//! the three consuming phases agree on which `try_<base>` names are companions:
//!
//! * the **typechecker** (`infer_method_call` / `infer_call`) recurses into the
//!   base method to reuse its argument validation + return-type synthesis, then
//!   wraps the result in `Result[_, AllocError]`;
//! * the **interpreter** (`eval_method_call` / `eval_call`) runs the base
//!   operation and wraps its value in `Result.Ok(_)` — the tree-walk host
//!   allocator never actually OOMs, so the companion is always `Ok` (failure
//!   injection arrives with the codegen runtime allocator wrappers, item 8);
//! * the **effect checker** seeds every companion with `allocates(Heap)`, the
//!   same effect its panicking counterpart carries.
//!
//! Only `try_<base>` forms whose panicking `<base>` already exists on a builtin
//! collection are registered. Companions whose base operation does not exist yet
//! (`Map.reserve`, `Vec.from_iter` — see B-2026-08-26-22) are deferred until
//! that base lands — see the tracker entry. That rule is the
//! whole explanation for the gap B-2026-08-25-20 reported as three missing
//! `try_*` methods: the missing half is the PANICKING base, not the companion
//! layer, which is why `try_extend` was a one-line registration here (its base
//! `Vec.extend` already existed) while its two row-mates were not.
//!
//! `Vec.from_iter` shipped WITHOUT its companion for one release
//! (B-2026-08-26-22) and gained it in B-2026-08-26-27. The gap was not an
//! oversight: `from_iter` lowers to `iter.collect()`, and `collect`'s codegen
//! grew its accumulator through the PANICKING allocator, so a `try_from_iter`
//! built on that would have returned `Ok` unconditionally and aborted inside
//! `collect` on real OOM — a companion that lies about being fallible, which is
//! worse than one that does not exist. The fix was to give the collect engine
//! itself a fallible switch rather than to wrap an infallible one.
//!
//! `Vec.reserve` / `Vec.reserve_exact` LEFT that deferred list in the same
//! change that landed their bases (B-2026-08-25-20), which is what the rule is
//! for: the base arrives, the companion follows, and nothing here has to know
//! how the base works. `Box.new` / `Rc.new` are not on the list at all and
//! never will be — design.md § Fallible Allocation's table has no
//! `Box`/`Rc`/`Arc` row because Kāra spells heap indirection with `shared` /
//! `par` type declarations rather than per-use-site wrappers, so there is no
//! per-allocation constructor to give a `try_` twin (B-2026-08-25-19).

/// Instance methods whose `try_<base>` companion returns
/// `Result[<base-ret>, AllocError]`. Each base is a panicking, heap-allocating
/// method on a builtin collection (`Vec` / `VecDeque` / `String` / `Map` /
/// `Set` / `SortedSet`). The interception is gated on a builtin-collection
/// receiver at every call site so a user type that happens to define a
/// like-named method is never shadowed.
pub const TRY_ALLOC_INSTANCE_BASES: &[&str] = &[
    "push",              // Vec.push, String.push
    "push_str",          // String.push_str
    "push_back",         // VecDeque.push_back
    "push_front",        // VecDeque.push_front
    "extend_from_slice", // Vec.extend_from_slice
    // `extend` is the spec's spelling (design.md § Fallible Allocation's table
    // row `extend(iter)` / `try_extend(iter)`); the typechecker, interpreter
    // and codegen all already treat it as an exact alias of
    // `extend_from_slice`, so registering it here is the whole of
    // `Vec.try_extend`. B-2026-08-25-20.
    "extend",
    // B-2026-08-25-20 — the reserve family. These are the row's headline
    // `try_reserve` / `try_reserve_exact`, and they could only be registered
    // once their PANICKING bases landed, which is the deferral rule above
    // working exactly as written rather than an exception to it.
    "reserve",
    "reserve_exact",
    // B-2026-08-26-22 — registered only now that each has a FALLIBLE codegen
    // arm. B-2026-08-25-20 landed their panicking bases but deliberately left
    // them off this list, because a name here without a matching
    // `CODEGEN_FALLIBLE_INSTANCE_BASES` entry typechecks and then fails
    // `karac build` — the run-vs-build shape.
    "resize",
    "append",
    "insert", // Map.insert, Set.insert, SortedSet.insert, SortedMap.insert
    "clone",  // Vec/String/Map/SortedMap/Set/SortedSet/VecDeque.clone
];

/// Static constructors whose `Type.try_<base>(...)` companion returns
/// `Result[<constructor-ret>, AllocError]`. Each base is a path-form
/// constructor recognized by the typechecker (`Vec.with_capacity`,
/// `VecDeque.with_capacity`, `String.with_capacity`, `Vec.from_slice`).
pub const TRY_ALLOC_STATIC_BASES: &[&str] = &["with_capacity", "from_slice", "from_iter"];

/// Effect-checker seed key for any instance `try_*` companion. Seeded once with
/// `allocates(Heap)`; the method-call effect walker routes every recognized
/// `try_<base>` instance call to it (the static constructor forms are seeded by
/// their fully-qualified `Type.try_<base>` key instead, alongside the panicking
/// constructors).
pub const TRY_ALLOC_EFFECT_KEY: &str = "__builtin_try_alloc";

/// `true` when `method` is a recognized instance `try_*` companion — i.e. it is
/// `try_<base>` for some `base` in [`TRY_ALLOC_INSTANCE_BASES`]. Returns the
/// stripped base name so callers can recurse into it.
pub fn instance_companion_base(method: &str) -> Option<&'static str> {
    let base = method.strip_prefix("try_")?;
    TRY_ALLOC_INSTANCE_BASES
        .iter()
        .copied()
        .find(|&b| b == base)
}

/// `true` when `method` is a recognized static `try_*` constructor companion —
/// `try_<base>` for some `base` in [`TRY_ALLOC_STATIC_BASES`]. Returns the
/// stripped base name.
pub fn static_companion_base(method: &str) -> Option<&'static str> {
    let base = method.strip_prefix("try_")?;
    TRY_ALLOC_STATIC_BASES.iter().copied().find(|&b| b == base)
}

/// Instance `try_*` companions whose **codegen** (`karac build`) lowering has
/// landed (phase-8-stdlib-floor item 8). The `try_<base>` form for a base in
/// this set flows through to its dispatcher (`compile_vec_method`) and emits
/// real fallible allocation + `Result`; any other recognized companion is still
/// interpreter-only and `compile_method_call` rejects it loudly. Grows as more
/// `try_*` codegen arms land (`from_slice`, the `with_capacity` constructors,
/// `clone`, the `Map`/`Set` `insert` forms — the last need fallible runtime FFI).
pub const CODEGEN_FALLIBLE_INSTANCE_BASES: &[&str] = &[
    "push",              // Vec.try_push
    "push_back",         // VecDeque.try_push_back (shares Vec storage / the push arm)
    "push_str",          // String.try_push_str
    "push_front",        // VecDeque.try_push_front
    "extend_from_slice", // Vec.try_extend_from_slice
    "extend",            // Vec.try_extend (alias of the above, all phases)
    "reserve",           // Vec.try_reserve       (B-2026-08-25-20)
    "reserve_exact",     // Vec.try_reserve_exact (B-2026-08-25-20)
    "resize",            // Vec.try_resize        (B-2026-08-26-22)
    "append",            // Vec.try_append        (B-2026-08-26-22)
    "clone",             // Vec/VecDeque/String.try_clone (Map/Set rejected at dispatch)
    "insert", // Map/Set/SortedSet.try_insert (routes to compile_map_method / compile_set_method)
];

/// `true` when `method`'s instance `try_*` companion has codegen lowering today
/// (its base is in [`CODEGEN_FALLIBLE_INSTANCE_BASES`]).
pub fn instance_companion_has_codegen(method: &str) -> bool {
    instance_companion_base(method).is_some_and(|b| CODEGEN_FALLIBLE_INSTANCE_BASES.contains(&b))
}

/// Static constructor `try_*` companions whose **codegen** (`karac build`)
/// lowering has landed (phase-8-stdlib-floor item 8), keyed by
/// `(type_name, base)`. The `Type.try_<base>` form for a pair in this set
/// flows through to `compile_assoc_call`'s real fallible lowering; any other
/// recognized static companion is still interpreter-only and
/// `compile_assoc_call` rejects it loudly. Grows as more constructor `try_*`
/// codegen arms land.
pub const CODEGEN_FALLIBLE_STATIC: &[(&str, &str)] = &[
    ("Vec", "from_slice"),         // Vec.try_from_slice
    ("Vec", "with_capacity"),      // Vec.try_with_capacity
    ("VecDeque", "with_capacity"), // VecDeque.try_with_capacity (shared Vec storage)
    ("String", "with_capacity"),   // String.try_with_capacity (byte element)
    ("Vec", "from_iter"),          // Vec.try_from_iter (B-2026-08-26-27)
];

/// `true` when `type_name.<method>` is a static `try_*` constructor companion
/// whose codegen lowering has landed (its `(type_name, base)` pair is in
/// [`CODEGEN_FALLIBLE_STATIC`]).
pub fn static_companion_has_codegen(type_name: &str, method: &str) -> bool {
    static_companion_base(method).is_some_and(|b| CODEGEN_FALLIBLE_STATIC.contains(&(type_name, b)))
}
