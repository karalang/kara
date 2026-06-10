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
//! (`Vec.reserve` / `Vec.append` / `Vec.resize` / `Box.new` / `Rc.new` / …) are
//! deferred until that base lands — see the tracker entry.

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
    "insert",            // Map.insert, Set.insert, SortedSet.insert
    "clone",             // Vec/String/Map/Set/SortedSet/VecDeque.clone
];

/// Static constructors whose `Type.try_<base>(...)` companion returns
/// `Result[<constructor-ret>, AllocError]`. Each base is a path-form
/// constructor recognized by the typechecker (`Vec.with_capacity`,
/// `VecDeque.with_capacity`, `String.with_capacity`, `Vec.from_slice`).
pub const TRY_ALLOC_STATIC_BASES: &[&str] = &["with_capacity", "from_slice"];

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
