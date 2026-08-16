//! Borrow- and ownership-mode classification of the current function's
//! params and locals.
//!
//! Which names are `ref`/`mut ref` params (`ref_params` per body-walk,
//! `signature_ref_params` per declared mode), which own their Vec/String
//! buffer (`owned_vecstr_params`, `owned_struct_params`), which are
//! borrow-views a for-loop or accessor produced (`for_loop_borrow_vars`,
//! `borrow_accessor_let_payload`, `for_loop_owned_agg_vars`,
//! `borrowed_agg_payload_struct_vars`, `entry_slot_ref_vars`), and which
//! hold `Option[shared]` heap payloads by value vs by ref
//! (`var_option_shared_heap`, `ref_option_shared_heap`). Consulted by the
//! cleanup registrars and the RC inc/dec emission to decide who frees
//! what. Extracted from `Codegen` as a cluster-15 sub-slice of the
//! state-decomposition spike; see
//! `docs/spikes/state-decomposition-codegen-methodcall.md`.

use std::collections::{HashMap, HashSet};

use inkwell::types::{BasicTypeEnum, StructType};

pub(crate) struct BorrowVars<'ctx> {
    /// Variables that are ref parameters (name → inner LLVM type for dereferencing).
    pub(crate) ref_params: HashMap<String, BasicTypeEnum<'ctx>>,
    /// The SUBSET of `ref_params` that are genuine function-SIGNATURE
    /// `ref`/`mut ref` parameters (B-2026-07-21-11 regression fix):
    /// `ref_params` also carries pattern-binding pointer SHIMS (borrow-mode
    /// payload bindings, `bind_pattern_values_via_ptr` GEP shims, `let r: ref
    /// T` locals) whose ownership is governed by their own machinery
    /// (`borrowed_agg_payload_struct_vars` deep-copies, borrow
    /// classification). The ref-chain clone legs must fire ONLY for chains
    /// rooted at a signature param — cloning a shim-rooted chain duplicates
    /// the existing copy machinery into a leak (caught by the
    /// B-2026-07-17-20 LSan test). Mirrors `ref_params`' lifecycle: cleared
    /// at function entry, swapped around mono bodies, shadow-danced per name.
    pub(crate) signature_ref_params: std::collections::HashSet<String>,
    /// Locals bound to a `mut ref V` slot pointer returned by
    /// `m.entry(k).or_insert(d)` / `or_insert_with(f)` — the two-step
    /// `let r = m.entry(k).or_insert(0); *r += 1`. The binding's alloca holds
    /// the raw slot pointer (`*mut V`); the value (name → V's LLVM type) drives
    /// the deref-read (`*r`) and deref-write (`*r += 1` / `*r = v`). This is
    /// the codegen analog of the interpreter's `Value::MapSlotRef`. Kept
    /// separate from `ref_params` (immutable borrows, no write-through, and the
    /// borrow path deliberately excludes `or_insert`).
    pub(crate) entry_slot_ref_vars: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Owned (bare `String` / `Vec[T]`, non-ref) parameters of the
    /// function currently being compiled. The call ABI passes these
    /// `{data, len, cap}` headers by value while the CALLER retains the
    /// buffer's scope-exit free (no ownership transfer at the call
    /// boundary today), so any consume site inside the callee that
    /// RETAINS the value beyond the call — `Vec.push(param)`,
    /// `return param` — must deep-copy the buffer instead of aliasing
    /// it. Without the copy, the caller's free leaves the retained
    /// alias dangling (kata-22 backtracking: `out.push(cur)` at the
    /// recursion base case; `fn id(s: String) -> String { s }`).
    /// Cleared per-function alongside `ref_params`.
    pub(crate) owned_vecstr_params: HashSet<String>,
    /// `for w in vec` loop-element bindings whose element is a heap
    /// `{ptr,len,cap}` type (`String` / `Vec`). `for` over a Vec is
    /// BORROW-iteration — the loop binds `w` to an ALIAS of `data[i]` and the
    /// source Vec retains ownership (it is usable after the loop) — so a consume
    /// site that RETAINS `w` (`m.entry(w)`, `v.push(w)`, `m.insert(w, ..)`) must
    /// deep-copy it, exactly like an owned param: otherwise both the sink's
    /// drop and the source Vec's drop free the same buffer (double-free; the
    /// interpreter clones, so this was an A/B mismatch — B-2026-06-20-13).
    /// `maybe_defensive_copy_param_arg` treats membership here the same as
    /// `owned_vecstr_params`. Added by `register_for_loop_bindings` for heap
    /// element types; removed when a later `let` rebinds the name (shadow); all
    /// cleared per-function alongside `owned_vecstr_params`.
    pub(crate) for_loop_borrow_vars: HashSet<String>,
    /// `let g = coll.get(k)` (also `.first()` / `.last()`) bindings whose RHS is
    /// a borrow-returning collection accessor — `Map`/`SortedMap` `.get` returns
    /// an `Option[V]` whose payload ALIASES the bucket's stored value, and
    /// `Vec`/`Slice`/`Array` `.get`/`.first`/`.last` return `Option[ref T]`
    /// aliasing element storage (the class `scrutinee_is_borrow_call`
    /// recognizes for a DIRECT `match coll.get(k)` scrutinee). The let-site
    /// already suppresses `g`'s own scope-exit drop, but the intermediate
    /// binding hid the alias property from a later `match g { Some(v) => <move
    /// v> }` / `if let Some(v) = g`: the arm treated `g` as an owned `Option`,
    /// so an escaping/dropped payload freed the aliased buffer a second time —
    /// double-freeing against the collection's own element drop. This records
    /// the binding name → its `Option[..]` type so `scrutinee_is_borrowed_binding`
    /// re-admits it into the borrow protection (the `Map` payload clones on
    /// escape via `borrow_get_payload_clone_te`; the `ref`-typed `Vec` payload
    /// self-gates to alias-only, matching the direct form exactly). Cleared
    /// per-function alongside `for_loop_borrow_vars`. B-2026-07-09-13.
    pub(crate) borrow_accessor_let_payload: std::collections::HashMap<String, crate::ast::TypeExpr>,
    /// Heap-owning **struct/enum** `for`-loop element bindings (B-2026-07-04-17).
    /// Like `for_loop_borrow_vars` the binding is a bit-copy alias of the
    /// container slot whose heap the container's per-element drop frees — but
    /// structs/enums use the callee-*entry*-copy ownership model, NOT the
    /// caller-side `maybe_defensive_copy_param_arg` copy Vec/String use (routing
    /// them through that would double-copy at call-arg sites → leak). So this
    /// set is consulted ONLY at LOCAL new-owner consume sites that have no
    /// callee to entry-copy — a whole-struct move `let x = a` and a field/whole
    /// move into a fresh struct literal (`A { s: a.s }`) — where the new owner's
    /// slot is deep-copied in place so it and the container own independent
    /// heap. Populated by `register_for_loop_bindings` (gated on recursive
    /// copy-support); removed on shadow-rebind; cleared per-function alongside
    /// `for_loop_borrow_vars`.
    pub(crate) for_loop_owned_agg_vars: HashSet<String>,
    /// Struct payload bindings from a match arm on a BORROWED / owned-elsewhere
    /// scrutinee (`pattern_binding_is_borrow` — e.g. `for it in items { match it
    /// { Fu(f) => … } }` over `items: ref Vec[It]`, classed read-only by
    /// `scrutinee_is_readonly_owned_agg_loop_var`). The binding `f` aliases the
    /// container's live-variant payload, whose heap fields the container owns and
    /// frees — so a Vec/String field COPIED OUT of `f` (`let ps = f.params`)
    /// must own an independent buffer, exactly like a for-loop struct element
    /// (`for_loop_owned_agg_vars`). Consulted by
    /// `deep_copy_owned_struct_param_field_move`. Without it, `ps` shallow-aliased
    /// the container's buffer and both freed it → double-free (B-2026-07-17-20;
    /// the struct-only twin `for f in items { let ps = f.params }` was already
    /// clean via `for_loop_owned_agg_vars`, but the enum-payload match binding
    /// reached neither set). Cleared per-function alongside the sibling sets.
    pub(crate) borrowed_agg_payload_struct_vars: HashSet<String>,
    /// Owned (bare, non-ref) **struct** params with at least one heap
    /// (`Vec`/`String`) field. Same copy-model rationale as
    /// `owned_vecstr_params`, one level in: a by-value struct param is a
    /// shallow copy whose heap-field buffers alias the caller's, but the
    /// caller retains and frees them. So moving a heap field OUT
    /// (`let inner = h.v`) into an owned local that the callee then frees
    /// double-frees against the caller's struct-drop. The let-FieldAccess
    /// lowering deep-copies such a field's buffer so the moved-out local is
    /// independent (B-2026-06-10-2). Cleared per-function alongside
    /// `ref_params`.
    pub(crate) owned_struct_params: HashSet<String>,
    /// Per-binding inner-shared-heap layout for `Option[shared T]`
    /// variables. Populated by `track_rc_option_var` at let-binding
    /// time; read by the `Assign` arm so reassignment of a tracked
    /// Option[shared T] binding adjusts refcounts symmetrically to
    /// the plain shared-T arm (dec old inner pointer, inc new inner
    /// pointer unless RHS is a fresh `Some(...)` literal). Without
    /// this, `next_a = n.next;` (LeetCode #2 recursive variant)
    /// stranded the old inner ref and over-decremented at scope
    /// exit, freeing a still-aliased chain.
    pub(crate) var_option_shared_heap: HashMap<String, StructType<'ctx>>,
    /// `mut ref Option[shared T]` parameters, keyed by name → inner shared heap
    /// layout. The sibling of `var_option_shared_heap` for the by-ref case: the
    /// param's local alloca holds the BORROW pointer (the caller's Option slot
    /// address), not the Option struct, so a reassignment (`prev = Some(n)`)
    /// must run the ARC retain/release store THROUGH `get_data_ptr(name)` rather
    /// than into the local slot — otherwise the write lands in a callee-local
    /// copy and never propagates back to the caller (B-2026-07-12-3).
    pub(crate) ref_option_shared_heap: HashMap<String, StructType<'ctx>>,
}
