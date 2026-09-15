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

    /// Maximum bytes an inline descriptor can hold. Mirrors
    /// `RuntimeKaracString::INLINE_CAPACITY`, exactly as
    /// [`STRING_DESCRIPTOR_BYTES`](Self::STRING_DESCRIPTOR_BYTES) mirrors
    /// `size_of::<RuntimeKaracString>()`.
    ///
    /// **It is a mirror rather than a reference, and not by preference.**
    /// `karac-runtime` IS a dependency under the `llvm` feature, so
    /// `karac_runtime::RuntimeKaracString::INLINE_CAPACITY` compiles — and
    /// then fails to LINK, for every binary that links `libkarac`. Pulling
    /// the runtime rlib in makes the linker keep its `#[no_mangle] karac_*`
    /// surface (the link line forces it with
    /// `--export-dynamic-symbol=karac_*`), and that surface references
    /// `KARAC_SPAWN_SITES_ENABLED` — an `extern static` that CODEGEN EMITS
    /// INTO COMPILED PROGRAMS and that nothing in the compiler defines. The
    /// build then dies with `undefined symbol: KARAC_SPAWN_SITES_ENABLED` on
    /// `karac` itself and on unrelated test bins like `drop_fuzz`.
    /// `karac_jit_runner` gets away with it because a JIT'd module supplies
    /// those globals; the compiler has no such module. Measured while
    /// landing B-2026-09-15-13.
    ///
    /// So the drift has to be caught by a test instead, and it is — by
    /// `codegen_inline_encoding_contract` in `runtime/src/sso.rs`, which
    /// restates these three constants and asserts the bytes they imply
    /// against `write_inline` itself, at every length from 0 to the
    /// capacity. Change either side and that test fails.
    ///
    /// Worth recording why codegen names this number at all:
    /// `karac_string_try_inline_into` was built to keep it out, answering a
    /// VERDICT rather than taking a threshold, so a caller branched on the
    /// answer and never learned the bound. Emitting the encoding inline
    /// gives that up — a fits-test needs a constant to compare against.
    /// That was a deliberate trade, at 10x on the measured rail.
    pub(super) const STRING_INLINE_CAPACITY: u64 = 23;

    /// Byte 23 of an inline descriptor: bit 7 set, bits 0..=6 the length.
    /// The low byte of `INLINE_FLAG` (bit 63 of `cap`) on a little-endian
    /// target, which `runtime/src/sso.rs` enforces with a `compile_error!`
    /// on any other endianness. `codegen_inline_encoding_contract` in
    /// `runtime/src/sso.rs` asserts the whole encoding against the runtime's
    /// own `write_inline` rather than trusting this comment.
    const STRING_INLINE_FLAG_BYTE: u64 = 0x80;

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

    /// A POSITIVE "this receiver cannot be a `String`" signal, and the only
    /// kind of signal it is safe to gate SSO work on.
    ///
    /// **Two previous guards here were wrong in the unsafe direction, both by
    /// reading the ABSENCE of a String signal as evidence of a `Vec`.** The
    /// first read `!vec_elem_types.contains_key(v)`, but that table maps any
    /// `{ptr,len,cap}`-shaped local to its ELEMENT type and a `String` goes in
    /// it with element `i8` — so the guard skipped promotion for exactly the
    /// Strings that needed it, and the self-hosted item parser SIGSEGV'd on a
    /// `push_str` through an inline descriptor. `vec_elem_type_for_var` cannot
    /// be used either: it DEFAULTS to `i64` for an absent variable, so "not in
    /// the table" is indistinguishable from "a `Vec[i64]`".
    ///
    /// `var_elem_type_exprs` is different in kind: the registrar
    /// (`stmts.rs`) branches on the binding's type and inserts a `String` into
    /// `vec_elem_types` + `string_vars` in one arm and everything else into
    /// `var_elem_type_exprs` in the other, so a `String` never reaches this
    /// table by construction — its own comment says "a String is none of
    /// them". `control_flow.rs` already relies on the same distinction. The
    /// other four insert sites are a slice, a `.chars()` binding, a shadow
    /// restore and a match-binding copy; none introduces a `String` either.
    ///
    /// Both halves are required, belt and braces: a positive non-String signal
    /// AND the absence of the positive String signal. Anything unknown answers
    /// `false` and keeps the SSO path, because the failure mode of guessing
    /// wrong is silent memory corruption in one direction and a few wasted
    /// instructions in the other.
    ///
    /// Measured worth (2026-09-13, kata corpus): the mutating-method
    /// chokepoint alone was ~90% of the regression on the median high-effect
    /// kata, on programs containing no Strings at all.
    pub(super) fn receiver_is_definitely_not_string(&self, var_name: &str) -> bool {
        self.var_types.var_elem_type_exprs.contains_key(var_name)
            && !self.var_types.string_vars.contains(var_name)
    }

    /// Whether the tag-aware String read/construct path is switched on.
    /// See [`sso_enabled`] — off by default while the sweep lands.
    ///
    /// **Also off on wasm, unconditionally, and that is a correctness gate
    /// rather than a policy one.** The inline overlay needs the descriptor's
    /// 24 bytes to be wholly covered by its three fields, and
    /// `{ptr, i64, i64}` is gapless only at a pointer width of 8. On wasm32
    /// the `i64` at offset 8 leaves bytes 4..=7 belonging to no field, while
    /// the overlay's data bytes 0..=22 run straight through them.
    ///
    /// Writing those bytes is not the problem — `write_inline` in
    /// `runtime/src/sso.rs` does it through a raw byte pointer. KEEPING them
    /// is. A `String` moves through the compiler as an LLVM aggregate, and
    /// `load { ptr, i64, i64 }` reads three fields, not 24 bytes: the
    /// `substring` arm below has the runtime write a descriptor into
    /// `ss.result` and loads it straight back (`ss.load`), so on a padded
    /// target the hole's contents are dropped one instruction after they are
    /// written, and every later store writes three fields again. Supporting
    /// it would mean lowering every descriptor move in the compiler to a
    /// 24-byte `memcpy` — on the value type that moves most, to pessimise the
    /// 64-bit path SSO exists to speed up.
    ///
    /// So no inline descriptor is ever constructed there, no reader can meet
    /// one, and `String` on wasm behaves exactly as it did before SSO. The
    /// runtime's two construction entrypoints refuse on the same condition
    /// (`RuntimeKaracString::DESCRIPTOR_IS_GAPLESS`) independently of this
    /// gate, so a gap on either side degrades to the heap path rather than to
    /// corruption. Measured as B-2026-09-12-20: before this, every inline
    /// string of length >= 5 came back from a wasm export with a four-byte
    /// hole of zeros.
    ///
    /// `wasm_browser` / `wasm_wasi` are the only sub-64-bit entries in
    /// `V1_TARGETS`, so the target test and the pointer-width test pick out
    /// the same set today; the runtime constant is the one that states the
    /// actual precondition, and it is what a new target would be judged by.
    pub(super) fn sso_on(&self) -> bool {
        sso_enabled() && !crate::target::active_target_is_wasm()
    }

    /// Emit `String.substring`'s inline-construction fast path as IR, in
    /// place of a call to `karac_string_try_inline_into`.
    ///
    /// From the current block: branch to a fresh fast block when `n` fits
    /// inline, else to `on_too_long` (the caller's heap arm). The fast block
    /// writes the descriptor and branches to `on_inlined`.
    ///
    /// The three writes are `RuntimeKaracString::write_inline`'s three
    /// writes, in its order: the bytes, a zero-fill of the unused content
    /// bytes, and the byte-23 flag/length trailer. The zero-fill is not
    /// cosmetic — `as_bytes` reads only `byte_len()` of them, but a
    /// descriptor compared or hashed as 24 opaque bytes would otherwise see
    /// leftover stack.
    ///
    /// **Why not the call.** It is an OPTIMIZATION BARRIER, not merely call
    /// overhead: nothing in the surrounding loop can be simplified or
    /// vectorised across an opaque callee, and on the inline path there is no
    /// `malloc` for its cost to hide behind. Measured on
    /// `bench/sso/substr.kara`, 10M iterations, auto-par pinned, x86-64:
    /// **212ms through the call, 21ms through these instructions**, against a
    /// 125ms `KARAC_SSO=0` baseline — so the call cost more than the malloc it
    /// exists to avoid. Two other explanations were tested and refuted:
    /// annotating the declaration `memory(argmem: readwrite) nounwind
    /// willreturn` recovered nothing (213ms), which rules out aliasing, and
    /// folding the comparison's literal side recovered nothing (211ms), which
    /// rules out the store-to-load-forwarding shape the spike doc predicted.
    /// B-2026-09-15-13.
    ///
    /// The JIT is NOT a second path here — `karac run` goes through this same
    /// codegen — so after this change `karac_string_try_inline_into` has no
    /// caller in the compiler at all. It stays as a `#[no_mangle]` runtime
    /// export (removing ABI surface is a separate decision), but it is no
    /// longer on any hot path. Its sibling `write_inline` still owns the
    /// encoding, and `codegen_inline_encoding_contract` in
    /// `runtime/src/sso.rs` asserts these instructions against it.
    ///
    /// A negative `n` cannot arise from the validated `end - start` at the
    /// only call site, but the fits-test is UNSIGNED, so one would read as
    /// enormous and take the heap arm — the same refusal the runtime makes,
    /// for the same reason.
    pub(super) fn sso_emit_inline_construct(
        &self,
        src: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        out: PointerValue<'ctx>,
        on_inlined: inkwell::basic_block::BasicBlock<'ctx>,
        on_too_long: inkwell::basic_block::BasicBlock<'ctx>,
        prefix: &str,
    ) {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();
        let i8_t = self.context.i8_type();
        let cap = i64_t.const_int(Self::STRING_INLINE_CAPACITY, false);

        let fits = self
            .builder
            .build_int_compare(IntPredicate::ULE, n, cap, &format!("{prefix}.fits"))
            .unwrap();
        let fast_bb = self
            .context
            .append_basic_block(fn_val, &format!("{prefix}.fast"));
        self.builder
            .build_conditional_branch(fits, fast_bb, on_too_long)
            .unwrap();

        self.builder.position_at_end(fast_bb);
        self.builder.build_memcpy(out, 1, src, 1, n).unwrap();
        let tail = unsafe {
            self.builder
                .build_gep(i8_t, out, &[n], &format!("{prefix}.tailp"))
                .unwrap()
        };
        let tail_len = self
            .builder
            .build_int_nsw_sub(cap, n, &format!("{prefix}.tailn"))
            .unwrap();
        self.builder
            .build_memset(tail, 1, i8_t.const_zero(), tail_len)
            .unwrap();
        let flag_p = unsafe {
            self.builder
                .build_gep(i8_t, out, &[cap], &format!("{prefix}.flagp"))
                .unwrap()
        };
        let len_b = self
            .builder
            .build_int_truncate(n, i8_t, &format!("{prefix}.lenb"))
            .unwrap();
        let flag = self
            .builder
            .build_or(
                len_b,
                i8_t.const_int(Self::STRING_INLINE_FLAG_BYTE, false),
                &format!("{prefix}.flag"),
            )
            .unwrap();
        self.builder.build_store(flag_p, flag).unwrap();
        self.builder.build_unconditional_branch(on_inlined).unwrap();
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

#[cfg(test)]
mod tests {
    /// The flag/length trailer must be the descriptor's LAST byte: an
    /// off-by-one here writes one byte past a 24-byte descriptor. The
    /// encoding itself is cross-checked against the runtime's own
    /// `write_inline` by `codegen_inline_encoding_contract` in
    /// `runtime/src/sso.rs` — it cannot be checked from here, because
    /// `libkarac` cannot link `karac-runtime` (see
    /// `STRING_INLINE_CAPACITY`). B-2026-09-15-13.
    #[test]
    fn sso_inline_capacity_leaves_room_for_the_trailer() {
        type Cg<'a> = crate::codegen::Codegen<'a>;
        assert_eq!(
            Cg::STRING_INLINE_CAPACITY + 1,
            Cg::STRING_DESCRIPTOR_BYTES,
            "the flag/length trailer must be the descriptor's last byte",
        );
        assert_eq!(
            Cg::STRING_INLINE_FLAG_BYTE,
            0x80,
            "flag is bit 7 of byte 23"
        );
    }
}
