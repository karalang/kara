//! Small-String Optimization (SSO) — codegen-side tag helpers.
//!
//! These mirror, bit-for-bit, the runtime encoding contract in
//! `runtime/src/sso.rs` (the single source of truth). A Kāra `String`
//! reuses its 24-byte `{ptr, len, cap}` descriptor to store short strings
//! inline; the discriminant is the **sign bit (bit 63) of `cap`**:
//!
//! | state       | `cap` viewed as `i64` | drop |
//! |-------------|-----------------------|------|
//! | static-heap | `cap == 0`            | none |
//! | owned-heap  | `cap > 0`             | `free(data)` |
//! | inline      | `cap < 0`             | none |
//!
//! Encoding the flag as the sign bit is what lets the buffer-free decision
//! stay a single signed compare, [`Codegen::sso_string_is_owned_heap`]
//! (`SGT cap, 0`). That predicate is a *provable no-op today*: no code
//! path has ever produced a `cap` with bit 63 set (a real capacity never
//! approaches 2^63), so it is identical to the historical `UGT cap, 0`
//! until inline construction is switched on in a later slice. `Vec` never
//! sets the flag, so routing a `Vec` buffer-free through this predicate is
//! byte-identical to before — the accessors are correctness-safe for both.
//!
//! See `docs/spikes/small-string-optimization.md` for the staged plan.

use inkwell::values::{FunctionValue, IntValue, PointerValue, StructValue};
use inkwell::IntPredicate;

/// Is inline **construction** — and therefore the tag-aware read path that
/// must accompany it — switched on?
///
/// Default **off** while Slice 2 lands. The accessors below are the whole
/// read surface SSO needs, and they are being routed into ~430 field-0 /
/// field-1 read sites over several commits. A half-swept tag-aware String
/// surface is the shape that produces silent data corruption, so every
/// intermediate commit aims to be a no-op at SSO=off: each accessor emits
/// exactly the raw load it replaced, so the IR does not move at all — not
/// merely "the branch is never taken at runtime".
///
/// **That is no longer literally true of the whole campaign, and the weaker
/// claim is the honest one.** The commit that turned on inline construction
/// also flipped 14 buffer-free gates from `UGT` to `SGT` UNCONDITIONALLY (they
/// have to be inline-safe before any inline value exists, and gating them on
/// an env var would leave the unsafe predicate live by default). At SSO=off
/// that tree is *semantically* identical with 14 predicates flipped, not
/// byte-identical — which is why five IR-assertion tests in `tests/codegen.rs`
/// had to be updated to expect `icmp sgt`.
///
/// `KARAC_SSO=1` (also `on` / `true`) turns the whole thing on, which is how
/// the sweep is tested ahead of the default flip. Read once — this is asked
/// per read site, and an `env::var` per site would show up in compile time.
fn sso_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("KARAC_SSO")
            .map(|v| {
                let v = v.trim();
                v == "1" || v.eq_ignore_ascii_case("on") || v.eq_ignore_ascii_case("true")
            })
            .unwrap_or(false)
    })
}

impl<'ctx> super::Codegen<'ctx> {
    /// Size in bytes of the `{ptr, len, cap}` descriptor — and therefore
    /// the exact span an inline String's bytes occupy. Mirrors
    /// `size_of::<RuntimeKaracString>()`, which `runtime/src/sso.rs` pins
    /// with a layout test. Copying a whole descriptor is how an inline
    /// String is cloned/moved: the bytes travel inside it.
    pub(super) const STRING_DESCRIPTOR_BYTES: u64 = 24;

