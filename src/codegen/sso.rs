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
/// intermediate commit must be a *byte-identical* no-op: with SSO off each
/// accessor emits exactly the raw load it replaced, so the IR does not move
/// at all — not merely "the branch is never taken at runtime".
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
        fn_val: FunctionValue<'ctx>,
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
        let Some(slot) = self.sso_spill_to_entry_alloca(fn_val, sv, prefix) else {
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

    /// Reserve an entry-block alloca holding one String descriptor. Placed
    /// before the entry block's first instruction so it dominates every use
    /// and stays a candidate for SROA/mem2reg promotion.
    fn sso_spill_to_entry_alloca(
        &self,
        fn_val: FunctionValue<'ctx>,
        _sv: StructValue<'ctx>,
        prefix: &str,
    ) -> Option<PointerValue<'ctx>> {
        let entry = fn_val.get_first_basic_block()?;
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
