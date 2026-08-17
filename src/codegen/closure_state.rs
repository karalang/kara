//! Closure-lowering state: fat-pointer types, heap-env ownership tracking,
//! and the compile-time staging cells.
//!
//! Everything `Codegen` keeps about closures as VALUES: the per-binding fat
//! pointer function types, the heap-env closure ownership tables (which
//! locals own a boxed env, which functions RETURN one — directly or inside
//! a tuple / array / Vec / aggregate — and the per-function owner maps
//! `reject_heap_env_misuse` walks), the currying set, per-closure capture
//! path modes, the unique-name counter, and the two `pending_closure_*`
//! staging cells `compile_closure` threads to the surrounding `let`.
//! Extracted from `Codegen` as a cluster-15 sub-slice of the
//! state-decomposition spike; see
//! `docs/spikes/state-decomposition-codegen-methodcall.md`.

use rustc_hash::FxHashMap;
use std::collections::HashMap;

use inkwell::types::{BasicTypeEnum, FunctionType};

use crate::ownership::{CapturePath, OwnershipMode};
use crate::resolver::SpanKey;

pub(crate) struct ClosureState<'ctx> {
    // ── Closure compilation ────────────────────────────────────────
    /// Monotonic counter used to generate unique closure function names.
    pub(crate) closure_counter: u32,
    /// Maps local variable names that hold closure fat-pointers to their LLVM function type.
    /// Required for indirect calls: `build_indirect_call` needs the callee's function type.
    pub(crate) closure_fn_types: HashMap<String, FunctionType<'ctx>>,
    /// Names of functions whose return value is a heap-env closure (their direct
    /// tail / `return` is an escaping capturing closure literal). A
    /// `let f = <call to such a fn>` binding therefore owns a heap env and gets
    /// a `FreeClosureEnv` cleanup. Computed once before function bodies compile.
    pub(crate) fns_returning_heap_env: std::collections::HashSet<String>,
    /// Local bindings in the current function that own a heap-env closure (a
    /// `FreeClosureEnv` was registered). Used to reject not-yet-supported
    /// escapes of such a binding (return / copy / store / pass — Slice 1 is
    /// call-only) and reset per function.
    pub(crate) heap_env_closure_vars: std::collections::HashSet<String>,
    /// Currying (B-2026-07-12-12): local closure-VALUE bindings in the current
    /// function whose CALL returns a heap-env closure — `let make = |n| |x| x +
    /// n;` binds `make`, and `make(5)` yields the inner closure's reference-
    /// counted heap env. Populated per function (before the misuse guard runs)
    /// by `compute_curry_closure_vars`, then consulted by
    /// `is_heap_env_producing_call` so a `make(..)` call routes through the SAME
    /// free / owner / misuse machinery as a call to a named
    /// `fns_returning_heap_env` function. Reset per function.
    pub(crate) curry_closure_vars: std::collections::HashSet<String>,
    /// Per-function map (set at the top of `reject_heap_env_misuse`, read by its
    /// exhaustive walk): a struct local `h` constructed as `let h = H { f:
    /// make(..) }` → the set of its fields that hold a heap-env closure. Such an
    /// `h` OWNS those env boxes (codegen registers an instance-specific
    /// `FreeClosureEnv` on each field GEP), so the guard sanctions a field-call
    /// `(h.f)(x)` and a non-closure field read while rejecting any escape of `h`
    /// or a non-call projection of a closure field (store-in-struct slice).
    pub(crate) heap_env_aggregate_owners:
        std::collections::HashMap<String, std::collections::HashSet<String>>,
    /// Names of functions whose return value is a heap-env-OWNING aggregate — a
    /// struct local that owns one or more heap-env closure fields, returned as a
    /// bare tail / `return h` (aggregate-escape slice). Maps fn name → the set of
    /// the returned struct's field names that own a heap env. A
    /// `let r = <call to such a fn>` binding therefore owns those env boxes (the
    /// caller registers an instance `FreeClosureEnv` on each named field; the
    /// callee moved them out at the same refcount). A FIXPOINT (relay-of-aggregate)
    /// computed once before bodies compile, after `fns_returning_heap_env`.
    pub(crate) fns_returning_heap_env_aggregate:
        std::collections::HashMap<String, std::collections::HashSet<String>>,
    /// Names of functions whose return value is a heap-env-OWNING TUPLE — a tuple
    /// local owning one or more heap-env closure ELEMENTS, returned as a bare tail /
    /// `return t` (container-escape slice). Maps fn name → the set of returned tuple
    /// element INDICES that own a heap env. A `let r = <call to such a fn>` binding
    /// then owns those env boxes (the caller registers a per-element `FreeClosureEnv`;
    /// the callee moved them out at the same refcount). The tuple twin of
    /// `fns_returning_heap_env_aggregate`; a FIXPOINT computed before bodies compile.
    pub(crate) fns_returning_heap_env_tuple:
        std::collections::HashMap<String, std::collections::HashSet<usize>>,
    /// Names of functions whose return value is a heap-env-OWNING fixed-size ARRAY —
    /// the array twin of `fns_returning_heap_env_tuple`. Maps fn name → the returned
    /// array's heap-env element INDICES. Same caller-adopts / callee-moves-out
    /// contract; the only codegen difference is the element GEP form (array
    /// `build_gep [0, idx]` vs tuple `build_struct_gep`).
    pub(crate) fns_returning_heap_env_array:
        std::collections::HashMap<String, std::collections::HashSet<usize>>,
    /// Names of functions whose return value is a heap-env-OWNING `Vec[Fn]` — a Vec
    /// owner returned as a bare tail / `return v` (Vec-escape slice). A set (not a
    /// map): a Vec is homogeneous `Vec[Fn]` of unknown length, so there are no
    /// per-element indices to carry — every live element is a closure. A
    /// `let r = <call to such a fn>` binding becomes a Vec owner: the callee moved
    /// the buffer out by value (its tail-return cap-zero suppressed its own dynamic
    /// closure drop loop), and the caller's binding adopts that drop loop. A FIXPOINT
    /// (relay-of-Vec), computed after the tuple/array fixpoints.
    pub(crate) fns_returning_heap_env_vec: std::collections::HashSet<String>,
    /// Per-function map (reset each function): an aggregate-owner local `h` → the
    /// `(struct type name, field index)` of each heap-env field it owns. Recorded
    /// when the field's `FreeClosureEnv` is registered (struct-literal store OR an
    /// aggregate-returning call result). Used by `neutralize_moved_aggregate_env_slots`
    /// to runtime-null those field env slots when `h` is moved out via a return, so
    /// the callee's field drop no-ops and the box flows to the caller at the same
    /// refcount (the aggregate analog of `neutralize_moved_closure_env_slot`).
    pub(crate) heap_env_owner_fields: std::collections::HashMap<String, Vec<(String, u32)>>,
    /// Per-function map (set in `reject_heap_env_misuse`, read by its walk): a TUPLE
    /// local `t` constructed as `let t = (make(..), ..)` / `(f, ..)` → the set of its
    /// element INDICES that hold a heap-env closure. Such a `t` OWNS those env boxes
    /// (codegen registers an instance `FreeClosureEnv` on each element GEP), so the
    /// guard sanctions a tuple-index call `(t.0)(x)` and a non-closure element read
    /// while rejecting any escape of `t` or a non-call projection of a closure
    /// element (tuple-store slice). The tuple analog of `heap_env_aggregate_owners`.
    pub(crate) heap_env_tuple_owners:
        std::collections::HashMap<String, std::collections::HashSet<usize>>,
    /// Per-function map (set in `reject_heap_env_misuse`, read by its walk): a
    /// fixed-size ARRAY local `a` constructed as `let a: Array[Fn,N] = [make(..), ..]`
    /// / `[f, ..]` → the set of its element INDICES that hold a heap-env closure.
    /// Such an `a` OWNS those env boxes (codegen registers an instance
    /// `FreeClosureEnv` on each element GEP), so the guard sanctions an array-index
    /// call `(a[i])(x)` while rejecting any escape of `a` or a non-call projection
    /// of a closure element (array-store slice). The array analog of
    /// `heap_env_tuple_owners`; an array is homogeneous, so an array-of-closures
    /// has no non-closure sibling elements to read (unlike a tuple). Only an
    /// `ExprKind::ArrayLiteral` RHS qualifies — a bare `[..]` lowers to a Vec
    /// `PrefixCollectionLiteral`, whose element store stays rejected (Vec slice).
    pub(crate) heap_env_array_owners:
        std::collections::HashMap<String, std::collections::HashSet<usize>>,
    /// Per-function set (set in `reject_heap_env_misuse`, read by its walk AND by
    /// the Vec-binding codegen): a `Vec[Fn]` local `v` bound as `let v: Vec[Fn] =
    /// Vec.new()` / `Vec.with_capacity(..)` that receives at least one heap-env
    /// closure PUSH (`v.push(make(k))` fresh, or `v.push(f)` for a heap-env
    /// binding). Such a `v` OWNS the env boxes of every element it holds; codegen
    /// registers a DYNAMIC per-element `FreeClosureEnv` drop loop (`0..len`) at its
    /// scope exit (the dynamic-length analog of the array/tuple per-slot drops).
    /// The guard sanctions `v.push(<heap-env>)`, `(v[i])(x)`, and read-only
    /// `len`/`is_empty`/`capacity`, while rejecting any other use (escape,
    /// projection, a non-heap-env push, or a moving/aliasing method) — a mixed
    /// heap-env + stack-env Vec can't be drop-classified at runtime, so it is
    /// rejected, never silently miscompiled. Unlike the by-value array/tuple owner
    /// maps, this carries no per-index set: a Vec is homogeneous `Vec[Fn]`, every
    /// live element is a closure, and the length is dynamic.
    pub(crate) heap_env_vec_owners: std::collections::HashSet<String>,
    /// Staging slot — set by `compile_closure` so the surrounding `let` binding can record
    /// the function type under the newly bound name.
    pub(crate) pending_closure_fn_type: Option<FunctionType<'ctx>>,
    /// Staging slot — caller-supplied LLVM types for a closure's parameters,
    /// consulted by `compile_closure` when the source has no type annotation
    /// to refine. Used by `Vec.sort_by` to push the element type into
    /// `|a, b|` closures so tuple receivers don't collapse to bare `i64`.
    /// Taken once and cleared on entry to `compile_closure`.
    pub(crate) pending_closure_param_hints: Option<Vec<BasicTypeEnum<'ctx>>>,
    /// Per-closure capture path modes sourced from
    /// `OwnershipCheckResult::closure_capture_path_modes` — line 353
    /// phase-5 checklist disjoint-capture slice 4. When a closure
    /// expression's `SpanKey` lives in this map, `compile_closure` lays
    /// the env struct out with one slot per captured `CapturePath` (each
    /// sized to the leaf type at the projection chain end) and stitches
    /// the leaf values back into a fresh root alloca in the synthesized
    /// closure body. Absent → fall back to the per-name
    /// `collect_closure_free_vars` layout (preserves the `compile_to_ir`
    /// path and any codegen-only tests that don't run the ownership
    /// pass).
    pub(crate) closure_capture_paths: FxHashMap<SpanKey, Vec<(CapturePath, OwnershipMode)>>,
}
