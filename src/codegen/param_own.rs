//! #14 — Callee-ownership for by-value aggregate (`struct` / `enum`)
//! parameters.
//!
//! ## The bug
//!
//! Codegen passes a by-value aggregate argument as a SHALLOW copy (the
//! struct/enum words, including any heap `ptr`) under a *caller-retains*
//! model: the caller's source binding frees the buffers at its scope exit,
//! and the callee's param frees nothing. That model is sound only when the
//! callee *consumes-and-frees* (destructures) or *ignores* the value. It
//! double-frees when the callee **transfers the value OUT** — moves the param
//! into its return value (directly, or wrapped into a returned struct/enum
//! literal). Then the caller's source binding AND the returned value alias the
//! same buffer, and BOTH free it (`exit 133`).
//!
//! ## Why not move-by-default
//!
//! The "proper" fix — have the caller MOVE the arg (suppress its source drop)
//! and the callee OWN the param — is unsound here because Kāra's move-checker
//! does NOT reject double-consume / use-after-move: `take(x); take(x)` and
//! `take(x); println(x.f)` both compile and run correctly TODAY under
//! caller-retains. Caller-side move would turn those into use-after-frees.
//!
//! ## The fix: entry deep-copy + callee-owned drop
//!
//! At function entry, deep-copy the owned aggregate param's heap-field buffers
//! so the callee owns buffers INDEPENDENT of the caller's retained originals,
//! then register the param's scope-exit drop. The param now behaves exactly
//! like a `let`-bound local owned binding, so ALL existing local
//! move-suppression (tail return, struct/enum-literal consume, match
//! destructure, pass-as-arg) applies to it for free. Result: the caller frees
//! its original once; the callee frees its copy once (or suppresses that drop
//! when the copy is transferred out, leaving the destination the sole owner).
//! No caller-side change, hence no move-checker dependency — `take(x); take(x)`
//! keeps working (each call copies at entry).
//!
//! ## Depth discipline
//!
//! The entry copy MIRRORS the registered drop's depth EXACTLY. Both the struct
//! drop (`emit_struct_drop_synthesis`) and the enum drop
//! (`emit_enum_drop_switch`) free OUTER Vec/String buffers only — a nested
//! `Vec[heap_T]`'s elements are a bounded leak on both sides, never corruption
//! — recursing into nested structs/tuples. So the copy is an outer-buffer
//! copy (`emit_vecstr_defensive_copy` with `elem_te = None`, no element
//! recursion) per Vec/String field/payload, recursing into nested
//! structs/tuples.
//!
//! ## Bail conditions (left on caller-retains — never a regression)
//!
//! Any aggregate whose drop frees buffers this routine can't soundly duplicate
//! is left untouched (returns `false`): Map/Set handles, HTTP side-table
//! handles (`Response`/`RequestBuilder`), shared (RC) types, and `Result`
//! fields plus the `Option` payloads this routine can't yet duplicate
//! (boxed-wide, struct/enum-inline, plain-enum = B-27). A non-shared user-ENUM
//! field IS supported (#19, 2026-06-12): the struct drop frees its live-variant
//! `VecOrString` payload (post-#15/#18) and `deep_copy_one_aggregate_field`
//! duplicates exactly that via `deep_copy_enum_heap_payload_in_place`, keeping
//! copy and drop symmetric. An `Option[String]` / `Option[Vec[..]]` field (an
//! inline `{ptr,len,cap}` payload) IS supported too (B-2026-07-03-28 Facet A,
//! 2026-07-03): `deep_copy_option_inline_payload_in_place` duplicates the `Some`
//! buffer type-aware off the field `TypeExpr`, symmetric with the struct drop's
//! `OptionInline` free (which is gated on this very copy-supported predicate).
//! Bailing on the rest preserves today's exact behavior for those shapes.

use inkwell::types::{BasicType, BasicTypeEnum, StructType};
use inkwell::values::PointerValue;
use inkwell::{AddressSpace, IntPredicate};
use std::collections::HashMap;

use crate::ast::{Expr, ExprKind, TypeExpr, TypeKind};

use super::state::{EnumDropKind, EnumLayout};

impl<'ctx> super::Codegen<'ctx> {
    /// Make an owned by-value aggregate parameter callee-owned: emit the entry
    /// deep-copy of its heap fields and register its scope-exit drop. Returns
    /// `true` if ownership was taken; `false` if the param was left on the
    /// caller-retains model (no copy, no drop — status quo). See the module
    /// doc for the full rationale.
    pub(super) fn make_aggregate_param_callee_owned(
        &mut self,
        type_name: &str,
        slot: PointerValue<'ctx>,
    ) -> bool {
        // #17 — the seeded std.tracing builder value types (`LogEvent` / `Span`
        // / `SpanField`) used to be name-excluded here. Their chained builder
        // methods (`info(..).with_field(..).with_field(..).in_span(..)`) move
        // individual `self` fields into returned literals, and engaging
        // entry-copy on top of the caller-retains `owned_struct_params` field-move
        // band-aid double-copied / emptied the chained fields. That redundancy is
        // now resolved generally: (gap 1) `compile_function` retires the
        // `owned_struct_params` band-aid for a callee-owned param, and (gap 2)
        // `compile_struct_init` cap-zeros a slot-sourced Vec/String/enum field
        // moved into a returned literal. With both in place these types are
        // callee-owned like any other aggregate — no name exclusion needed.
        // Non-shared user STRUCT.
        if self.struct_types.contains_key(type_name) && !self.shared_types.contains_key(type_name) {
            if !self.aggregate_param_copy_supported_struct(type_name, &mut Vec::new()) {
                // B-2026-07-18-31/-32 — a GENERIC struct param whose fields are
                // bare type params (`Pair[T] { a: T, b: T }`) fails the base
                // copy-support check: `field_copy_supported` sees the erased `T`
                // and bails at its conservative `_ => false` arm, so the param
                // stays caller-retains. But the callee still MOVES those fields
                // out into a returned literal (`Pair { a: p.b, b: p.a }`) —
                // aliasing the caller's buffers, which both the caller's own
                // drop and the returned value then free (double-free, masked at
                // -O2 but live under `karac run`/-O0). When an active monomorph
                // subst resolves the params to copy-supported heap types AND the
                // slot is the concrete mono layout, entry-copy the mono heap
                // fields so the param is callee-owned exactly like its concrete
                // twin (`struct PairS { a: String, b: String }`), which never
                // had this bug. The mono entry-copy GEPs the CONCRETE layout, so
                // it is offset-correct for any field count — unlike the base
                // bare-`T`-reinterpret drop path (B-2026-07-15-11), which stays
                // single-field-gated.
                return self.try_make_generic_struct_param_callee_owned(type_name, slot);
            }
            // B-2026-07-10-4: rc-inc buried bare-shared during entry-copy so it stays
            // symmetric with the combined drop's per-element rc-dec (a copy-supported
            // struct can carry a shared handle buried in a `Vec[struct]` element /
            // nested struct — `FnDefNode.params[].ty`, `FnDefNode.body`).
            let saved = self.deep_copy_rc_inc_bare_shared;
            self.deep_copy_rc_inc_bare_shared = true;
            self.deep_copy_struct_heap_fields_in_place(slot, type_name);
            self.deep_copy_rc_inc_bare_shared = saved;
            self.track_struct_var(type_name, slot);
            return true;
        }
        // Non-shared user ENUM (NOT the type-erased Option/Result, whose
        // payloads are handled by their own dedicated machinery).
        if let Some(layout) = self.enum_layouts.get(type_name).cloned() {
            if layout.is_shared || type_name == "Option" || type_name == "Result" {
                return false;
            }
            // Only meaningful when some variant carries a heap payload —
            // otherwise the drop is a no-op and there's nothing to copy.
            let any_heap = layout
                .field_drop_kinds
                .values()
                .any(|ks| ks.iter().any(|k| k.is_heap_bearing()));
            if !any_heap {
                return false;
            }
            self.deep_copy_enum_heap_payload_in_place(type_name, slot, &layout);
            self.track_enum_var(type_name, slot);
            return true;
        }
        false
    }