    /// The owned-heap predicate: `(i64) cap > 0`. True only when the
    /// descriptor owns a malloc'd buffer that a drop must `free` — inline
    /// (`cap < 0`) and static-literal (`cap == 0`) both answer false.
    ///
    /// This is the tag-aware replacement for the historical
    /// `IntPredicate::UGT cap, 0` buffer-free gate. Emitting `SGT` costs
    /// the same one instruction and is a no-op until the inline flag is
    /// ever set, but it makes every buffer-free path inline-safe ahead of
    /// the construction slice. Safe for `Vec` (whose `cap` is always a
    /// non-negative element count, so `SGT` and `UGT` agree).
    pub(super) fn sso_string_is_owned_heap(&self, cap: IntValue<'ctx>) -> IntValue<'ctx> {
        let zero = cap.get_type().const_zero();
        self.builder
            .build_int_compare(IntPredicate::SGT, cap, zero, "sso.owned_heap")
            .unwrap()
    }

    /// The inline predicate: `(i64) cap < 0` (the flag / sign bit is set).
    ///
    /// Mirrors `RuntimeKaracString::is_inline`. Every read that would
    /// otherwise trust a raw `data`/`len` field on a String must branch on
    /// this first: when the flag is set those two fields are overlaid data
    /// bytes, not a heap descriptor.
    pub(super) fn sso_string_is_inline(&self, cap: IntValue<'ctx>) -> IntValue<'ctx> {
        let zero = cap.get_type().const_zero();
        self.builder
            .build_int_compare(IntPredicate::SLT, cap, zero, "sso.inline")
            .unwrap()
    }

    /// Whether the tag-aware String read/construct path is switched on.
    /// See [`sso_enabled`] — off by default while the sweep lands.
    pub(super) fn sso_on(&self) -> bool {
        sso_enabled()
    }

    /// Promote an inline String at `slot` into ordinary heap form, in place.
    /// A no-op for a heap or static descriptor, and compiled out entirely
    /// when SSO is off.
    ///
    /// This is how MUTATING string ops stay correct without every one of
    /// their reads becoming tag-aware. `push_str` and friends read `len` and
    /// `cap` raw and repeatedly: the growth test, the destination offset, the
    /// aliasing rebase, the post-copy length store. On an inline descriptor
    /// those fields are overlaid data bytes, and the failure is quiet rather
    /// than loud — `needs_grow` is `UGT(new_len, cap)` and an inline `cap` is
    /// negative, so as UNSIGNED it is enormous, the grow is skipped, and the
    /// copy lands at a garbage offset off the end of the descriptor.
    ///
    /// Promoting once at the head of the op leaves every read downstream
    /// looking at a normal heap string, so those paths stay byte-for-byte the
    /// pre-SSO code. It costs a short mutated string its inline win, which is
    /// the right trade: the corpus's short strings are overwhelmingly *read*
    /// (the lexer's per-token `substring` is the motivating case), and folly's
    /// `fbstring` promotes on mutation for the same reason.
    pub(super) fn sso_deinline_in_place(&self, slot: PointerValue<'ctx>, prefix: &str) {
        if !self.sso_on() {
            return;
        }
        let Some(cur) = self.builder.get_insert_block() else {
            return;
        };
        let Some(fn_val) = cur.get_parent() else {
            return;
        };
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let vec_ty = self.vec_struct_type();

        let cap = self.sso_load_cap(slot, prefix);
        let is_inline = self.sso_string_is_inline(cap);
        let promote_bb = self
            .context
            .append_basic_block(fn_val, &format!("{prefix}.deinline"));
        let done_bb = self
            .context
            .append_basic_block(fn_val, &format!("{prefix}.deinline.done"));
        self.builder
            .build_conditional_branch(is_inline, promote_bb, done_bb)
            .unwrap();

        self.builder.position_at_end(promote_bb);
        let n = self.sso_select_len(cap, i64_t.const_zero(), &format!("{prefix}.di"));
        // `n + 1` so the promoted buffer keeps the NUL-terminated contract
        // every other String-producing path maintains.
        let bytes = self
            .builder
            .build_int_add(n, i64_t.const_int(1, false), &format!("{prefix}.di.bytes"))
            .unwrap();
        let buf = self
            .builder
            .build_call(
                self.runtime_fns.alloc_or_panic_fn,
                &[bytes.into()],
                &format!("{prefix}.di.buf"),
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        // Copy BEFORE overwriting any field: the bytes being copied are the
        // descriptor itself, so a store to `data`/`len` would clobber them.
        self.builder.build_memcpy(buf, 1, slot, 1, n).unwrap();
        let nul = unsafe {
            self.builder
                .build_gep(i8_t, buf, &[n], &format!("{prefix}.di.nul"))
                .unwrap()
        };
        self.builder.build_store(nul, i8_t.const_zero()).unwrap();
        let d_p = self
            .builder
            .build_struct_gep(vec_ty, slot, 0, &format!("{prefix}.di.d"))
            .unwrap();
        let l_p = self
            .builder
            .build_struct_gep(vec_ty, slot, 1, &format!("{prefix}.di.l"))
            .unwrap();
        let c_p = self
            .builder
            .build_struct_gep(vec_ty, slot, 2, &format!("{prefix}.di.c"))
            .unwrap();
        self.builder.build_store(d_p, buf).unwrap();
        self.builder.build_store(l_p, n).unwrap();
        self.builder.build_store(c_p, n).unwrap();
        self.builder.build_unconditional_branch(done_bb).unwrap();

        self.builder.position_at_end(done_bb);
    }

    /// Tag-aware data pointer for a String descriptor held in memory at
    /// `slot`. Mirrors `RuntimeKaracString::data_ptr`: an inline string's
    /// bytes begin at the descriptor's own address, a heap or static one's
    /// at field 0.
    ///
    /// `heap` is the caller's already-loaded field-0 value, so a site that
    /// has one does not reload it. Emitted branch-free (one compare, one
    /// select). Correctness-safe for `Vec`, which never sets the flag — the
    /// select always yields `heap`; keeping `Vec` off the select entirely is
    /// the Slice 3 perf refinement.
    pub(super) fn sso_string_data_ptr_from_slot(
        &self,
        slot: PointerValue<'ctx>,
        heap: PointerValue<'ctx>,
        prefix: &str,
    ) -> PointerValue<'ctx> {
        let cap = self.sso_load_cap(slot, prefix);
        let is_inline = self.sso_string_is_inline(cap);
        self.builder
            .build_select(is_inline, slot, heap, &format!("{prefix}.data_ptr"))
            .unwrap()
            .into_pointer_value()
    }

    /// Tag-aware byte length for a String descriptor at `slot`. Mirrors
    /// `RuntimeKaracString::byte_len`: an inline string's length is in bits
    /// 0..=6 of `cap`'s high byte, not the `len` field (which an inline
    /// descriptor overlays with data bytes 8..=15). `heap_len` is the
    /// caller's already-loaded field-1 value.
    pub(super) fn sso_string_len_from_slot(
        &self,
        slot: PointerValue<'ctx>,
        heap_len: IntValue<'ctx>,
        prefix: &str,
    ) -> IntValue<'ctx> {
        let cap = self.sso_load_cap(slot, prefix);
        self.sso_select_len(cap, heap_len, prefix)
    }

    /// Load the `cap` discriminant from a descriptor slot.
    fn sso_load_cap(&self, slot: PointerValue<'ctx>, prefix: &str) -> IntValue<'ctx> {
        let vec_ty = self.vec_struct_type();
        let i64_t = self.context.i64_type();
        let cap_p = self
            .builder
            .build_struct_gep(vec_ty, slot, 2, &format!("{prefix}.cap.p"))
            .unwrap();
        self.builder
            .build_load(i64_t, cap_p, &format!("{prefix}.cap"))
            .unwrap()
            .into_int_value()
    }

    /// `select(cap < 0, (cap >> 56) & 0x7f, heap_len)` — the length half of
    /// the discriminant, split out so a caller that already holds `cap` does
    /// not reload it.
    pub(super) fn sso_select_len(
        &self,
        cap: IntValue<'ctx>,
        heap_len: IntValue<'ctx>,
        prefix: &str,
    ) -> IntValue<'ctx> {
        let i64_t = self.context.i64_type();
        let is_inline = self.sso_string_is_inline(cap);
        // Logical shift: the flag bit must not sign-extend into the length.
        let shifted = self
            .builder
            .build_right_shift(
                cap,
                i64_t.const_int(56, false),
                false,
                &format!("{prefix}.ilen.sh"),
            )
            .unwrap();
        let inline_len = self
            .builder
            .build_and(
                shifted,
                i64_t.const_int(0x7f, false),
                &format!("{prefix}.ilen"),
            )
            .unwrap();
        self.builder
            .build_select(
                is_inline,
                inline_len,
                heap_len,
                &format!("{prefix}.byte_len"),
            )
            .unwrap()
            .into_int_value()
    }

    /// Tag-aware `(data_ptr, byte_len)` for a String held as an **SSA
    /// aggregate** rather than in memory.
    ///
    /// This is the form that needs the spill the campaign flagged as Slice
    /// 2's main new complexity: an inline string's data pointer *is* its
    /// descriptor's address, and an SSA value has none. So the value is
    /// stored to an **entry-block** alloca and read back through the slot
    /// accessors. Entry-block placement matters twice over — the address is
    /// then stable for the whole function, and a call site inside a loop
    /// allocates once rather than growing the frame per iteration.
    ///
    /// The returned pointer is only valid for an **immediate read** (a
    /// memcpy source, a comparison, a runtime call that copies). It must
    /// never be stored into anything outliving the frame: an inline
    /// descriptor is self-referential, so a stored pointer dangles as soon
    /// as the value is copied elsewhere.
    ///
    /// With SSO off this is exactly the two `extract_value`s it replaced —
    /// no alloca, no store, no IR movement at all.
    pub(super) fn sso_string_parts_from_value(
        &self,
        sv: StructValue<'ctx>,
        prefix: &str,
    ) -> (PointerValue<'ctx>, IntValue<'ctx>) {
        let raw_ptr = self
            .builder
            .build_extract_value(sv, 0, &format!("{prefix}.ptr"))
            .unwrap()
            .into_pointer_value();
        let raw_len = self
            .builder
            .build_extract_value(sv, 1, &format!("{prefix}.len"))
            .unwrap()
            .into_int_value();
        if !self.sso_on() {
            return (raw_ptr, raw_len);
        }
        let Some(slot) = self.sso_spill_to_entry_alloca(prefix) else {
            // No entry block to hang the alloca on — cannot happen for a
            // function we are emitting a body into, but degrade to the raw
            // fields rather than panicking mid-emit.
            return (raw_ptr, raw_len);
        };
        self.builder.build_store(slot, sv).unwrap();
        let cap = self.sso_load_cap(slot, prefix);
        let is_inline = self.sso_string_is_inline(cap);
        let data = self
            .builder
            .build_select(is_inline, slot, raw_ptr, &format!("{prefix}.data_ptr"))
            .unwrap()
            .into_pointer_value();
        let len = self.sso_select_len(cap, raw_len, prefix);
        (data, len)
    }

    /// Reserve an entry-block alloca to receive a String descriptor written
    /// by a runtime `*_into` entrypoint. Same entry-block placement rule as
    /// [`Self::sso_spill_to_entry_alloca`]: the address is stable for the
    /// whole function, and a construction site inside a loop reserves one
    /// slot rather than growing the frame per iteration.
    ///
    /// The loaded descriptor is an ordinary owned String value — inline or
    /// heap, per its own `cap` tag — so the enclosing let/temp machinery
    /// frees it at scope exit exactly as before. An inline one has `cap < 0`,
    /// which every buffer-free gate already skips (Slice 1's hardening).
    pub(super) fn sso_descriptor_alloca(
        &self,
        fn_val: FunctionValue<'ctx>,
        name: &str,
    ) -> PointerValue<'ctx> {
        let entry = fn_val.get_first_basic_block().unwrap();
        let alloca_b = self.context.create_builder();
        match entry.get_first_instruction() {
            Some(first) => alloca_b.position_before(&first),
            None => alloca_b.position_at_end(entry),
        }
        alloca_b.build_alloca(self.vec_struct_type(), name).unwrap()
    }

    /// Reserve an entry-block alloca holding one String descriptor. Placed
    /// before the entry block's first instruction so it dominates every use
    /// and stays a candidate for SROA/mem2reg promotion.
    fn sso_spill_to_entry_alloca(&self, prefix: &str) -> Option<PointerValue<'ctx>> {
        // Derived from the builder's current position rather than
        // `self.current_fn`: this accessor is called from ~120 read sites,
        // some of which emit into synthesized functions where `current_fn`
        // is not the function being written. The insert block's parent is
        // always the right one by construction.
        let entry = self
            .builder
            .get_insert_block()?
            .get_parent()?
            .get_first_basic_block()?;
        let alloca_b = self.context.create_builder();
        match entry.get_first_instruction() {
            Some(first) => alloca_b.position_before(&first),
            None => alloca_b.position_at_end(entry),
        }
        alloca_b
            .build_alloca(self.vec_struct_type(), &format!("{prefix}.spill"))
            .ok()
    }
}
