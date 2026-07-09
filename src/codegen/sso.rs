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

use inkwell::values::IntValue;
use inkwell::IntPredicate;

impl<'ctx> super::Codegen<'ctx> {
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
    /// Mirrors `RuntimeKaracString::is_inline`. Not yet wired — the read
    /// sites that must branch on it (`substring`/concat construction, the
    /// tag-aware `string_data_ptr`, the string-match dispatch tree) land
    /// with inline construction in Slice 2; this is the shared primitive
    /// they will build on, kept beside `sso_string_is_owned_heap` so the
    /// two halves of the discriminant stay in one place.
    #[allow(dead_code)]
    pub(super) fn sso_string_is_inline(&self, cap: IntValue<'ctx>) -> IntValue<'ctx> {
        let zero = cap.get_type().const_zero();
        self.builder
            .build_int_compare(IntPredicate::SLT, cap, zero, "sso.inline")
            .unwrap()
    }
}