    /// #21 — the tuple-param analog of [`Self::make_aggregate_param_callee_owned`].
    /// A bare (non-ref) by-value TUPLE param with an enum / nested-struct heap
    /// leaf (`fn f(p: (Tok, i64))`) is, without this, a shallow copy SHARING the
    /// caller's heap pointer. When the callee consumes a leaf internally
    /// (`match p.0`) while the caller's owning struct drop (`NestedTuple`) also
    /// frees that buffer, both free it → double-free (#21 P5/P6, which cross the
    /// call boundary so no caller-side suppression resolves them). Deep-copy the
    /// tuple's heap leaves at entry (`deep_copy_one_aggregate_field`, which
    /// already recurses through tuple / enum / nested-struct elements) and
    /// register a `TypeExpr`-driven scope-exit drop (`synthesize_tuple_drop_fn_te`)
    /// so the param owns an INDEPENDENT copy — caller and callee free distinct
    /// buffers. Bails (caller-retains status quo) when any leaf is not
    /// copy-supported (`Map` / shared / `Option` / `Result`), matching the
    /// struct-param policy. Returns whether entry-copy engaged.
    pub(super) fn make_tuple_param_callee_owned(
        &mut self,
        elems: &[TypeExpr],
        agg_ty: StructType<'ctx>,
        slot: PointerValue<'ctx>,
    ) -> bool {
        if !elems.iter().any(|e| self.type_expr_has_drop_heap(e)) {
            return false;
        }
        let mut stack = Vec::new();
        if !elems
            .iter()
            .all(|e| self.field_copy_supported(e, &mut stack))
        {
            return false;
        }
        for (j, ete) in elems.iter().enumerate() {
            self.deep_copy_one_aggregate_field(slot, agg_ty, j as u32, ete);
        }
        match self.synthesize_tuple_drop_fn_te(agg_ty, elems) {
            Some(drop_fn) => {
                if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                    frame.push(super::state::CleanupAction::StructDrop {
                        struct_alloca: slot,
                        drop_fn,
                    });
                }
                true
            }
            None => false,
        }
    }

    /// Recursively decide whether a struct's heap content can be soundly
    /// outer-buffer-copied to mirror its drop. `stack` guards against
    /// self-referential owned structs (which would recurse forever — bail).
    /// B-2026-06-14-28 — does this struct transitively own a `shared`
    /// (RC-pointer) field? Used to classify a plain struct carried inline as
    /// a shared-enum-variant payload (`Add(BinOp)`, `BinOp { left: Expr,
    /// right: Expr }`) so the enum-box RC drop walker rc-dec's its inline RC
    /// children. Walks direct shared fields, `Option[shared T]` fields, and
    /// recurses through nested non-shared struct / tuple fields; `stack`
    /// guards self-reference. Conservative on collections/enums (they don't
    /// hold a *direct* shared edge this walk needs to dec — their own drop
    /// machinery handles inner shared values).
    pub(super) fn struct_owns_shared_field(
        &self,
        struct_name: &str,
        stack: &mut Vec<String>,
    ) -> bool {
        if stack.iter().any(|s| s == struct_name) {
            return false;
        }
        let Some(ftes) = self.struct_field_type_exprs.get(struct_name).cloned() else {
            return false;
        };
        stack.push(struct_name.to_string());
        let owns = ftes.iter().any(|fte| self.field_owns_shared(fte, stack));
        stack.pop();
        owns
    }

    /// Name-set companion to `option_inner_shared_type_for_type_expr`: does
    /// `Option[T]` / `Result[T, _]` have a shared `T`, judged by the early
    /// `shared_type_decl_names` set (before `shared_types` layouts exist)?
    fn option_inner_decl_shared(&self, fte: &TypeExpr) -> bool {
        let TypeKind::Path(p) = &fte.kind else {
            return false;
        };
        let Some(args) = p.generic_args.as_ref() else {
            return false;
        };
        args.iter().any(|a| {
            if let crate::ast::GenericArg::Type(t) = a {
                if let TypeKind::Path(ip) = &t.kind {
                    if let Some(name) = ip.segments.last() {
                        return self.shared_type_decl_names.contains(name.as_str());
                    }
                }
            }
            false
        })
    }

    fn field_owns_shared(&self, fte: &TypeExpr, stack: &mut Vec<String>) -> bool {
        match &fte.kind {
            TypeKind::Tuple(elems) => elems.iter().any(|e| self.field_owns_shared(e, stack)),
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str).unwrap_or("");
                // A direct shared field (the `Expr` edge) — the one we dec.
                // Use the NAME set (`shared_type_decl_names`), not
                // `shared_types`: this classifier runs inside `declare_enums`,
                // before `shared_types` is populated for `Expr` (B-2026-06-14-28).
                if self.shared_type_decl_names.contains(head) {
                    return true;
                }
                // `Option[shared T]` — the inner shared edge is reachable.
                if (head == "Option" || head == "Result") && self.option_inner_decl_shared(fte) {
                    return true;
                }
                // B-2026-06-14-31 — a `Vec[shared T]` field also owns a shared
                // edge: its element boxes are RC pointers that must be dec'd
                // when the owning struct drops (the `Call(CallExpr { args:
                // Vec[Expr] })` shape). Without this, a struct whose ONLY
                // shared content is a `Vec[shared]` would be classified as
                // non-walkable and the shared-enum box drop would skip its
                // payload entirely, leaking the buffer + every element box.
                // Judged by the NAME set (same reason as the direct-field
                // case): this runs before `shared_types` is populated.
                if (head == "Vec" || head == "VecDeque") && self.option_inner_decl_shared(fte) {
                    return true;
                }
                // Recurse through a nested non-shared user struct.
                if self.struct_field_type_exprs.contains_key(head)
                    && !self.shared_type_decl_names.contains(head)
                {
                    return self.struct_owns_shared_field(head, stack);
                }
                false
            }
            _ => false,
        }
    }

    pub(super) fn aggregate_param_copy_supported_struct(
        &self,
        struct_name: &str,
        stack: &mut Vec<String>,
    ) -> bool {
        if stack.iter().any(|s| s == struct_name) {
            return false;
        }
        if self.shared_types.contains_key(struct_name) {
            return false;
        }
        let Some(ftes) = self.struct_field_type_exprs.get(struct_name).cloned() else {
            return false;
        };
        stack.push(struct_name.to_string());
        let ok = ftes.iter().all(|fte| self.field_copy_supported(fte, stack));
        stack.pop();
        ok
    }

    fn field_copy_supported(&self, fte: &TypeExpr, stack: &mut Vec<String>) -> bool {
        match &fte.kind {
            TypeKind::Tuple(elems) => elems.iter().all(|e| self.field_copy_supported(e, stack)),
            // Borrows carry no owned heap — the struct drop never frees them.
            TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_) => true,
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str).unwrap_or("");
                match head {
                    // "str" is the typechecker-internal spelling of `String` (see
                    // `is_string_type_expr` / `type_expr_has_drop_heap`, which
                    // already treat the two as synonyms). It appears here when a
                    // generic field's bare `T` is resolved through a MONOMORPH
                    // subst whose value is `str` — e.g. a METHOD monomorph
                    // (`impl[T] Box[T] { fn get(self) -> T { self.v } }` at
                    // T=String records `type_subst_names["T"] = "str"`, whereas the
                    // free-fn twin records `String`). Omitting it made
                    // `aggregate_param_copy_supported_struct_mono` bail, so the
                    // generic method's owned `self` was NOT entry-copied (stayed a
                    // caller-retains alias) and returning `self.v` double-freed the
                    // aliased buffer (B-2026-07-18-44; the free-fn twin already
                    // worked via the `String` spelling). `deep_copy_one_aggregate_
                    // field` copies it correctly (it keys on `is_string_type_expr`,
                    // which matches both spellings).
                    "String" | "str" | "Vec" | "VecDeque" => true,
                    "Slice" => true,
                    // Heap the outer-buffer copy can't duplicate → bail.
                    "Map" | "HashMap" | "Set" | "HashSet" | "SortedSet" | "SortedMap"
                    | "BTreeMap" | "BTreeSet" => false,
                    // HTTP side-table handle structs (see emit_struct_drop_synthesis).
                    "Response" | "RequestBuilder" => false,
                    // B-2026-07-03-28 Facet A — an `Option[String]`/`Option[Vec[..]]`
                    // field with an inline `{ptr,len,cap}` payload IS copyable:
                    // `deep_copy_option_inline_payload_in_place` duplicates the
                    // `Some` buffer type-aware off the field TypeExpr, and the
                    // struct drop's `OptionInline` arm (gated on this same
                    // copy-supported predicate) frees it — copy == drop, so a
                    // callee-owned copy and the caller's retained original own
                    // independent buffers. An `Option[shared]` field is ALSO
                    // copyable (B-2026-07-03-28 shared leg): its inline payload is
                    // a single RC box pointer (word 1, ptrtoint), so the "copy" is
                    // an rc-INC of the box when Some
                    // (`deep_copy_option_inline_payload_in_place`'s shared branch),
                    // symmetric with the Vec-element / destructure-leaf drop's
                    // `Option[shared]` rc-DEC (`emit_nested_struct_shared_rc_decs_ex`
                    // / `RcDecOption`). Other `Option` payloads (boxed-wide,
                    // struct/enum-inline, plain-enum = B-27) and every `Result`
                    // stay caller-retains (this routine can't duplicate them, and
                    // the drop correspondingly leaves them excluded).
                    // B-2026-07-18-2: under for-loop strict-shared mode an
                    // `Option` field is UNSUPPORTED — a shared-bearing struct's
                    // drain (synthesized as non-copy-supported) skips Option
                    // fields, so a registered element's aliased Option leaf
                    // would lose its leaf-cleanup and leak.
                    "Option" if self.copy_support_for_loop_shared_mode => false,
                    "Option" => {
                        Self::option_payload_te(fte)
                            .map(|pt| {
                                self.is_string_type_expr(&pt)
                                    || self.extract_vec_elem_type(&pt).is_some()
                            })
                            .unwrap_or(false)
                            || self.option_inner_shared_type_for_type_expr(fte).is_some()
                            // B-2026-07-04-7 — an `Option[<non-shared struct/enum>]`
                            // field is ALSO copyable: its `Some` payload is either
                            // BOXED (wider than the 3-word inline area) or inline in
                            // words 1..3, and `deep_copy_option_struct_enum_payload_in_place`
                            // duplicates it (allocating a fresh box, deep-copying the
                            // payload's heap) — the copy peer of `emit_option_drop_fn`'s
                            // boxed/inline free (`option_payload_struct_or_enum_drop_ok`).
                            // Symmetric copy == drop, so a callee-owned copy and the
                            // caller's retained original own independent heap.
                            || Self::option_payload_te(fte)
                                .map(|pt| self.option_payload_struct_or_enum_copyable(&pt, stack))
                                .unwrap_or(false)
                    }
                    "Result" => false,
                    _ if is_primitive_type_name(head) => true,
                    // B-2026-07-18-2: a DIRECT `shared` handle field is copyable
                    // in for-loop strict-shared mode — the "copy" is an rc-INC of
                    // the box (`deep_copy_rc_inc_bare_shared` arm), symmetric with
                    // the drop's rc-DEC. Hard bail outside that mode (entry-copy
                    // / clone / drop-synthesis gates keep their meaning).
                    _ if self.shared_types.contains_key(head) => {
                        self.copy_support_for_loop_shared_mode
                    }
                    _ if self.struct_types.contains_key(head) => {
                        self.aggregate_param_copy_supported_struct(head, stack)
                    }
                    // User enum field (#19 FIXED 2026-06-12). Without entry-copy,
                    // a by-value transfer of an enum-field struct (`let b =
                    // wrap(a)`, `wrap(s: Span) -> Span { s }`) leaves `b` shallow-
                    // aliasing the source's enum buffer; post-#15 BOTH struct drops
                    // free it → double-free (#19). `EnumDropKind` only ever frees a
                    // `VecOrString` payload — exactly what
                    // `deep_copy_enum_heap_payload_in_place` duplicates (wired into
                    // `deep_copy_one_aggregate_field`) — so entry-copy is symmetric
                    // with the struct drop's enum-field free: whatever the drop
                    // frees, the copy copies; carved-out nested-aggregate payloads
                    // are `EnumDropKind::None`, freed by neither. Shared enums bail
                    // at the `shared_types` arm above; Option/Result bail above too,
                    // so any enum reaching here is a non-shared user enum.
                    _ if self.enum_layouts.contains_key(head) => !self.enum_layouts[head].is_shared,
                    // Generic type param / unknown → conservative bail.
                    _ => false,
                }
            }
            // Array[T, N] of heap, fn-ptr types, etc. → conservative bail.
            _ => false,
        }
    }

    /// Does `emit_struct_clone_fn` produce a fully INDEPENDENT deep copy of this
    /// struct — every heap field duplicated into its own allocation with no
    /// aliasing back to the source? This is STRICTLY narrower than
    /// `aggregate_param_copy_supported_struct`, which describes what the
    /// deep-copy-ON-ENTRY path (`deep_copy_*_in_place`) can duplicate. That path
    /// handles `Vec[shared]` / `Option[shared]` (rc-inc'ing the shared elements);
    /// the CLONE path (`emit_clone_fn_for_type_expr`) does NOT — it shallow-copies
    /// a shared handle with no refcount bump, so cloning a struct that transitively
    /// owns a shared element aliases it and later double-frees / SEGVs. This
    /// predicate therefore admits ONLY String, primitive, `Vec`/`VecDeque` of a
    /// clone-duplicable element, and nested clone-duplicable structs; it bails on
    /// any shared type, `Option`/`Result`/enum, `Map`/`Set`, or `Slice` field. It
    /// gates the B-2026-07-09-12 deep-clone-on-bind so only the shapes the clone
    /// infra reproduces exactly are upgraded from view to owned copy.
    pub(super) fn struct_clone_fully_duplicates(
        &self,
        struct_name: &str,
        stack: &mut Vec<String>,
    ) -> bool {
        if stack.iter().any(|s| s == struct_name) {
            return false;
        }
        if self.shared_types.contains_key(struct_name) {
            return false;
        }
        let Some(ftes) = self.struct_field_type_exprs.get(struct_name).cloned() else {
            return false;
        };
        stack.push(struct_name.to_string());
        let ok = ftes
            .iter()
            .all(|fte| self.clone_field_fully_duplicates(fte, stack));
        stack.pop();
        ok
    }

    fn clone_field_fully_duplicates(&self, fte: &TypeExpr, stack: &mut Vec<String>) -> bool {
        match &fte.kind {
            TypeKind::Tuple(elems) => elems
                .iter()
                .all(|e| self.clone_field_fully_duplicates(e, stack)),
            // Borrows carry no owned heap — the clone leaves them as shared views
            // and the struct drop never frees them.
            TypeKind::Ref(_) | TypeKind::MutRef(_) => true,
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str).unwrap_or("");
                match head {
                    "String" => true,
                    // Vec/VecDeque clone deep-copies the buffer AND clones each
                    // element — sound only when the element itself fully
                    // duplicates (so a `Vec[shared]` bails here: its element clone
                    // would shallow-copy the handle).
                    "Vec" | "VecDeque" => match p.generic_args.as_ref().and_then(|a| a.first()) {
                        Some(crate::ast::GenericArg::Type(elem_te)) => {
                            self.clone_field_fully_duplicates(elem_te, stack)
                        }
                        _ => false,
                    },
                    _ if is_primitive_type_name(head) => true,
                    // B-2026-07-18-2: a DIRECT `shared` handle field is copyable
                    // in for-loop strict-shared mode — the "copy" is an rc-INC of
                    // the box (`deep_copy_rc_inc_bare_shared` arm), symmetric with
                    // the drop's rc-DEC. Hard bail outside that mode (entry-copy
                    // / clone / drop-synthesis gates keep their meaning).
                    _ if self.shared_types.contains_key(head) => {
                        self.copy_support_for_loop_shared_mode
                    }
                    _ if self.struct_types.contains_key(head) => {
                        self.struct_clone_fully_duplicates(head, stack)
                    }
                    // Option / Result / user enum (the clone infra's niche +
                    // shared-payload gaps), Map / Set, Slice, and unknowns → bail.
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// B-2026-07-04-7 — is an `Option[P]` payload `P` (a non-shared user
    /// struct/enum) deep-COPYABLE, so `field_copy_supported`'s `Option` arm can
    /// admit it (making the owning struct callee-owned and its `OptionInline`
    /// drop safe)? The drop side (`emit_option_drop_fn`, gated on
    /// `option_payload_struct_or_enum_drop_ok`) already frees such a payload; the
    /// copy peer is `deep_copy_option_struct_enum_payload_in_place`, which for a
    /// STRUCT recurses via `deep_copy_struct_heap_fields_in_place` (so require the
    /// struct be recursively copy-supported — copy-depth == drop-depth) and for a
    /// non-shared ENUM via `deep_copy_enum_heap_payload_in_place` (the SAME
    /// machinery a DIRECT non-shared enum field already trusts in
    /// `field_copy_supported`'s enum arm, so admit any non-shared enum here too).
    fn option_payload_struct_or_enum_copyable(
        &self,
        payload_te: &TypeExpr,
        stack: &mut Vec<String>,
    ) -> bool {
        if !self.option_payload_struct_or_enum_drop_ok(payload_te) {
            return false;
        }
        let TypeKind::Path(p) = &payload_te.kind else {
            return false;
        };
        let head = p.segments.first().map(String::as_str).unwrap_or("");
        if self.shared_types.contains_key(head) {
            return false;
        }
        if self.struct_types.contains_key(head) {
            return self.aggregate_param_copy_supported_struct(head, stack);
        }
        self.enum_layouts
            .get(head)
            .map(|l| !l.is_shared)
            .unwrap_or(false)
    }

    /// Deep-copy every Vec/String heap field of the struct value at `base_ptr`,
    /// recursing into nested structs/tuples. Mirrors
    /// `emit_struct_drop_synthesis`'s field walk.
    pub(super) fn deep_copy_struct_heap_fields_in_place(
        &mut self,
        base_ptr: PointerValue<'ctx>,
        struct_name: &str,
    ) {
        let Some(&st) = self.struct_types.get(struct_name) else {
            return;
        };
        let Some(ftes) = self.struct_field_type_exprs.get(struct_name).cloned() else {
            return;
        };
        for (i, fte) in ftes.iter().enumerate() {
            self.deep_copy_one_aggregate_field(base_ptr, st, i as u32, fte);
        }
    }

    /// B-2026-07-18-31/-32 — make a GENERIC by-value struct param callee-owned
    /// when its fields are bare type params erased by the base layout but bound
    /// to copy-supported heap types by the active monomorph subst. Returns
    /// `true` (ownership taken: mono entry-copy + mono drop registered) or
    /// `false` (left caller-retains, unchanged). Only engages when (a) an active
    /// subst exists, (b) the concrete mono struct layout is buildable, and (c)
    /// every field, resolved through the subst, is copy-supported. Outside a
    /// monomorph — or for a struct whose fields don't all resolve to
    /// copy-supported types — it is a no-op, so non-generic params and the
    /// existing base path are untouched.
    fn try_make_generic_struct_param_callee_owned(
        &mut self,
        type_name: &str,
        slot: PointerValue<'ctx>,
    ) -> bool {
        // The mono layout drives the field GEPs; absent it (no active subst /
        // non-generic struct) there is nothing to widen, so bail to
        // caller-retains exactly as before.
        let Some(mono_st) = self.mono_struct_type_from_active_subst(type_name) else {
            return false;
        };
        if !self.aggregate_param_copy_supported_struct_mono(type_name) {
            return false;
        }
        let saved = self.deep_copy_rc_inc_bare_shared;
        self.deep_copy_rc_inc_bare_shared = true;
        self.deep_copy_struct_heap_fields_in_place_mono(slot, type_name, mono_st);
        self.deep_copy_rc_inc_bare_shared = saved;
        // Register the PER-MONOMORPH drop (`__karac_drop_struct_Pair$str`) so a
        // field NOT moved out (e.g. `fn peek[T](p: Pair[T]) -> T { p.a.clone()
        // }`) still frees its entry-copied buffer; the base drop skips the
        // erased bare-`T` fields and would leak it. Fields that ARE moved out
        // have their caps zeroed by `suppress_struct_field_move_into_literal`,
        // so the mono drop no-ops on them.
        match self.active_subst_struct_inst(type_name) {
            Some(inst) => self.track_struct_var_inst(type_name, slot, Some(inst)),
            None => self.track_struct_var(type_name, slot),
        }
        true
    }

    /// Mono twin of [`aggregate_param_copy_supported_struct`]: resolve each
    /// declared field `TypeExpr` through the ACTIVE monomorph subst before
    /// classifying, so a generic struct whose fields are bare type params
    /// (`Pair[T] { a: T, b: T }`) is judged on its CONCRETE instantiation
    /// (`T = String` → `String` is copyable) instead of bailing at
    /// `field_copy_supported`'s bare-`T` `_ => false` arm. A field that resolves
    /// to a NESTED generic struct (`Inner[T]` → `Inner[String]`) still recurses
    /// through the base `aggregate_param_copy_supported_struct`, which reads
    /// `Inner`'s own erased bare-`T` fields and bails — so a nested-generic
    /// field keeps the caller-retains behavior (a documented residual, same
    /// class as B-2026-07-15-11's single-field gate).
    fn aggregate_param_copy_supported_struct_mono(&self, struct_name: &str) -> bool {
        if self.shared_types.contains_key(struct_name) {
            return false;
        }
        let Some(ftes) = self.struct_field_type_exprs.get(struct_name).cloned() else {
            return false;
        };
        // A struct with no heap-typed field after resolution has nothing to copy
        // — treat as unsupported so we don't needlessly flip it to callee-owned
        // (its base path already returned false, i.e. caller-retains no-op).
        let mut any_heap = false;
        for fte in &ftes {
            let resolved = self.subst_monomorph_type_params(fte);
            if !self.field_copy_supported(&resolved, &mut vec![struct_name.to_string()]) {
                return false;
            }
            if self.type_expr_has_drop_heap(&resolved) {
                any_heap = true;
            }
        }
        any_heap
    }

    /// Mono twin of [`deep_copy_struct_heap_fields_in_place`]: GEP at the
    /// CONCRETE mono struct layout (`mono_st`) and classify each field through
    /// its subst-resolved `TypeExpr`, so a generic struct param's bare-`T` heap
    /// fields (`Pair[String]`) are entry-copied at their real `{ptr,len,cap}`
    /// offsets. The base-layout twin GEPs the erased `{i64,…}` and reads bare
    /// `T`, so it copies nothing (B-2026-07-18-32).
    fn deep_copy_struct_heap_fields_in_place_mono(
        &mut self,
        base_ptr: PointerValue<'ctx>,
        struct_name: &str,
        mono_st: StructType<'ctx>,
    ) {
        let Some(ftes) = self.struct_field_type_exprs.get(struct_name).cloned() else {
            return;
        };
        for (i, fte) in ftes.iter().enumerate() {
            let resolved = self.subst_monomorph_type_params(fte);
            self.deep_copy_one_aggregate_field(base_ptr, mono_st, i as u32, &resolved);
        }
    }

    /// Build the concrete generic instantiation `TypeExpr` (`Pair[String]`) for
    /// `struct_name` from the ACTIVE monomorph subst, so a callee-owned generic
    /// param's scope-exit drop is registered as the per-monomorph
    /// `__karac_drop_struct_Pair$str`. `None` when the struct declares no
    /// generic params or the subst binds none of them (in which case the caller
    /// falls back to the base name-keyed drop).
    fn active_subst_struct_inst(&self, struct_name: &str) -> Option<TypeExpr> {
        use crate::ast::{GenericArg, PathExpr};
        use crate::token::Span;
        let params = self.struct_generic_params.get(struct_name)?;
        if params.is_empty() {
            return None;
        }
        let mut args = Vec::with_capacity(params.len());
        for p in params {
            let te = if let Some(full) = self.type_subst_type_exprs.get(p) {
                full.clone()
            } else {
                let name = self.type_subst_names.get(p)?;
                TypeExpr {
                    kind: TypeKind::Path(PathExpr {
                        segments: vec![name.clone()],
                        generic_args: None,
                        span: Span::default(),
                    }),
                    span: Span::default(),
                }
            };
            args.push(GenericArg::Type(te));
        }
        Some(TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![struct_name.to_string()],
                generic_args: Some(args),
                span: Span::default(),
            }),
            span: Span::default(),
        })
    }

    /// Rc-INC the shared box handle stored at `slot` (an 8-byte RC pointer word),
    /// null-guarded. The "copy" of a shared handle is a refcount bump so the copy
    /// co-owns the box, symmetric with the drop's rc-DEC. Used by the
    /// B-2026-07-09-12 clone-on-extract (view-destructure) path.
    pub(super) fn rc_inc_shared_handle_at_slot(
        &self,
        slot: PointerValue<'ctx>,
        heap_type: StructType<'ctx>,
    ) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let handle = self
            .builder
            .build_load(ptr_ty, slot, "viewdup.handle")
            .unwrap()
            .into_pointer_value();
        let is_null = self
            .builder
            .build_is_null(handle, "viewdup.isnull")
            .unwrap();
        let do_bb = self.context.append_basic_block(fn_val, "viewdup.inc.do");
        let cont_bb = self.context.append_basic_block(fn_val, "viewdup.inc.cont");
        self.builder
            .build_conditional_branch(is_null, cont_bb, do_bb)
            .unwrap();
        self.builder.position_at_end(do_bb);
        self.emit_refcount_inc_by_type(heap_type, handle);
        self.builder.build_unconditional_branch(cont_bb).unwrap();
        self.builder.position_at_end(cont_bb);
    }

    /// Rc-INC every element of a `Vec[shared]` value at `vec_field_ptr` whose
    /// outer `{ptr,len,cap}` buffer was just deep-copied, so the duplicated Vec
    /// independently co-owns each element box. Mirrors the whole-Vec drop's
    /// per-element rc-DEC (`emit_vec_elem_rc_dec_fn`: load handle, null-check,
    /// rc-dec). VIEW-SCOPED (B-2026-07-09-12 clone-on-extract): deliberately NOT
    /// wired into the shared `deep_copy_*` param-copy machinery, whose earlier
    /// `Vec[shared]` arm double-inc'd against other per-site inc paths (the
    /// reverted param-path leak). Here the only inc is this one and the leaf's own
    /// per-element rc-dec drop balances it.
    pub(super) fn rc_inc_vec_shared_elements(
        &mut self,
        vec_field_ptr: PointerValue<'ctx>,
        heap_type: StructType<'ctx>,
    ) {
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let vec_ty = self.vec_struct_type();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let (Ok(data_pp), Ok(len_pp)) = (
            self.builder
                .build_struct_gep(vec_ty, vec_field_ptr, 0, "viewvsh.data.pp"),
            self.builder
                .build_struct_gep(vec_ty, vec_field_ptr, 1, "viewvsh.len.pp"),
        ) else {
            return;
        };
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "viewvsh.data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_load(i64_t, len_pp, "viewvsh.len")
            .unwrap()
            .into_int_value();
        let loop_bb = self.context.append_basic_block(fn_val, "viewvsh.loop");
        let body_bb = self.context.append_basic_block(fn_val, "viewvsh.body");
        let exit_bb = self.context.append_basic_block(fn_val, "viewvsh.exit");
        let pre_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(loop_bb).unwrap();
        self.builder.position_at_end(loop_bb);
        let idx_phi = self.builder.build_phi(i64_t, "viewvsh.i").unwrap();
        idx_phi.add_incoming(&[(&i64_t.const_int(0, false), pre_bb)]);
        let i = idx_phi.as_basic_value().into_int_value();
        let in_range = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "viewvsh.cmp")
            .unwrap();
        self.builder
            .build_conditional_branch(in_range, body_bb, exit_bb)
            .unwrap();
        self.builder.position_at_end(body_bb);
        // Each element slot is one pointer-width RC handle.
        let slot = unsafe {
            self.builder
                .build_gep(ptr_ty, data, &[i], "viewvsh.slot")
                .unwrap()
        };
        self.rc_inc_shared_handle_at_slot(slot, heap_type);
        let body_end = self.builder.get_insert_block().unwrap();
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "viewvsh.next")
            .unwrap();
        self.builder.build_unconditional_branch(loop_bb).unwrap();
        idx_phi.add_incoming(&[(&next, body_end)]);
        self.builder.position_at_end(exit_bb);
    }

    /// Copy one aggregate field in place per its TypeExpr. String/Vec → outer
    /// buffer copy; nested struct → recurse; tuple → recurse per element;
    /// everything else (primitive, borrow, ignored kinds) → no-op.
    pub(super) fn deep_copy_one_aggregate_field(
        &mut self,
        base_ptr: PointerValue<'ctx>,
        agg_ty: StructType<'ctx>,
        idx: u32,
        fte: &TypeExpr,
    ) {
        let vec_ty = self.vec_struct_type();
        // B-2026-07-10-4 — clone-on-extract mode only (`deep_copy_rc_inc_bare_shared`):
        // a bare `shared` field is a shallow pointer in the copy; rc-INC it so the
        // clone independently co-owns the box, symmetric with the leaf's combined
        // struct-drop rc-DEC. The ENTRY-COPY path leaves the flag false and skips
        // this (its shared handling is unchanged — a bump there leaked).
        if self.deep_copy_rc_inc_bare_shared {
            if let Some(heap_ty) = self.shared_heap_type_for_type_expr(fte) {
                if let Ok(field_ptr) =
                    self.builder
                        .build_struct_gep(agg_ty, base_ptr, idx, "p14.shclone")
                {
                    self.rc_inc_shared_handle_at_slot(field_ptr, heap_ty);
                }
                return;
            }
        }
        // String / Vec field → copy the OUTER buffer in place (`elem_te = None`),
        // mirroring the struct drop's outer-only free (nested Vec elements are a
        // bounded leak on both sides, never corruption).
        let elem_ty: Option<BasicTypeEnum<'ctx>> = if self.is_string_type_expr(fte) {
            Some(self.context.i8_type().into())
        } else {
            self.extract_vec_elem_type(fte)
        };
        if let Some(elem_ty) = elem_ty {
            // B-2026-07-03-28 — copy-depth must equal drop-depth. The struct drop
            // DRAINS a `Vec[elem]` field's `String`/`Map`/`Set`/nested-`Vec`
            // elements (`emit_struct_drop_synthesis`'s VecOrString arm via
            // `elem_te_needs_direct_recursive_drain`), so this entry-copy must
            // element-DEEP copy exactly those shapes — else the callee's copy
            // would share the caller's element buffers and both drains would
            // free them (the test-1 double-free). `emit_vecstr_defensive_copy`'s
            // element-deep mode (`elem_te = Some`) duplicates each element's
            // String / Map / Set / inner-Vec buffer; other element shapes stay
            // outer-only (`None`), matching the drop's outer-only handling for
            // them.
            let inner_te = crate::codegen::helpers::vec_inner_type_expr(fte);
            let deep_elem_te = inner_te
                .clone()
                .filter(Self::elem_te_needs_direct_recursive_drain);
            if let Ok(field_ptr) = self
                .builder
                .build_struct_gep(agg_ty, base_ptr, idx, "p14.f")
            {
                if let Ok(val) = self.builder.build_load(vec_ty, field_ptr, "p14.v") {
                    let copied =
                        self.emit_vecstr_defensive_copy(val, elem_ty, deep_elem_te.as_ref());
                    let _ = self.builder.build_store(field_ptr, copied);
                }
                // B-2026-07-04-9(a) — a `Vec[struct]` / `Vec[enum]` / `Vec[Option]`
                // element whose per-element drop
                // (`vec_elem_agg_drop_for_type_expr`) frees inner heap the OUTER
                // `{ptr,len,cap}` copy above cannot reach (`type_expr_has_drop_heap`
                // is FALSE for an all-`Option` struct like `ArgN`, so the
                // `emit_vecstr_defensive_copy` agg branch — and `emit_clone_fn`,
                // whose Option copy is shallow — both miss it). After the outer
                // buffer is duplicated, deep-copy each copied element in place with
                // the SAME machinery the entry-copy uses for a nested struct field
                // (`deep_copy_struct_heap_fields_in_place` / enum / Option), which —
                // unlike `emit_clone_fn` — duplicates `Option[String]` buffers and
                // rc-INCs `Option[shared]` boxes, symmetric with the drop's
                // per-element free / rc-dec. Without this the copied element buffers
                // alias the source and both drains free them (double-free in
                // `__karac_drop_struct_<Outer>`).
                if let Some(elem_te) = inner_te.as_ref() {
                    self.deep_copy_vec_aggregate_elements_in_place(agg_ty, base_ptr, idx, elem_te);
                }
            }
            return;
        }
        // Nested non-shared user struct → recurse into it in place.
        if let TypeKind::Path(p) = &fte.kind {
            if let Some(head) = p.segments.first() {
                if self.struct_types.contains_key(head.as_str())
                    && !self.shared_types.contains_key(head.as_str())
                {
                    if let Ok(field_ptr) = self
                        .builder
                        .build_struct_gep(agg_ty, base_ptr, idx, "p14.nf")
                    {
                        let name = head.clone();
                        self.deep_copy_struct_heap_fields_in_place(field_ptr, &name);
                    }
                    return;
                }
            }
        }
        // Nested user-ENUM field (#19 FIXED) → deep-copy its live-variant
        // Vec/String payload in place, mirroring the struct drop's per-field enum
        // free (`emit_struct_drop_synthesis`'s `EnumField` arm → `__karac_drop_<E>`).
        // `deep_copy_enum_heap_payload_in_place` duplicates exactly the
        // `VecOrString` payloads `EnumDropKind` frees, so the entry-copy stays
        // symmetric with the drop. Shared enums / Option / Result never reach here
        // — `field_copy_supported` bails on them, so the struct is caller-retains.
        if let TypeKind::Path(p) = &fte.kind {
            if let Some(head) = p.segments.first() {
                if let Some(layout) = self.enum_layouts.get(head.as_str()).cloned() {
                    if !layout.is_shared && head != "Option" && head != "Result" {
                        if let Ok(field_ptr) = self
                            .builder
                            .build_struct_gep(agg_ty, base_ptr, idx, "p14.ef")
                        {
                            let name = head.clone();
                            self.deep_copy_enum_heap_payload_in_place(&name, field_ptr, &layout);
                        }
                        return;
                    }
                }
            }
        }
        // B-2026-07-03-28 Facet A — an `Option[String]`/`Option[Vec[..]]` field
        // (inline `{ptr,len,cap}` payload): deep-copy the `Some` buffer in place
        // so a callee-owned param owns it independently, symmetric with the
        // struct drop's `OptionInline` free. `field_copy_supported` already
        // vetted the payload class, so any Option reaching here is copyable.
        if let TypeKind::Path(p) = &fte.kind {
            if p.segments.last().map(|s| s.as_str()) == Some("Option") {
                if let Ok(field_ptr) = self
                    .builder
                    .build_struct_gep(agg_ty, base_ptr, idx, "p14.of")
                {
                    self.deep_copy_option_inline_payload_in_place(field_ptr, fte);
                }
                return;
            }
        }
        // Tuple field → recurse into each element.
        if let TypeKind::Tuple(elems) = &fte.kind {
            if !elems.is_empty() {
                if let (Ok(field_ptr), Some(BasicTypeEnum::StructType(tup_ty))) = (
                    self.builder
                        .build_struct_gep(agg_ty, base_ptr, idx, "p14.tf"),
                    agg_ty.get_field_type_at_index(idx),
                ) {
                    for (j, ete) in elems.iter().enumerate() {
                        self.deep_copy_one_aggregate_field(field_ptr, tup_ty, j as u32, ete);
                    }
                }
            }
        }
        // Primitive / borrow / ignored kind → nothing to copy.
    }

    /// B-2026-07-04-9(a) — deep-copy each element of an already-outer-copied
    /// `Vec[<aggregate>]` struct FIELD in place. The outer buffer copy above
    /// duplicates the `{ptr,len,cap}` array, but each element is a shallow
    /// bit-copy still aliasing the source's per-element heap; the struct drop
    /// DRAINS those elements (`vec_elem_agg_drop_for_type_expr`), so without a
    /// per-element deep copy the callee's whole-drop and the caller's retained
    /// drop free the SAME element buffers (double-free in
    /// `__karac_drop_struct_<Outer>`). This reuses the SAME field-copy machinery
    /// the entry-copy uses for a nested aggregate field — a struct element via
    /// `deep_copy_struct_heap_fields_in_place`, an enum element via
    /// `deep_copy_enum_heap_payload_in_place`, an `Option` element via
    /// `deep_copy_option_inline_payload_in_place` — which (unlike the
    /// `emit_vecstr_defensive_copy` / `emit_clone_fn` agg path, shallow for
    /// `Option`) duplicates `Option[String]` buffers and rc-INCs `Option[shared]`
    /// boxes, symmetric with the per-element drop's free / rc-dec. Bare `shared`
    /// elements (`Vec[shared]` — an 8-byte RC pointer slot) and no-heap elements
    /// are skipped: the former's drop is a pure rc-dec needing a paired
    /// per-element rc-inc (a distinct residual), the latter needs no copy.
    fn deep_copy_vec_aggregate_elements_in_place(
        &mut self,
        agg_ty: StructType<'ctx>,
        base_ptr: PointerValue<'ctx>,
        idx: u32,
        elem_te: &TypeExpr,
    ) {
        // B-2026-07-10-4 — clone-on-extract / symmetric-entry-copy mode only: a bare
        // `shared` element (`Vec[TypeExpr]` variant-field list, `Vec[Expr]` arg list)
        // is an 8-byte RC pointer the outer buffer copy aliased without a refcount
        // bump; rc-INC each so the duplicate co-owns the element boxes, symmetric
        // with the struct drop's per-element rc-dec (`vec_elem_agg_drop_for_type_expr`
        // → `__karac_vec_elem_rc_dec_<T>`). Flag off (plain entry-copy) skips this —
        // its Vec[shared] element handling is unchanged.
        if self.deep_copy_rc_inc_bare_shared {
            if let Some(heap_ty) = self.shared_heap_type_for_type_expr(elem_te) {
                if let Ok(field_ptr) =
                    self.builder
                        .build_struct_gep(agg_ty, base_ptr, idx, "p14a.shvec")
                {
                    self.rc_inc_vec_shared_elements(field_ptr, heap_ty);
                }
                return;
            }
        }
        // Classify the element; bail unless it is a value-deep-copyable
        // aggregate whose per-element drop frees inner heap.
        enum ElemCopy {
            Struct(String),
            Enum(String),
            Option,
        }
        let plan = match &elem_te.kind {
            TypeKind::Path(p) => {
                let name = p.segments.first().map(String::as_str).unwrap_or("");
                if name == "Option" {
                    // Only the inline `Some`-payload shapes the drop actually
                    // frees (`vec_elem_agg_drop_for_type_expr`'s Option arm).
                    let frees = Self::option_payload_te(elem_te)
                        .map(|pt| {
                            self.option_payload_inline_recursive_drop_ok(&pt)
                                || self.option_payload_struct_or_enum_drop_ok(&pt)
                        })
                        .unwrap_or(false);
                    frees.then_some(ElemCopy::Option)
                } else if self.shared_heap_type_for_type_expr(elem_te).is_some() {
                    // Bare `shared` element — rc-inc case, handled elsewhere.
                    None
                } else if self.struct_types.contains_key(name)
                    && !self.shared_types.contains_key(name)
                {
                    Some(ElemCopy::Struct(name.to_string()))
                } else if self
                    .enum_layouts
                    .get(name)
                    .map(|l| !l.is_shared)
                    .unwrap_or(false)
                {
                    Some(ElemCopy::Enum(name.to_string()))
                } else {
                    None
                }
            }
            _ => None,
        };
        let Some(plan) = plan else {
            return;
        };

        let fn_val = match self.current_fn {
            Some(f) => f,
            None => return,
        };
        let vec_ty = self.vec_struct_type();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let elem_ty = self.llvm_type_for_type_expr(elem_te);

        // Reload the (now outer-copied) Vec field's data ptr + len.
        let Ok(field_ptr) = self
            .builder
            .build_struct_gep(agg_ty, base_ptr, idx, "p14a.f")
        else {
            return;
        };
        let (Ok(data_pp), Ok(len_pp)) = (
            self.builder
                .build_struct_gep(vec_ty, field_ptr, 0, "p14a.data.pp"),
            self.builder
                .build_struct_gep(vec_ty, field_ptr, 1, "p14a.len.pp"),
        ) else {
            return;
        };
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "p14a.data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_load(i64_t, len_pp, "p14a.len")
            .unwrap()
            .into_int_value();

        // Per-element loop `0..len` (empty Vec runs zero iterations).
        let loop_bb = self.context.append_basic_block(fn_val, "p14a.loop");
        let body_bb = self.context.append_basic_block(fn_val, "p14a.body");
        let exit_bb = self.context.append_basic_block(fn_val, "p14a.exit");
        let pre_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(loop_bb).unwrap();

        self.builder.position_at_end(loop_bb);
        let idx_phi = self.builder.build_phi(i64_t, "p14a.i").unwrap();
        idx_phi.add_incoming(&[(&i64_t.const_int(0, false), pre_bb)]);
        let i = idx_phi.as_basic_value().into_int_value();
        let in_range = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "p14a.cmp")
            .unwrap();
        self.builder
            .build_conditional_branch(in_range, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let slot = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[i], "p14a.slot")
                .unwrap()
        };
        match &plan {
            ElemCopy::Struct(name) => self.deep_copy_struct_heap_fields_in_place(slot, name),
            ElemCopy::Enum(name) => {
                if let Some(layout) = self.enum_layouts.get(name).cloned() {
                    self.deep_copy_enum_heap_payload_in_place(name, slot, &layout);
                }
            }
            ElemCopy::Option => self.deep_copy_option_inline_payload_in_place(slot, elem_te),
        }
        // A sub-copy may have appended blocks and moved the insert point —
        // branch back from wherever we now are.
        let body_end = self.builder.get_insert_block().unwrap();
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "p14a.next")
            .unwrap();
        self.builder.build_unconditional_branch(loop_bb).unwrap();
        idx_phi.add_incoming(&[(&next, body_end)]);

        self.builder.position_at_end(exit_bb);
    }

    /// Deep-copy (outer buffers only) the live variant's Vec/String payload of
    /// the enum value at `base_ptr`. Emits a tag switch mirroring
    /// `emit_enum_drop_switch`; only variants with a VecOrString payload get a
    /// case. The enum's payload words are stored as raw i64s (data = ptrtoint,
    /// then len, then cap), so the copy reconstructs a `{ptr,len,cap}` value,
    /// runs `emit_vecstr_defensive_copy`, and writes the copied words back.
    pub(super) fn deep_copy_enum_heap_payload_in_place(
        &mut self,
        enum_name: &str,
        base_ptr: PointerValue<'ctx>,
        layout: &EnumLayout<'ctx>,
    ) {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let enum_ty = layout.llvm_type;
        let fn_val = self.current_fn.unwrap();

        // Per-variant payload element TypeExprs (for buffer-element sizing).
        let variant_tes: HashMap<String, Vec<TypeExpr>> = self
            .enum_variant_field_type_exprs(enum_name)
            .into_iter()
            .map(|(_tag, name, tes)| (name, tes))
            .collect();

        let tag_ptr = self
            .builder
            .build_struct_gep(enum_ty, base_ptr, 0, "p14e.tag.p")
            .unwrap();
        let tag = self
            .builder
            .build_load(i64_t, tag_ptr, "p14e.tag")
            .unwrap()
            .into_int_value();

        let mut tag_entries: Vec<(String, u64)> =
            layout.tags.iter().map(|(n, t)| (n.clone(), *t)).collect();
        tag_entries.sort_by_key(|(_, t)| *t);

        let merge_bb = self.context.append_basic_block(fn_val, "p14e.merge");
        let mut cases: Vec<(
            inkwell::values::IntValue<'ctx>,
            inkwell::basic_block::BasicBlock<'ctx>,
        )> = Vec::new();
        let mut case_bbs: Vec<(String, inkwell::basic_block::BasicBlock<'ctx>)> = Vec::new();
        for (name, tag_v) in &tag_entries {
            let has_heap = layout
                .field_drop_kinds
                .get(name)
                .map(|ks| ks.iter().any(|k| k.is_heap_bearing()))
                .unwrap_or(false);
            if !has_heap {
                continue;
            }
            let bb = self
                .context
                .append_basic_block(fn_val, &format!("p14e.{name}"));
            cases.push((i64_t.const_int(*tag_v, false), bb));
            case_bbs.push((name.clone(), bb));
        }

        self.builder.build_switch(tag, merge_bb, &cases).unwrap();

        for (name, bb) in &case_bbs {
            self.builder.position_at_end(*bb);
            if let (Some(kinds), Some(offsets)) = (
                layout.field_drop_kinds.get(name),
                layout.field_word_offsets.get(name),
            ) {
                for (fi, (kind, (start_word, _num_words))) in
                    kinds.iter().zip(offsets.iter()).enumerate()
                {
                    // B-2026-06-13-13: a nested-struct payload is deep-copied by
                    // recursing into the struct's own heap fields in place — the
                    // symmetric peer of the enum drop's `NestedStruct` arm, so the
                    // callee copy and caller temp own independent buffers (no
                    // double-free). The struct's words start at `start_word + 1`.
                    if *kind == EnumDropKind::NestedStruct {
                        let sname =
                            variant_tes
                                .get(name)
                                .and_then(|tes| tes.get(fi))
                                .and_then(|te| match &te.kind {
                                    TypeKind::Path(p) => p.segments.first().cloned(),
                                    _ => None,
                                });
                        if let Some(sname) = sname {
                            if let Ok(field_ptr) = self.builder.build_struct_gep(
                                enum_ty,
                                base_ptr,
                                (*start_word + 1) as u32,
                                "p14e.nstruct.p",
                            ) {
                                self.deep_copy_struct_heap_fields_in_place(field_ptr, &sname);
                            }
                        }
                        continue;
                    }
                    if *kind != EnumDropKind::VecOrString {
                        continue;
                    }
                    let data_idx = (*start_word + 1) as u32;
                    let len_idx = (*start_word + 2) as u32;
                    let cap_idx = (*start_word + 3) as u32;

                    let data_w = self.load_enum_word(enum_ty, base_ptr, data_idx, "p14e.data");
                    let len_w = self.load_enum_word(enum_ty, base_ptr, len_idx, "p14e.len");
                    let cap_w = self.load_enum_word(enum_ty, base_ptr, cap_idx, "p14e.cap");
                    let data_p = self
                        .builder
                        .build_int_to_ptr(data_w, ptr_ty, "p14e.data.p")
                        .unwrap();

                    // Reconstruct the {ptr,len,cap} value the defensive copy expects.
                    let mut sv = vec_ty.get_undef();
                    sv = self
                        .builder
                        .build_insert_value(sv, data_p, 0, "p14e.sv.d")
                        .unwrap()
                        .into_struct_value();
                    sv = self
                        .builder
                        .build_insert_value(sv, len_w, 1, "p14e.sv.l")
                        .unwrap()
                        .into_struct_value();
                    sv = self
                        .builder
                        .build_insert_value(sv, cap_w, 2, "p14e.sv.c")
                        .unwrap()
                        .into_struct_value();

                    let elem_ty: BasicTypeEnum<'ctx> = variant_tes
                        .get(name)
                        .and_then(|tes| tes.get(fi))
                        .map(|te| {
                            if self.is_string_type_expr(te) {
                                self.context.i8_type().into()
                            } else {
                                self.extract_vec_elem_type(te)
                                    .unwrap_or_else(|| self.context.i8_type().into())
                            }
                        })
                        .unwrap_or_else(|| self.context.i8_type().into());

                    // Outer-buffer copy (`elem_te = None`), mirroring the enum
                    // drop's outer-only payload free.
                    let copied = self
                        .emit_vecstr_defensive_copy(sv.into(), elem_ty, None)
                        .into_struct_value();
                    let cd = self
                        .builder
                        .build_extract_value(copied, 0, "p14e.cd")
                        .unwrap()
                        .into_pointer_value();
                    let cl = self
                        .builder
                        .build_extract_value(copied, 1, "p14e.cl")
                        .unwrap()
                        .into_int_value();
                    let cc = self
                        .builder
                        .build_extract_value(copied, 2, "p14e.cc")
                        .unwrap()
                        .into_int_value();
                    let cd_w = self
                        .builder
                        .build_ptr_to_int(cd, i64_t, "p14e.cd.w")
                        .unwrap();

                    self.store_enum_word(enum_ty, base_ptr, data_idx, cd_w.into());
                    self.store_enum_word(enum_ty, base_ptr, len_idx, cl.into());
                    self.store_enum_word(enum_ty, base_ptr, cap_idx, cc.into());
                }
            }
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        self.builder.position_at_end(merge_bb);
    }

    /// B-2026-07-03-28 Facet A — deep-copy an `Option[String]` / `Option[Vec[..]]`
    /// FIELD's inline `Some` payload in place, so a callee-owned by-value
    /// aggregate param owns a buffer independent of the caller's retained
    /// original. The type-erased `Option` layout carries no payload drop-kind, so
    /// this is TYPE-AWARE off the field's `TypeExpr` (the copy peer of the
    /// type-aware `emit_option_drop_fn`): tag-switch on `Some`, reconstruct the
    /// inline `{ptr,len,cap}` from words 1..3, run `emit_vecstr_defensive_copy`
    /// (element-DEEP for a `Vec[String]`/collection payload, matching the drop),
    /// and write the fresh `{ptr,len,cap}` words back. `None`-tag runs nothing.
    /// Only the inline-`{ptr,len,cap}` payload class is handled here (the same
    /// class `option_inline_payload_elem` recognises); `field_copy_supported`'s
    /// `Option` arm gates callers to exactly that, keeping copy == drop.
    pub(super) fn deep_copy_option_inline_payload_in_place(
        &mut self,
        field_ptr: PointerValue<'ctx>,
        opt_te: &TypeExpr,
    ) {
        // B-2026-07-03-28 shared leg — an `Option[shared]` payload is a single
        // inline RC box pointer (word 1, ptrtoint), NOT an `{ptr,len,cap}`
        // buffer. The caller-retains entry-copy of it is an rc-INC of the box
        // when Some (so the callee's copy holds an independent ref), the exact
        // peer of `emit_nested_struct_shared_rc_decs_ex`'s `Option[shared]`
        // rc-DEC arm. Handle it before the String/Vec buffer-copy path (which
        // would `return` early on a shared payload).
        if let Some((_, inner_info)) = self.option_inner_shared_type_for_type_expr(opt_te) {
            self.rc_inc_option_inline_shared_payload_in_place(field_ptr, inner_info.heap_type);
            return;
        }
        let Some(payload_te) = Self::option_payload_te(opt_te) else {
            return;
        };
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let fn_val = self.current_fn.unwrap();
        let Some(layout) = self.enum_layouts.get("Option").cloned() else {
            return;
        };
        let option_ty = layout.llvm_type;
        let some_tag = layout.tags.get("Some").copied().unwrap_or(1);

        // Element type + (for a Vec[collection] payload) the element-deep TypeExpr,
        // mirroring the drained-Vec entry-copy so a `Vec[String]` payload's char
        // buffers are copied too, matching `emit_option_drop_fn`'s deep free.
        let (elem_ty, deep_elem_te): (BasicTypeEnum<'ctx>, Option<TypeExpr>) =
            if self.is_string_type_expr(&payload_te) {
                (self.context.i8_type().into(), None)
            } else if let Some(et) = self.extract_vec_elem_type(&payload_te) {
                let inner = crate::codegen::helpers::vec_inner_type_expr(&payload_te)
                    .filter(Self::elem_te_needs_direct_recursive_drain);
                (et, inner)
            } else {
                // B-2026-07-04-7 — a non-shared struct/enum payload (BOXED when
                // wider than the 3-word inline area, else inline in words 1..3),
                // not the `{ptr,len,cap}` overlay this fn's buffer-copy path
                // handles. Deep-copy it via the box-aware peer of
                // `emit_option_drop_fn`'s boxed/inline payload free. Pass the OUTER
                // `opt_te` (`Option[Val]`) — the helper re-extracts the payload
                // itself; passing `payload_te` would make its `option_payload_te`
                // return `None` and silently copy nothing (→ shared box → double-free).
                self.deep_copy_option_struct_enum_payload_in_place(field_ptr, opt_te);
                return;
            };

        let tag_ptr = self
            .builder
            .build_struct_gep(option_ty, field_ptr, 0, "p14o.tag.p")
            .unwrap();
        let tag = self
            .builder
            .build_load(i64_t, tag_ptr, "p14o.tag")
            .unwrap()
            .into_int_value();
        let is_some = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                i64_t.const_int(some_tag, false),
                "p14o.some",
            )
            .unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "p14o.some");
        let merge_bb = self.context.append_basic_block(fn_val, "p14o.merge");
        self.builder
            .build_conditional_branch(is_some, some_bb, merge_bb)
            .unwrap();

        self.builder.position_at_end(some_bb);
        // Words: data=idx1, len=idx2, cap=idx3.
        let data_w = self.load_enum_word(option_ty, field_ptr, 1, "p14o.data");
        let len_w = self.load_enum_word(option_ty, field_ptr, 2, "p14o.len");
        let cap_w = self.load_enum_word(option_ty, field_ptr, 3, "p14o.cap");
        let data_p = self
            .builder
            .build_int_to_ptr(data_w, ptr_ty, "p14o.data.p")
            .unwrap();
        let mut sv = vec_ty.get_undef();
        sv = self
            .builder
            .build_insert_value(sv, data_p, 0, "p14o.sv.d")
            .unwrap()
            .into_struct_value();
        sv = self
            .builder
            .build_insert_value(sv, len_w, 1, "p14o.sv.l")
            .unwrap()
            .into_struct_value();
        sv = self
            .builder
            .build_insert_value(sv, cap_w, 2, "p14o.sv.c")
            .unwrap()
            .into_struct_value();
        let copied = self
            .emit_vecstr_defensive_copy(sv.into(), elem_ty, deep_elem_te.as_ref())
            .into_struct_value();
        let cd = self
            .builder
            .build_extract_value(copied, 0, "p14o.cd")
            .unwrap()
            .into_pointer_value();
        let cl = self
            .builder
            .build_extract_value(copied, 1, "p14o.cl")
            .unwrap();
        let cc = self
            .builder
            .build_extract_value(copied, 2, "p14o.cc")
            .unwrap();
        let cd_w = self
            .builder
            .build_ptr_to_int(cd, i64_t, "p14o.cd.w")
            .unwrap();
        self.store_enum_word(option_ty, field_ptr, 1, cd_w.into());
        self.store_enum_word(option_ty, field_ptr, 2, cl);
        self.store_enum_word(option_ty, field_ptr, 3, cc);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
    }

    /// B-2026-07-04-7 — deep-copy an `Option[<non-shared struct/enum>]` FIELD's
    /// `Some` payload in place, so a callee-owned by-value aggregate param owns
    /// heap independent of the caller's retained original. Unlike the
    /// `{ptr,len,cap}` String/Vec overlay (`deep_copy_option_inline_payload_in_place`)
    /// and the single-RC-pointer shared payload (`rc_inc_..._shared_...`), a
    /// struct/enum payload is either BOXED — when its LLVM word count exceeds the
    /// 3-word inline area, exactly the predicate `coerce_to_payload_words` boxes
    /// on — with word 1 holding the box pointer, or INLINE overlaying words 1..3.
    /// This is the copy peer of `emit_option_drop_fn`'s boxed/inline branch: on
    /// `Some`, if boxed, `malloc` a fresh box, shallow-copy the payload value in,
    /// then deep-copy its heap fields in place (`deep_copy_{struct,enum}_...`) and
    /// store the new box pointer; if inline, deep-copy the payload's heap fields
    /// in place over the Option's payload words. The deep-copy helpers duplicate
    /// exactly the buffers the payload's own `__karac_drop_*` frees (copy ==
    /// drop), so the callee copy and caller original own independent heap. `None`
    /// runs nothing.
    fn deep_copy_option_struct_enum_payload_in_place(
        &mut self,
        field_ptr: PointerValue<'ctx>,
        opt_te: &TypeExpr,
    ) {
        let Some(payload_te) = Self::option_payload_te(opt_te) else {
            return;
        };
        let payload_name = match &payload_te.kind {
            TypeKind::Path(p) => p.segments.first().cloned(),
            _ => None,
        };
        let Some(payload_name) = payload_name else {
            return;
        };
        if self.shared_types.contains_key(&payload_name) {
            return;
        }
        let is_struct = self.struct_types.contains_key(&payload_name);
        let enum_layout = self
            .enum_layouts
            .get(&payload_name)
            .filter(|l| !l.is_shared)
            .cloned();
        if !is_struct && enum_layout.is_none() {
            return;
        }

        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_val = self.current_fn.unwrap();
        let Some(layout) = self.enum_layouts.get("Option").cloned() else {
            return;
        };
        let option_ty = layout.llvm_type;
        let some_tag = layout.tags.get("Some").copied().unwrap_or(1);

        let tag_ptr = self
            .builder
            .build_struct_gep(option_ty, field_ptr, 0, "p14oe.tag.p")
            .unwrap();
        let tag = self
            .builder
            .build_load(i64_t, tag_ptr, "p14oe.tag")
            .unwrap()
            .into_int_value();
        let is_some = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                i64_t.const_int(some_tag, false),
                "p14oe.some",
            )
            .unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "p14oe.some");
        let merge_bb = self.context.append_basic_block(fn_val, "p14oe.merge");
        self.builder
            .build_conditional_branch(is_some, some_bb, merge_bb)
            .unwrap();

        self.builder.position_at_end(some_bb);
        let payload_llty = self.llvm_type_for_type_expr(&payload_te);
        let payload_words = Self::llvm_type_word_count(payload_llty);
        // Payload area starts at Option field index 1.
        let payload_base = self
            .builder
            .build_struct_gep(option_ty, field_ptr, 1, "p14oe.pl")
            .unwrap();
        if payload_words > 3 {
            // BOXED — word 1 holds the box pointer. Allocate a fresh box, copy
            // the payload value in, deep-copy its heap in place, store the new
            // pointer. Null-guarded (a Some tag with a null box can't occur, but
            // mirror `emit_option_drop_fn`'s box null-guard for symmetry).
            let old_w = self
                .builder
                .build_load(i64_t, payload_base, "p14oe.box.w0")
                .unwrap()
                .into_int_value();
            let old_box = self
                .builder
                .build_int_to_ptr(old_w, ptr_ty, "p14oe.oldbox")
                .unwrap();
            let old_null = self
                .builder
                .build_is_null(old_box, "p14oe.oldbox.null")
                .unwrap();
            let copy_bb = self.context.append_basic_block(fn_val, "p14oe.box.copy");
            self.builder
                .build_conditional_branch(old_null, merge_bb, copy_bb)
                .unwrap();
            self.builder.position_at_end(copy_bb);
            let raw_size = payload_llty.size_of().unwrap();
            let size = if raw_size.get_type().get_bit_width() == 64 {
                raw_size
            } else {
                self.builder
                    .build_int_z_extend(raw_size, i64_t, "p14oe.sz64")
                    .unwrap()
            };
            let new_box = self
                .builder
                .build_call(self.malloc_fn, &[size.into()], "p14oe.newbox")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let boxval = self
                .builder
                .build_load(payload_llty, old_box, "p14oe.boxval")
                .unwrap();
            self.builder.build_store(new_box, boxval).unwrap();
            if let Some(el) = &enum_layout {
                self.deep_copy_enum_heap_payload_in_place(&payload_name, new_box, el);
            } else {
                self.deep_copy_struct_heap_fields_in_place(new_box, &payload_name);
            }
            let new_w = self
                .builder
                .build_ptr_to_int(new_box, i64_t, "p14oe.newbox.w")
                .unwrap();
            self.builder.build_store(payload_base, new_w).unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        } else {
            // INLINE — the payload overlays words 1..3 in place; deep-copy its
            // heap fields directly (`payload_base` reinterprets as `payload_llty*`).
            if let Some(el) = &enum_layout {
                self.deep_copy_enum_heap_payload_in_place(&payload_name, payload_base, el);
            } else {
                self.deep_copy_struct_heap_fields_in_place(payload_base, &payload_name);
            }
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        self.builder.position_at_end(merge_bb);
    }

    /// B-2026-07-03-28 shared leg — rc-INC an `Option[shared]` FIELD's inline
    /// box pointer (word 1, ptrtoint) when Some, so a callee-owned by-value
    /// aggregate param holds an independent ref to the shared box. The exact
    /// inc peer of `emit_nested_struct_shared_rc_decs_ex`'s `Option[shared]`
    /// rc-dec arm (synth_drop.rs): read the Option tag, and on Some load word 1
    /// as i64, `int_to_ptr`, null-guard, and `emit_refcount_inc_by_type` on the
    /// recovered box. A `None` payload runs nothing. Symmetric copy == drop, so
    /// the callee copy and the caller's retained original both own a ref that
    /// each drop path (Vec-element / destructure-leaf) rc-decs exactly once.
    fn rc_inc_option_inline_shared_payload_in_place(
        &mut self,
        field_ptr: PointerValue<'ctx>,
        heap_type: StructType<'ctx>,
    ) {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_val = self.current_fn.unwrap();
        let Some(layout) = self.enum_layouts.get("Option").cloned() else {
            return;
        };
        let option_ty = layout.llvm_type;
        let some_tag = layout.tags.get("Some").copied().unwrap_or(1);

        let tag_ptr = self
            .builder
            .build_struct_gep(option_ty, field_ptr, 0, "p14os.tag.p")
            .unwrap();
        let tag = self
            .builder
            .build_load(i64_t, tag_ptr, "p14os.tag")
            .unwrap()
            .into_int_value();
        let is_some = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                i64_t.const_int(some_tag, false),
                "p14os.some",
            )
            .unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "p14os.some");
        let merge_bb = self.context.append_basic_block(fn_val, "p14os.merge");
        self.builder
            .build_conditional_branch(is_some, some_bb, merge_bb)
            .unwrap();

        self.builder.position_at_end(some_bb);
        let w1 = self.load_enum_word(option_ty, field_ptr, 1, "p14os.w1");
        let inner = self
            .builder
            .build_int_to_ptr(w1, ptr_ty, "p14os.inner")
            .unwrap();
        let inner_null = self
            .builder
            .build_is_null(inner, "p14os.inner.isnull")
            .unwrap();
        let inc_bb = self.context.append_basic_block(fn_val, "p14os.inc.do");
        let skip_bb = self.context.append_basic_block(fn_val, "p14os.inc.skip");
        self.builder
            .build_conditional_branch(inner_null, skip_bb, inc_bb)
            .unwrap();
        self.builder.position_at_end(inc_bb);
        self.emit_refcount_inc_by_type(heap_type, inner);
        self.builder.build_unconditional_branch(skip_bb).unwrap();
        self.builder.position_at_end(skip_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
    }

    /// The payload `TypeExpr` of an `Option[T]` type expr, else `None`.
    pub(super) fn option_payload_te(opt_te: &TypeExpr) -> Option<TypeExpr> {
        let TypeKind::Path(p) = &opt_te.kind else {
            return None;
        };
        if p.segments.last().map(|s| s.as_str()) != Some("Option") {
            return None;
        }
        match p.generic_args.as_ref()?.first()? {
            crate::ast::GenericArg::Type(t) => Some(t.clone()),
            _ => None,
        }
    }

    /// B-2026-07-08-9: split a `Result[T, E]` `TypeExpr` into its concrete
    /// `(ok, err)` payload `TypeExpr`s. Sibling of `option_payload_te` for the
    /// Display path. Returns `None` for a non-`Result` type or a `Result`
    /// missing either generic arg.
    pub(super) fn result_payload_tes(res_te: &TypeExpr) -> Option<(TypeExpr, TypeExpr)> {
        let TypeKind::Path(p) = &res_te.kind else {
            return None;
        };
        if p.segments.last().map(|s| s.as_str()) != Some("Result") {
            return None;
        }
        let args = p.generic_args.as_ref()?;
        let ok = match args.first()? {
            crate::ast::GenericArg::Type(t) => t.clone(),
            _ => return None,
        };
        let err = match args.get(1)? {
            crate::ast::GenericArg::Type(t) => t.clone(),
            _ => return None,
        };
        Some((ok, err))
    }

    /// #14 — at a struct-literal field init `S { f: obj.field }` whose value is
    /// a heap FIELD moved out of a tracked struct binding `obj` (a callee-owned
    /// by-value aggregate param, or a local), cap-zero that field's buffer in
    /// `obj`'s slot so `obj`'s `StructDrop` skips it — the new struct literal is
    /// now the sole owner. This is the field-access peer of the whole-Identifier
    /// `suppress_source_vec_cleanup_for_arg` (which the literal path already
    /// calls), and the analog of its TupleIndex arm.
    ///
    /// SCOPED to struct-literal field inits, where the value is genuinely MOVED
    /// into the new owner — NOT folded into the general suppression funnel,
    /// which also fires at by-value-arg sites where the callee may not take
    /// ownership (cap-zeroing there would leak). Without this, a builder method
    /// that moves `self`'s fields into a returned literal
    /// (`LogEvent { level: self.level, message: self.message, … }`) double-frees
    /// once `self` is a callee-owned by-value aggregate param — the source field
    /// AND the returned literal both free the same buffer (std.tracing's
    /// `with_field`).
    /// B-2026-07-15-24 — the concrete generic instantiation of a struct field
    /// moved out via `let bound = o.field`, derived from `o`'s recorded
    /// instantiation and the field's declared type. `o: GOuter[Vec[i64]]`,
    /// `field inner: GInner[T]` → `GInner[Vec[i64]]`. Feeds (a) the moved-out
    /// binding's own mono drop (so it GEPs the per-monomorph layout, not the
    /// base erased one) and (b) the source-field mono cap-zeroing in
    /// `suppress_struct_field_move_into_literal`. `None` unless `o` has a
    /// recorded generic instantiation and the field resolves to a generic named
    /// struct — a non-generic field (or an unrecorded object) keeps the
    /// name-shared base-layout behavior, unchanged.
    pub(super) fn field_move_out_struct_inst(&self, value: &Expr) -> Option<TypeExpr> {
        let ExprKind::FieldAccess { object, field } = &value.kind else {
            return None;
        };
        let obj_name = match &object.kind {
            ExprKind::Identifier(o) => o.as_str(),
            ExprKind::SelfValue => "self",
            _ => return None,
        };
        let obj_inst = self.enum_inst_var_types.get(obj_name)?;
        let TypeKind::Path(op) = &obj_inst.kind else {
            return None;
        };
        let obj_struct = op.segments.last()?.clone();
        let obj_args = op.generic_args.as_ref()?;
        let params = self.struct_generic_params.get(&obj_struct)?;
        if params.len() != obj_args.len() {
            return None;
        }
        let mut subst: std::collections::HashMap<String, TypeExpr> =
            std::collections::HashMap::new();
        for (p, a) in params.iter().zip(obj_args.iter()) {
            if let crate::ast::GenericArg::Type(te) = a {
                subst.insert(p.clone(), te.clone());
            }
        }
        if subst.is_empty() {
            return None;
        }
        let field_idx = self
            .struct_field_names
            .get(&obj_struct)?
            .iter()
            .position(|n| n == field)?;
        let fte = self
            .struct_field_type_exprs
            .get(&obj_struct)?
            .get(field_idx)?;
        let resolved = crate::codegen::helpers::subst_type_params_in_type_expr(fte, &subst);
        self.is_generic_named_struct_type_expr(&resolved)
            .then_some(resolved)
    }

    pub(super) fn suppress_struct_field_move_into_literal(&self, value: &Expr) {
        let ExprKind::FieldAccess { object, field } = &value.kind else {
            return;
        };
        // The source root is either a named binding (`obj.field`) or the method
        // receiver (`self.field`) — `self` is bound as an ordinary local named
        // "self" by `compile_function`. The std.tracing builder bodies move
        // `self.fields` / `self.message` out, so SelfValue must resolve here or
        // the move-out suppression never fires (#17 gap 2).
        let s: &str = match &object.kind {
            ExprKind::Identifier(s) => s.as_str(),
            ExprKind::SelfValue => "self",
            _ => return,
        };
        let Some(slot) = self.variables.get(s).copied() else {
            return;
        };
        let BasicTypeEnum::StructType(agg_ty) = slot.ty else {
            return;
        };
        let vec_ty = self.vec_struct_type();
        if agg_ty == vec_ty {
            return;
        }
        let Some(sname) = self.var_type_names.get(s).cloned() else {
            return;
        };
        let Some(idx) = self
            .struct_field_names
            .get(sname.as_str())
            .and_then(|names| names.iter().position(|n| n == field))
        else {
            return;
        };
        let field_llvm = agg_ty.get_field_type_at_index(idx as u32);
        let Ok(field_ptr) =
            self.builder
                .build_struct_gep(agg_ty, slot.ptr, idx as u32, "p14.fldmv.p")
        else {
            return;
        };
        // The field's declared type name (`inner: Inner` → "Inner"), used to
        // route named-struct / enum fields to the type-name-driven suppressors
        // that the LLVM-type-only match below can't classify (a Map/Set handle
        // and an enum's payload are both bare-word/ptr layouts indistinguishable
        // from other fields by LLVM type alone).
        let field_type_name: Option<String> = self
            .struct_field_type_exprs
            .get(sname.as_str())
            .and_then(|ftes| ftes.get(idx))
            .and_then(|fte| match &fte.kind {
                TypeKind::Path(p) => p.segments.first().cloned(),
                _ => None,
            });
        match field_llvm {
            // Direct Vec/String field → zero its cap (drop's `cap > 0` skips).
            Some(BasicTypeEnum::StructType(fst)) if fst == vec_ty => {
                if let Ok(cap_ptr) =
                    self.builder
                        .build_struct_gep(vec_ty, field_ptr, 2, "p14.fldmv.cap")
                {
                    let _ = self
                        .builder
                        .build_store(cap_ptr, self.context.i64_type().const_int(0, false));
                }
            }
            // A nested aggregate field: a named non-shared STRUCT, an ENUM, or a
            // tuple whose drop frees heap leaves the moved-out binding now owns.
            Some(BasicTypeEnum::StructType(fst)) => {
                // Named non-shared struct field (`inner: Inner`) — route through
                // the type-name-driven `zero_struct_move_caps`, which uniformly
                // disarms Vec/String (cap+len), Map/Set (null the handle —
                // B-2026-07-15-23), enum payloads, and nested structs. The
                // LLVM-type-driven `aggregate_has_heap_field` /
                // `zero_aggregate_field_caps` path below sees NEITHER a Map/Set
                // handle (a bare `ptr`, not the vec-struct) NOR an enum leaf (all
                // -i64 words), so a moved-out struct carrying only a Map/Set or an
                // enum field would otherwise leave the SOURCE live and double-free
                // it against `bound`'s drop (df9/dfB/dfC Map-SIGSEGV, dfD enum
                // double-free — all `karac check`-clean). `zero_struct_move_caps`
                // uses the base struct layout; for a generic monomorph whose base
                // erases a bare-`T` heap field the Vec/String path below (mono
                // LLVM type) is the precise one — the two overlap idempotently on
                // Vec, so run both.
                if let Some(name) = field_type_name.as_deref() {
                    if self.struct_types.contains_key(name) && !self.shared_types.contains_key(name)
                    {
                        // B-2026-07-15-24 — derive the moved-out field's concrete
                        // mono instantiation (`GInner[Vec[i64]]`) so the source
                        // cap-zeroing GEPs the PER-MONOMORPH layout. Without it a
                        // bare-`T` heap field placed before a Map/Vec field in the
                        // nested struct mis-offsets the handle null-store (base
                        // erased layout), leaving the source live → double-free /
                        // SIGSEGV on the nested move-out. A non-generic field
                        // yields `None` → the name-shared base-layout suppression,
                        // unchanged.
                        let nsub = self
                            .field_move_out_struct_inst(value)
                            .map(|inst| self.generic_struct_subst_from_inst(name, &inst))
                            .filter(|s| !s.is_empty());
                        self.zero_struct_move_caps_mono(field_ptr, name, nsub.as_ref());
                    } else if let Some(layout) = self.enum_layouts.get(name) {
                        // Enum field (#19) — cap-zero its `VecOrString` payload
                        // words so the owning struct's drop skips the buffer the
                        // moved-out binding now owns (`let tk = t.token`). Shared
                        // enums carry RC (no `VecOrString` kind) and self-skip;
                        // Option/Result have no static kind and `zero_enum_payload_caps`
                        // no-ops for them.
                        if !layout.is_shared {
                            let layout = layout.clone();
                            self.zero_enum_payload_caps(field_ptr, &layout);
                        }
                    }
                }
                // Mono-correct Vec/String cap-zero for a nested aggregate whose
                // heap is a directly-visible (possibly bare-`T`-monomorphized)
                // Vec/String field.
                if self.aggregate_has_heap_field(fst) {
                    self.zero_aggregate_field_caps(field_ptr, fst);
                }
            }
            _ => {}
        }
    }

    /// #27 (B-2026-06-14-8) — `let tk = h.ps.0.tok`: an enum field moved OUT of
    /// a struct that is itself nested in a TUPLE element. The source `value` is a
    /// `FieldAccess` whose OBJECT is a deeper place (`h.ps.0`, a `TupleIndex`),
    /// which [`Self::suppress_struct_field_move_into_literal`] (Identifier/`self`
    /// object only) can't reach. Resolve the field via the place-chain machinery
    /// ([`Self::field_chain_place_ptr`] / [`Self::place_chain_type_name`]) and
    /// cap-zero the enum payload in the owning struct's slot, so its drop skips
    /// the buffer the moved-out `tk` now owns (else double-free). Self-gates to a
    /// non-Identifier/`self` object (the shallow forms keep their dedicated
    /// suppressor), a non-owned-param root, and a non-shared user enum field.
    pub(super) fn suppress_place_field_enum_move_source(&mut self, value: &Expr) {
        let ExprKind::FieldAccess { object, field } = &value.kind else {
            return;
        };
        // Shallow forms (`obj.field` / `self.field`) are handled by
        // `suppress_struct_field_move_into_literal`; only a DEEPER place here.
        if matches!(object.kind, ExprKind::Identifier(_) | ExprKind::SelfValue) {
            return;
        }
        match Self::place_root_ident(value) {
            Some(root) if self.owned_struct_params.contains(root) => return,
            Some(_) => {}
            None => return,
        }
        let Some(obj_ty) = self.place_chain_type_name(object) else {
            return;
        };
        let Some(idx) = self
            .struct_field_names
            .get(obj_ty.as_str())
            .and_then(|names| names.iter().position(|n| n == field))
        else {
            return;
        };
        // The moved-out field must be a non-shared user enum (the only case that
        // double-frees through the owning struct's drop; Vec/String/struct fields
        // through a tuple element are a separate follow-on, not yet observed).
        let Some(ename) = self
            .struct_field_type_names
            .get(obj_ty.as_str())
            .and_then(|tns| tns.get(idx))
            .and_then(|n| n.clone())
        else {
            return;
        };
        let Some(layout) = self.enum_layouts.get(ename.as_str()).cloned() else {
            return;
        };
        if layout.is_shared {
            return;
        }
        let Some(st) = self.struct_types.get(obj_ty.as_str()).copied() else {
            return;
        };
        let Some(base_ptr) = self.field_chain_place_ptr(object) else {
            return;
        };
        let Ok(field_ptr) = self
            .builder
            .build_struct_gep(st, base_ptr, idx as u32, "p27.encap.p")
        else {
            return;
        };
        self.zero_enum_payload_caps(field_ptr, &layout);
    }

    fn load_enum_word(
        &self,
        enum_ty: StructType<'ctx>,
        base_ptr: PointerValue<'ctx>,
        idx: u32,
        name: &str,
    ) -> inkwell::values::IntValue<'ctx> {
        let i64_t = self.context.i64_type();
        let p = self
            .builder
            .build_struct_gep(enum_ty, base_ptr, idx, name)
            .unwrap();
        self.builder
            .build_load(i64_t, p, name)
            .unwrap()
            .into_int_value()
    }

    fn store_enum_word(
        &self,
        enum_ty: StructType<'ctx>,
        base_ptr: PointerValue<'ctx>,
        idx: u32,
        val: inkwell::values::BasicValueEnum<'ctx>,
    ) {
        if let Ok(p) = self
            .builder
            .build_struct_gep(enum_ty, base_ptr, idx, "p14e.store.p")
        {
            let _ = self.builder.build_store(p, val);
        }
    }
}

fn is_primitive_type_name(name: &str) -> bool {
    matches!(
        name,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
            | "bool"
            | "char"
    )
}
