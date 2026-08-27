//! Synthesized per-type helper functions: hash, eq, drop, and display.
//!
//! Houses the emit_*_for_type / emit_*_for_type_expr / emit_*_for_tuple
//! family of methods that lazily synthesize per-type LLVM functions
//! for hashing, equality, dropping, and display rendering. These
//! functions are emitted on first demand and cached in the matching
//! `hash_fn_cache` / `eq_fn_cache` / `enum_drop_fns` / `struct_drop_fns`
//! / `display_fn_cache` field on `Codegen`.
//!
//! Includes the FxHash byte-loop primitive `emit_fxhash_over_bytes`
//! consumed by every `emit_hash_fn_*` site, plus the `display_mangle_te`
//! type-name mangler used to key the display cache.

use crate::ast::*;

use inkwell::module::Linkage;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{FunctionValue, IntValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

impl<'ctx> super::Codegen<'ctx> {
    // ── Map codegen ───────────────────────────────────────────────

    /// FxHash multiplier — rustc-hash style. Picked by the
    /// `bench/hash_quality/` investigation (2026-05-15) as the
    /// fastest non-cryptographic hash on karac's per-K hash bench
    /// matrix (4-8× faster than FNV-1a on common workloads;
    /// geometric mean 0.56× of FNV-1a baseline across 18 cells).
    /// Mixed via rotate-left-5 + XOR + multiply per chunk.
    const FXHASH_SEED: u64 = 0x517c_c1b7_2722_0a95;
    const FXHASH_ROTATE: u64 = 5;

    /// Emit an FxHash byte loop over `byte_count` bytes starting at
    /// `data_ptr`. Per-byte step is `h = h.rotate_left(5) ^ byte;
    /// h = h * FXHASH_SEED`. Appends basic blocks to `hash_fn_val`.
    /// Builder must be positioned just before the first block of
    /// the loop; on return it is positioned at the exit block.
    /// Returns the accumulated hash `IntValue` (i64).
    ///
    /// For fixed-size `≤8`-byte primitive keys, prefer the inline
    /// fast-path in `emit_hash_fn_for_type` (one zext + one
    /// multiply, no loop) — it produces the same hash output as
    /// this byte loop when the loop runs the same byte count from
    /// an all-zero initial accumulator, because `rotate_left(0, 5)
    /// = 0` and the loop body collapses to `h = byte * SEED` on
    /// iteration 0. Wider primitives and variable-length keys
    /// (Vec, String, Slice) fall through to this byte loop.
    pub(super) fn emit_fxhash_over_bytes(
        &mut self,
        _hash_fn_val: FunctionValue<'ctx>,
        data_ptr: PointerValue<'ctx>,
        byte_count: IntValue<'ctx>,
    ) -> IntValue<'ctx> {
        self.emit_hash_bytes_call(data_ptr, byte_count)
    }

    /// Emit `call @karac_hash_bytes(ptr, len)` — SipHash-1-3 under the process
    /// seed, from `runtime/src/hashing.rs` over the shared `karac-hash` crate
    /// the interpreter also uses (B-2026-08-21-6).
    ///
    /// WHAT THIS REPLACED, and why a call rather than inline IR. Codegen used
    /// to inline an FxHash byte loop — `h = h.rotate_left(5) ^ byte; h *=
    /// 0x517cc1b727220a95` — into every per-type `hash_fn`. The seed was a
    /// compile-time CONSTANT sitting in the compiler's own source, so colliding
    /// keys could be generated offline and used to drive any `Map[String, _]`
    /// keyed on request data quadratic. design.md § `Hash` and `Hasher` names
    /// that exact threat and mandates a per-process-seeded DoS-resistant hash.
    ///
    /// Emitting a CALL keeps the permutation in ONE place shared with the
    /// interpreter rather than hand-written a second time in IR — the rule the
    /// Arrow IPC twin and `String.normalize` already follow. It costs one
    /// direct call per hash; the `karac_map_*` table was already calling its
    /// `hash_fn` through a function POINTER, so this adds a call frame, not a
    /// new indirection class.
    ///
    /// A USER hasher (B-2026-08-22-6) takes the other arm: it has no runtime
    /// entry point, because its permutation is user code. See
    /// [`Self::emit_user_hash_call`].
    pub(super) fn emit_hash_bytes_call(
        &mut self,
        data_ptr: PointerValue<'ctx>,
        byte_count: IntValue<'ctx>,
    ) -> IntValue<'ctx> {
        let Some(sym) = self.hash_hasher.runtime_symbol() else {
            return self.emit_user_hash_call(data_ptr, byte_count);
        };
        let f = self
            .module
            .get_function(sym)
            .expect("karac_hash_bytes / _fx externs declared in Codegen::new");
        self.builder
            .build_call(f, &[data_ptr.into(), byte_count.into()], "hash.bytes")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value()
    }

    /// The per-key-type EQUALITY function for a type carrying a hand-written
    /// `impl PartialEq` plus the `Eq` marker, or `None` (B-2026-08-26-10).
    ///
    /// The twin of [`Self::try_emit_user_impl_hash_fn`], and they must ship
    /// together: hashing a key through the user's impl while comparing it
    /// structurally places the key by one rule and looks it up by another, which
    /// loses entries rather than merely reordering them.
    ///
    /// Simpler than the hash side because `eq` is NOT generic — there is a
    /// single `T.eq` symbol to call, with no monomorph to request and so no
    /// synthesized wrapper. Requires the `Eq` marker as well as `PartialEq`,
    /// which is already what the `==` operator itself requires.
    fn try_emit_user_impl_eq_fn(
        &mut self,
        type_name: &str,
        fn_name: &str,
    ) -> Option<FunctionValue<'ctx>> {
        if !self.user_eq_impl_types.contains(type_name) {
            return None;
        }
        let user_eq = self.module.get_function(&format!("{type_name}.eq"))?;

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_t = self.context.i8_type();
        let saved_bb = self.builder.get_insert_block();

        let eq_fn = self.module.add_function(
            fn_name,
            i8_t.fn_type(&[ptr_ty.into(), ptr_ty.into()], false),
            Some(Linkage::Internal),
        );
        let entry_bb = self.context.append_basic_block(eq_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let a = eq_fn.get_nth_param(0).unwrap().into_pointer_value();
        let b = eq_fn.get_nth_param(1).unwrap().into_pointer_value();

        let saved_fn = self.current_fn.replace(eq_fn);
        let called = self
            .builder
            .build_call(user_eq, &[a.into(), b.into()], "user.eq")
            .unwrap()
            .try_as_basic_value()
            .basic();
        // `eq` returns `bool`, which lowers to `i1` or `i8` depending on the
        // signature; the container's comparator slot is `i8`, so normalize
        // rather than assume.
        let out = match called {
            Some(v) => {
                let iv = v.into_int_value();
                if iv.get_type().get_bit_width() == 8 {
                    iv
                } else {
                    self.builder
                        .build_int_z_extend(iv, i8_t, "user.eq.i8")
                        .unwrap()
                }
            }
            None => i8_t.const_zero(),
        };
        self.builder.build_return(Some(&out)).unwrap();
        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        Some(eq_fn)
    }

    /// The whole per-key-type hash function for a type carrying a user
    /// `impl Hash`, or `None` when it carries none (B-2026-08-26-10).
    ///
    /// A user `impl Hash` decides WHICH BYTES the key contributes; every other
    /// arm derives them structurally from the key's memory image. `desugar.rs`
    /// synthesized `karac_hash_bytes_of_<T>` to run the impl against a
    /// `KeyByteSink` and hand back the collected `Vec[u8]`, so all this does is
    /// feed those bytes to the container's hasher.
    ///
    /// Routing through `emit_hash_bytes_call` rather than computing a digest
    /// here is what keeps a user `impl Hash` COMPOSING with a user
    /// `BuildHasher` instead of overriding it — the two answer different
    /// questions, and design.md § `Hash` and `Hasher` keeps them apart on
    /// purpose.
    fn try_emit_user_impl_hash_fn(
        &mut self,
        type_name: &str,
        fn_name: &str,
    ) -> Option<FunctionValue<'ctx>> {
        let bytes_fn = self
            .module
            .get_function(&format!("karac_hash_bytes_of_{type_name}"))?;

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let saved_bb = self.builder.get_insert_block();

        let hash_fn = self.module.add_function(
            fn_name,
            i64_t.fn_type(&[ptr_ty.into()], false),
            Some(Linkage::Internal),
        );
        let entry_bb = self.context.append_basic_block(hash_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let key_ptr = hash_fn.get_nth_param(0).unwrap().into_pointer_value();

        // `current_fn` has to point at the function being EMITTED, not at
        // whatever was being compiled when the map asked for a hash function.
        // The buffer free appends basic blocks to `current_fn`, so leaving the
        // outer one installed put this body's branch targets in another
        // function — "Referring to a basic block in another function!" from the
        // module verifier, with the return landing in a block that was not this
        // function's either.
        let saved_fn = self.current_fn.replace(hash_fn);
        let digest = self.emit_user_impl_hash_call(bytes_fn, key_ptr);
        match digest {
            Some(h) => self.builder.build_return(Some(&h)).unwrap(),
            None => self
                .builder
                .build_return(Some(&i64_t.const_zero()))
                .unwrap(),
        };
        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        Some(hash_fn)
    }

    /// `karac_hash_bytes_of_<T>(key)` → hash the returned `Vec[u8]` → free it.
    ///
    /// The wrapper returns an OWNED `Vec[u8]` freshly built per call, so its
    /// buffer is this function's to release. Freeing it matters more than it
    /// looks: this runs once per key hash, which is the map's hot path, so a
    /// leak here would be per-insert and per-lookup rather than per-program.
    ///
    /// `None` when the call produces no value, which cannot happen for a
    /// `-> Vec[u8]` signature but keeps the caller on its structural path
    /// instead of emitting a broken function if it ever does.
    fn emit_user_impl_hash_call(
        &mut self,
        bytes_fn: FunctionValue<'ctx>,
        key_ptr: PointerValue<'ctx>,
    ) -> Option<IntValue<'ctx>> {
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let ret = self
            .builder
            .build_call(bytes_fn, &[key_ptr.into()], "hash.bytes.of")
            .unwrap()
            .try_as_basic_value()
            .basic()?;
        let slot = self.builder.build_alloca(vec_ty, "hash.bytes.vec").unwrap();
        self.builder.build_store(slot, ret).unwrap();

        let data_pp = self
            .builder
            .build_struct_gep(vec_ty, slot, 0, "hash.bytes.data.pp")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "hash.bytes.data")
            .unwrap()
            .into_pointer_value();
        let len_pp = self
            .builder
            .build_struct_gep(vec_ty, slot, 1, "hash.bytes.len.pp")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_t, len_pp, "hash.bytes.len")
            .unwrap()
            .into_int_value();

        let digest = self.emit_hash_bytes_call(data, len);
        // AFTER the digest is computed — the hash reads the buffer.
        self.emit_free_vec_buffer_if_owned(slot, 1);
        Some(digest)
    }

    /// Emit `B.build()` → `S.write(bytes)` → `S.finish()` for a user hasher
    /// (design.md § `Hash` and `Hasher`, "User-extensible hashers";
    /// B-2026-08-22-6).
    ///
    /// Three direct calls into functions the impl-block pass already declared
    /// under their `Type.method` symbols, wrapped around one stack slot for the
    /// per-hash state and one for the `Slice[u8]` header. The map's calling
    /// convention is untouched: `karac_map_*` still reaches this through the
    /// `hash_fn` POINTER in its control block, exactly as it reaches the
    /// builtin arm — the difference is entirely inside the synthesized
    /// `hash_fn`'s body.
    ///
    /// A fresh state PER HASH is what `BuildHasher` means: `build()` is the
    /// per-hash constructor and the builder is the per-table configuration. It
    /// is also what makes the result a function of the key alone, which the
    /// index depends on.
    ///
    /// Emits `0` if any of the three symbols is missing. That is unreachable
    /// through a checked program — `check_recorded_container_hasher` rejects a
    /// builder with no `BuildHasher` impl, and the trait's own conformance
    /// check requires the methods — and `0` degrades to "every key in one
    /// bucket", where `==` still decides, rather than to a miscompile.
    fn emit_user_hash_call(
        &mut self,
        data_ptr: PointerValue<'ctx>,
        byte_count: IntValue<'ctx>,
    ) -> IntValue<'ctx> {
        let i64_t = self.context.i64_type();
        let Some(builder) = self.hash_hasher.user_builder().map(str::to_string) else {
            return i64_t.const_zero();
        };
        let Some(state) = self.user_hasher_states.get(&builder).cloned() else {
            return i64_t.const_zero();
        };
        let (Some(build_fn), Some(write_fn), Some(finish_fn)) = (
            self.module.get_function(&format!("{builder}.build")),
            self.module.get_function(&format!("{state}.write")),
            self.module.get_function(&format!("{state}.finish")),
        ) else {
            return i64_t.const_zero();
        };

        // `build(ref self)` — the builder is a zero-field struct (the
        // typechecker rejects any other shape, because a container names a TYPE
        // and so has no value to carry fields), which is what makes an
        // uninitialized slot a complete value of it: the callee reads nothing
        // out of it. The slot still gets the builder's own LLVM type where one
        // is registered, so the IR says what it means.
        let builder_ty: BasicTypeEnum<'ctx> = self
            .type_decls
            .struct_types
            .get(builder.as_str())
            .map(|t| (*t).into())
            .unwrap_or_else(|| self.context.i8_type().into());
        let builder_slot = self
            .builder
            .build_alloca(builder_ty, "hasher.builder")
            .unwrap();
        let built = self
            .builder
            .build_call(build_fn, &[builder_slot.into()], "hasher.built")
            .unwrap()
            .try_as_basic_value()
            .basic();
        let Some(built) = built else {
            return i64_t.const_zero();
        };
        let built_ty: BasicTypeEnum<'ctx> = built.get_type();
        let state_slot = self.builder.build_alloca(built_ty, "hasher").unwrap();
        self.builder.build_store(state_slot, built).unwrap();

        // `write(mut ref self, bytes: ref Slice[u8])` — the slice header is
        // `{ ptr data, i64 len }` (`slice_struct_type`). Whether the parameter
        // arrives as that struct by value or as a pointer to it is a property
        // of how the signature lowered, so read it off the declared type rather
        // than assuming: getting it wrong is a verifier failure, not a bug that
        // waits for a particular key type.
        let slice_ty = self.slice_struct_type();
        let slice_slot = self.builder.build_alloca(slice_ty, "hasher.bytes").unwrap();
        let data_gep = self
            .builder
            .build_struct_gep(slice_ty, slice_slot, 0, "hasher.bytes.ptr")
            .unwrap();
        self.builder.build_store(data_gep, data_ptr).unwrap();
        let len_gep = self
            .builder
            .build_struct_gep(slice_ty, slice_slot, 1, "hasher.bytes.len")
            .unwrap();
        self.builder.build_store(len_gep, byte_count).unwrap();
        let slice_arg: inkwell::values::BasicMetadataValueEnum<'ctx> =
            match write_fn.get_type().get_param_types().get(1) {
                Some(inkwell::types::BasicMetadataTypeEnum::StructType(st)) => self
                    .builder
                    .build_load(*st, slice_slot, "hasher.bytes.val")
                    .unwrap()
                    .into(),
                _ => slice_slot.into(),
            };
        self.builder
            .build_call(write_fn, &[state_slot.into(), slice_arg], "")
            .unwrap();

        // `finish(ref self) -> u64`.
        self.builder
            .build_call(finish_fn, &[state_slot.into()], "hasher.finish")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .map(|v: inkwell::values::BasicValueEnum<'ctx>| v.into_int_value())
            .unwrap_or_else(|| i64_t.const_zero())
    }

    /// Record every type with BOTH `impl PartialEq` and the `Eq` marker
    /// (B-2026-08-26-10). Both, because that is the `==` operator's own rule —
    /// a bare `impl PartialEq` does not drive `==` either.
    pub(super) fn collect_user_eq_impl_types(&mut self, program: &Program) {
        let mut partial = std::collections::HashSet::new();
        let mut marker = std::collections::HashSet::new();
        for item in &program.items {
            let Item::ImplBlock(imp) = item else { continue };
            let Some(trait_name) = imp.trait_name.as_ref().and_then(|t| t.segments.last()) else {
                continue;
            };
            let Some(target) = super::helpers::impl_target_name(&imp.target_type) else {
                continue;
            };
            match trait_name.as_str() {
                "PartialEq" => {
                    partial.insert(target);
                }
                "Eq" => {
                    marker.insert(target);
                }
                _ => {}
            }
        }
        self.user_eq_impl_types = partial.intersection(&marker).cloned().collect();
    }

    /// Read every `impl BuildHasher for B { type Hasher = S }` out of the
    /// program into [`Codegen::user_hasher_states`] (B-2026-08-22-6).
    ///
    /// Keyed on the LAST path segment of both sides, which is the same key the
    /// `Type.method` symbols and the parser's `HasherKind::User` recording use,
    /// so all three agree on what names a hasher.
    pub(crate) fn collect_user_hasher_states(&mut self, program: &Program) {
        for item in &program.items {
            let Item::ImplBlock(imp) = item else { continue };
            let Some(tn) = &imp.trait_name else { continue };
            if tn.segments.last().map(String::as_str) != Some("BuildHasher") {
                continue;
            }
            let TypeKind::Path(target) = &imp.target_type.kind else {
                continue;
            };
            let Some(builder) = target.segments.last() else {
                continue;
            };
            for it in &imp.items {
                let ImplItem::AssocType(b) = it else { continue };
                if b.name != "Hasher" {
                    continue;
                }
                if let TypeKind::Path(p) = &b.ty.kind {
                    if let Some(state) = p.segments.last() {
                        self.user_hasher_states
                            .insert(builder.clone(), state.clone());
                    }
                }
            }
        }
    }

    /// Emit (or reuse) a module-level `karac_hash_{type_name}(ptr) -> i64` function.
    ///
    /// Per the `bench/hash_quality/` investigation (2026-05-15),
    /// karac's per-K hash is **FxHash** (rustc-hash style
    /// rotate-xor-multiply over 8-byte chunks). Geometric mean
    /// across 18 bench cells: 0.56× of the prior FNV-1a baseline
    /// (1.8× faster overall, up to 4-8× faster on integer keys).
    ///
    /// - **Integer primitives `≤8` bytes** (i8, i16, i32, i64,
    ///   char, bool): inline fast path — load value, zero-extend
    ///   to i64, multiply by `FXHASH_SEED`. One zext + one mul,
    ///   no loop. The initial accumulator is 0, so the per-byte
    ///   shape `h.rotate_left(5) ^ byte; h * SEED` collapses to
    ///   `value * SEED` when processed as a single chunk.
    /// - **`String`**: loads `{ ptr data, i64 len }` from the
    ///   struct and runs the FxHash byte loop over `data[0..len]`.
    /// - **Float primitives** (f32, f64) and **wider integers**
    ///   (i128, u128): byte loop over `sizeof(K)` raw bytes.
    /// - **Structs / other**: byte loop over raw struct bytes
    ///   (correct for value-only structs; tuple combiner in
    ///   `emit_hash_fn_for_tuple` per-field-recurses).
    pub(super) fn emit_hash_fn_for_type(
        &mut self,
        type_name: &str,
        key_ty: BasicTypeEnum<'ctx>,
    ) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_hash_{type_name}{}", self.hash_hasher.mangle_suffix());
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        if let Some(f) = self.try_emit_user_impl_hash_fn(type_name, &fn_name) {
            return f;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let saved_bb = self.builder.get_insert_block();

        let hash_fn_ty = i64_t.fn_type(&[ptr_ty.into()], false);
        let hash_fn = self
            .module
            .add_function(&fn_name, hash_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(hash_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let key_ptr = hash_fn.get_nth_param(0).unwrap().into_pointer_value();

        if type_name == "String" || type_name == "str" {
            // String struct: { ptr data, i64 len, i64 cap }
            let str_ty = self.vec_struct_type();
            let data_pp = self
                .builder
                .build_struct_gep(str_ty, key_ptr, 0, "s.data.pp")
                .unwrap();
            let data_ptr = self
                .builder
                .build_load(ptr_ty, data_pp, "s.data")
                .unwrap()
                .into_pointer_value();
            let len_p = self
                .builder
                .build_struct_gep(str_ty, key_ptr, 1, "s.len.p")
                .unwrap();
            let len = self
                .builder
                .build_load(i64_t, len_p, "s.len")
                .unwrap()
                .into_int_value();
            let hash = self.emit_fxhash_over_bytes(hash_fn, data_ptr, len);
            self.builder.build_return(Some(&hash)).unwrap();
        } else if let BasicTypeEnum::IntType(int_ty) = key_ty {
            // Integer primitive fast path: load value, zext to
            // i64, multiply by FXHASH_SEED. Matches the byte-loop
            // output for the i==0 case from an all-zero
            // accumulator (rotate(0, 5) = 0 → 0 ^ value = value;
            // value * SEED).
            let bit_width = int_ty.get_bit_width();
            if bit_width <= 64 {
                let raw = self
                    .builder
                    .build_load(int_ty, key_ptr, "fx.prim.raw")
                    .unwrap()
                    .into_int_value();
                // The loaded value is unused now that the hash reads the
                // key's BYTES in place rather than its numeric value.
                let _ = raw;
                // Exactly the integer's own bytes: no store, no widening. Two
                // keys of one monomorphic K always present the same byte
                // count, and a width mixed into the digest would be
                // meaningless across maps that can never share a key type.
                let nbytes = i64_t.const_int(u64::from(bit_width).div_ceil(8), false);
                let hash = self.emit_hash_bytes_call(key_ptr, nbytes);
                self.builder.build_return(Some(&hash)).unwrap();
            } else {
                // Wider integers (i128 / u128): fall back to byte loop.
                let raw_size = key_ty
                    .size_of()
                    .unwrap_or_else(|| i64_t.const_int(8, false));
                let size64 = if raw_size.get_type().get_bit_width() == 64 {
                    raw_size
                } else {
                    self.builder
                        .build_int_z_extend(raw_size, i64_t, "ksz64")
                        .unwrap()
                };
                let hash = self.emit_fxhash_over_bytes(hash_fn, key_ptr, size64);
                self.builder.build_return(Some(&hash)).unwrap();
            }
        } else {
            // Float primitives, structs, other compound types:
            // FxHash byte loop over `sizeof(K)` raw bytes.
            let raw_size = key_ty
                .size_of()
                .unwrap_or_else(|| i64_t.const_int(8, false));
            let size64 = if raw_size.get_type().get_bit_width() == 64 {
                raw_size
            } else {
                self.builder
                    .build_int_z_extend(raw_size, i64_t, "ksz64")
                    .unwrap()
            };
            let hash = self.emit_fxhash_over_bytes(hash_fn, key_ptr, size64);
            self.builder.build_return(Some(&hash)).unwrap();
        }

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        hash_fn
    }

    /// Emit (or reuse) a module-level `karac_eq_{type_name}(ptr, ptr) -> i1` function.
    ///
    /// - Integer primitives: load both values and `icmp eq`.
    /// - `String`: compare lengths then byte-by-byte.
    /// - Structs/other: byte-by-byte over raw `sizeof(K)` bytes.
    pub(super) fn emit_eq_fn_for_type(
        &mut self,
        type_name: &str,
        key_ty: BasicTypeEnum<'ctx>,
    ) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_eq_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        if let Some(f) = self.try_emit_user_impl_eq_fn(type_name, &fn_name) {
            return f;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let i8_t = self.context.i8_type();
        let bool_t = self.context.bool_type();

        let saved_bb = self.builder.get_insert_block();

        let eq_fn_ty = bool_t.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let eq_fn = self
            .module
            .add_function(&fn_name, eq_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(eq_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let a_ptr = eq_fn.get_nth_param(0).unwrap().into_pointer_value();
        let b_ptr = eq_fn.get_nth_param(1).unwrap().into_pointer_value();

        if type_name == "String" || type_name == "str" {
            // String: compare lengths first, then byte-by-byte on content.
            let str_ty = self.vec_struct_type();
            let la_p = self
                .builder
                .build_struct_gep(str_ty, a_ptr, 1, "la.p")
                .unwrap();
            let lb_p = self
                .builder
                .build_struct_gep(str_ty, b_ptr, 1, "lb.p")
                .unwrap();
            let len_a = self
                .builder
                .build_load(i64_t, la_p, "la")
                .unwrap()
                .into_int_value();
            let len_b = self
                .builder
                .build_load(i64_t, lb_p, "lb")
                .unwrap()
                .into_int_value();

            let neq_bb = self.context.append_basic_block(eq_fn, "neq");
            let bytes_bb = self.context.append_basic_block(eq_fn, "bytes");

            let len_eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, len_a, len_b, "len.eq")
                .unwrap();
            self.builder
                .build_conditional_branch(len_eq, bytes_bb, neq_bb)
                .unwrap();

            // neq_bb: return false
            self.builder.position_at_end(neq_bb);
            self.builder
                .build_return(Some(&bool_t.const_int(0, false)))
                .unwrap();

            // bytes_bb: load data ptrs, enter byte loop
            self.builder.position_at_end(bytes_bb);
            let da_p = self
                .builder
                .build_struct_gep(str_ty, a_ptr, 0, "da.p")
                .unwrap();
            let db_p = self
                .builder
                .build_struct_gep(str_ty, b_ptr, 0, "db.p")
                .unwrap();
            let data_a = self
                .builder
                .build_load(ptr_ty, da_p, "da")
                .unwrap()
                .into_pointer_value();
            let data_b = self
                .builder
                .build_load(ptr_ty, db_p, "db")
                .unwrap()
                .into_pointer_value();

            let loop_hdr = self.context.append_basic_block(eq_fn, "eq.hdr");
            let loop_bdy = self.context.append_basic_block(eq_fn, "eq.bdy");
            let loop_exit = self.context.append_basic_block(eq_fn, "eq.exit");

            self.builder.build_unconditional_branch(loop_hdr).unwrap();

            self.builder.position_at_end(loop_hdr);
            let i_phi = self.builder.build_phi(i64_t, "eq.i").unwrap();
            i_phi.add_incoming(&[(&i64_t.const_zero(), bytes_bb)]);
            let i_val = i_phi.as_basic_value().into_int_value();
            let cond = self
                .builder
                .build_int_compare(IntPredicate::ULT, i_val, len_a, "eq.cond")
                .unwrap();
            self.builder
                .build_conditional_branch(cond, loop_bdy, loop_exit)
                .unwrap();

            self.builder.position_at_end(loop_bdy);
            let bpa = unsafe {
                self.builder
                    .build_gep(i8_t, data_a, &[i_val], "bpa")
                    .unwrap()
            };
            let bpb = unsafe {
                self.builder
                    .build_gep(i8_t, data_b, &[i_val], "bpb")
                    .unwrap()
            };
            let ba = self
                .builder
                .build_load(i8_t, bpa, "ba")
                .unwrap()
                .into_int_value();
            let bb_v = self
                .builder
                .build_load(i8_t, bpb, "bb")
                .unwrap()
                .into_int_value();
            let bytes_eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, ba, bb_v, "beq")
                .unwrap();
            let i_next = self
                .builder
                .build_int_add(i_val, i64_t.const_int(1, false), "eq.i1")
                .unwrap();
            i_phi.add_incoming(&[(&i_next, loop_bdy)]);
            self.builder
                .build_conditional_branch(bytes_eq, loop_hdr, neq_bb)
                .unwrap();

            self.builder.position_at_end(loop_exit);
            self.builder
                .build_return(Some(&bool_t.const_int(1, false)))
                .unwrap();
        } else if let BasicTypeEnum::IntType(int_ty) = key_ty {
            // Integer primitives: load and compare directly.
            let va = self
                .builder
                .build_load(int_ty, a_ptr, "va")
                .unwrap()
                .into_int_value();
            let vb = self
                .builder
                .build_load(int_ty, b_ptr, "vb")
                .unwrap()
                .into_int_value();
            let eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, va, vb, "eq")
                .unwrap();
            self.builder.build_return(Some(&eq)).unwrap();
        } else {
            // Structs and other fixed-size types: byte-by-byte comparison.
            let raw_size = key_ty
                .size_of()
                .unwrap_or_else(|| i64_t.const_int(8, false));
            let size64 = if raw_size.get_type().get_bit_width() == 64 {
                raw_size
            } else {
                self.builder
                    .build_int_z_extend(raw_size, i64_t, "ksz64")
                    .unwrap()
            };

            let neq_bb = self.context.append_basic_block(eq_fn, "neq");
            let loop_hdr = self.context.append_basic_block(eq_fn, "eq.hdr");
            let loop_bdy = self.context.append_basic_block(eq_fn, "eq.bdy");
            let loop_exit = self.context.append_basic_block(eq_fn, "eq.exit");

            self.builder.build_unconditional_branch(loop_hdr).unwrap();

            self.builder.position_at_end(neq_bb);
            self.builder
                .build_return(Some(&bool_t.const_int(0, false)))
                .unwrap();

            self.builder.position_at_end(loop_hdr);
            let i_phi = self.builder.build_phi(i64_t, "eq.i").unwrap();
            i_phi.add_incoming(&[(&i64_t.const_zero(), entry_bb)]);
            let i_val = i_phi.as_basic_value().into_int_value();
            let cond = self
                .builder
                .build_int_compare(IntPredicate::ULT, i_val, size64, "eq.cond")
                .unwrap();
            self.builder
                .build_conditional_branch(cond, loop_bdy, loop_exit)
                .unwrap();

            self.builder.position_at_end(loop_bdy);
            let bpa = unsafe {
                self.builder
                    .build_gep(i8_t, a_ptr, &[i_val], "bpa")
                    .unwrap()
            };
            let bpb = unsafe {
                self.builder
                    .build_gep(i8_t, b_ptr, &[i_val], "bpb")
                    .unwrap()
            };
            let ba = self
                .builder
                .build_load(i8_t, bpa, "ba")
                .unwrap()
                .into_int_value();
            let bb_v = self
                .builder
                .build_load(i8_t, bpb, "bb")
                .unwrap()
                .into_int_value();
            let bytes_eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, ba, bb_v, "beq")
                .unwrap();
            let i_next = self
                .builder
                .build_int_add(i_val, i64_t.const_int(1, false), "eq.i1")
                .unwrap();
            i_phi.add_incoming(&[(&i_next, loop_bdy)]);
            self.builder
                .build_conditional_branch(bytes_eq, loop_hdr, neq_bb)
                .unwrap();

            self.builder.position_at_end(loop_exit);
            self.builder
                .build_return(Some(&bool_t.const_int(1, false)))
                .unwrap();
        }

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        eq_fn
    }

    pub(super) fn emit_hash_fn_for_type_expr(&mut self, te: &TypeExpr) -> FunctionValue<'ctx> {
        let type_name = Self::mangled_type_name(te);
        let fn_name = format!("karac_hash_{type_name}{}", self.hash_hasher.mangle_suffix());
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        // BEFORE the per-shape match: a struct key takes the
        // `emit_hash_fn_for_struct` arm below and would never reach
        // `emit_hash_fn_for_type`, so checking only there left the user's impl
        // unused for the one shape it is written for (B-2026-08-26-10).
        if let Some(f) = self.try_emit_user_impl_hash_fn(&type_name, &fn_name) {
            return f;
        }
        match &te.kind {
            TypeKind::Tuple(elems) if !elems.is_empty() => {
                self.emit_hash_fn_for_tuple(&type_name, elems)
            }
            // `Vec[T]` element/key: CONTENT hash that walks the elements.
            // The `_ =>` byte-loop fallback hashes the `{ptr,len,cap}` HEADER
            // (pointer identity), so two equal-contents vecs hash unequally
            // and `Set[Vec[T]]` / `Map[Vec[T], _]` never dedupe by value
            // (B-2026-06-20-15). Keyed on a richer name (`karac_hash_Vec_<elem>`)
            // than the shallow `mangled_type_name` "Vec", so distinct element
            // types don't share one body.
            TypeKind::Path(p) if p.segments.len() == 1 && p.segments[0] == "Vec" => {
                match p.generic_args.as_ref().and_then(|a| a.first()) {
                    Some(GenericArg::Type(elem_te)) => {
                        let elem_te = elem_te.clone();
                        self.emit_hash_fn_for_vec(&elem_te)
                    }
                    // No element TypeExpr recorded — header byte-loop fallback.
                    _ => {
                        let key_ty = self.llvm_type_for_type_expr(te);
                        self.emit_hash_fn_for_type(&type_name, key_ty)
                    }
                }
            }
            // User-struct path: dispatch to per-field hash (mirrors the
            // tuple shape) when the path resolves to a registered
            // struct. The byte-loop fallback in `emit_hash_fn_for_type`
            // hashes raw struct bytes — which includes ptr fields of
            // any `String` / `Vec` / `Map` field — so two structurally-
            // equal instances with different inner allocations hash
            // unequally. AOT used to mask this via the post-codegen
            // `ConstantMerge` pass folding identical string-literal
            // globals into one (so all `"alice"` Tags happened to
            // share a data pointer); LLJIT runs the pre-O2 IR and gets
            // bitten. See `wip-always-jit.md` W3.5 bug 4.
            TypeKind::Path(p)
                if p.segments.len() == 1
                    && self
                        .type_decls
                        .struct_field_type_exprs
                        .contains_key(&p.segments[0])
                    && !self.type_decls.shared_types.contains_key(&p.segments[0]) =>
            {
                let struct_name = p.segments[0].clone();
                self.emit_hash_fn_for_struct(&struct_name)
            }
            // B-2026-08-27-4 — the eq twin's sibling; the two must move
            // together or equal keys land in different buckets.
            TypeKind::Path(p)
                if p.segments.len() == 1
                    && self.type_decls.shared_types.contains_key(&p.segments[0]) =>
            {
                let shared_name = p.segments[0].clone();
                self.emit_hash_fn_for_shared(&shared_name)
            }
            _ => {
                let key_ty = self.llvm_type_for_type_expr(te);
                self.emit_hash_fn_for_type(&type_name, key_ty)
            }
        }
    }

    /// TypeExpr-aware eq-fn wrapper. Mirror of `emit_hash_fn_for_type_expr`.
    pub(super) fn emit_eq_fn_for_type_expr(&mut self, te: &TypeExpr) -> FunctionValue<'ctx> {
        let type_name = Self::mangled_type_name(te);
        let fn_name = format!("karac_eq_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        // Before the per-shape match, for the reason the hash twin documents: a
        // struct key takes the `emit_eq_fn_for_struct` arm below and would never
        // reach `emit_eq_fn_for_type`.
        if let Some(f) = self.try_emit_user_impl_eq_fn(&type_name, &fn_name) {
            return f;
        }
        match &te.kind {
            TypeKind::Tuple(elems) if !elems.is_empty() => {
                self.emit_eq_fn_for_tuple(&type_name, elems)
            }
            // `Vec[T]` element/key: CONTENT equality (length, then element-wise).
            // The `_ =>` byte-loop fallback compares the `{ptr,len,cap}` HEADER
            // (pointer identity), so two equal-contents vecs compare unequal —
            // the `Set[Vec[T]]` dedup bug (B-2026-06-20-15). See
            // `emit_hash_fn_for_type_expr`'s sibling Vec arm.
            TypeKind::Path(p) if p.segments.len() == 1 && p.segments[0] == "Vec" => {
                match p.generic_args.as_ref().and_then(|a| a.first()) {
                    Some(GenericArg::Type(elem_te)) => {
                        let elem_te = elem_te.clone();
                        self.emit_eq_fn_for_vec(&elem_te)
                    }
                    _ => {
                        let key_ty = self.llvm_type_for_type_expr(te);
                        self.emit_eq_fn_for_type(&type_name, key_ty)
                    }
                }
            }
            TypeKind::Path(p)
                if p.segments.len() == 1
                    && self
                        .type_decls
                        .struct_field_type_exprs
                        .contains_key(&p.segments[0])
                    && !self.type_decls.shared_types.contains_key(&p.segments[0]) =>
            {
                let struct_name = p.segments[0].clone();
                self.emit_eq_fn_for_struct(&struct_name)
            }
            // B-2026-08-27-4 — a `shared struct` / `shared enum` KEY. Without
            // this arm it fell to the byte-compare fallback below, which for a
            // shared key compares the POINTER.
            TypeKind::Path(p)
                if p.segments.len() == 1
                    && self.type_decls.shared_types.contains_key(&p.segments[0]) =>
            {
                let shared_name = p.segments[0].clone();
                self.emit_eq_fn_for_shared(&shared_name)
            }
            _ => {
                let key_ty = self.llvm_type_for_type_expr(te);
                self.emit_eq_fn_for_type(&type_name, key_ty)
            }
        }
    }

    /// Emit (or reuse) `karac_hash_Vec_<elem>(*const Vec) -> i64` — a
    /// CONTENT hash for a `Vec[T]` element/key. Walks `0..len` calling the
    /// per-element hash fn (through the type-expr dispatcher, so a
    /// `Vec[String]` / `Vec[Vec[i64]]` element recurses correctly) and folds
    /// each element hash into the FxHash tail-mix `state = state.rotate_left(5)
    /// ^ x; state *= SEED`, seeded with `len` so length is part of the digest
    /// (matching Rust's `Hash for [T]`) and equal-content vecs hash equal.
    ///
    /// The byte-loop fallback in `emit_hash_fn_for_type` would hash the
    /// `{ptr,len,cap}` HEADER (pointer identity), so two equal-contents vecs
    /// land in different buckets and `Set[Vec[T]]` never dedupes — the
    /// `B-2026-06-20-15` bug. Mirrors `emit_vec_clone_fn`'s struct-GEP +
    /// per-element-loop shape; the eq sibling is `emit_eq_fn_for_vec`.
    pub(super) fn emit_hash_fn_for_vec(&mut self, elem_te: &TypeExpr) -> FunctionValue<'ctx> {
        let elem_name = Self::display_mangle_te(elem_te);
        let fn_name = format!(
            "karac_hash_Vec_{elem_name}{}",
            self.hash_hasher.mangle_suffix()
        );
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let vec_ty = self.vec_struct_type();
        let elem_ty = self.llvm_type_for_type_expr(elem_te);
        // Recurse first — emit may switch the builder's insert block.
        let elem_hash = self.emit_hash_fn_for_type_expr(elem_te);

        let saved_bb = self.builder.get_insert_block();
        let hash_fn_ty = i64_t.fn_type(&[ptr_ty.into()], false);
        let hash_fn = self
            .module
            .add_function(&fn_name, hash_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(hash_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let key_ptr = hash_fn.get_nth_param(0).unwrap().into_pointer_value();

        // Load src.{data, len} from the {ptr,len,cap} header.
        let data_pp = self
            .builder
            .build_struct_gep(vec_ty, key_ptr, 0, "v.data.pp")
            .unwrap();
        let data_ptr = self
            .builder
            .build_load(ptr_ty, data_pp, "v.data")
            .unwrap()
            .into_pointer_value();
        let len_p = self
            .builder
            .build_struct_gep(vec_ty, key_ptr, 1, "v.len.p")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_t, len_p, "v.len")
            .unwrap()
            .into_int_value();

        // Element stride in bytes.
        let raw_size = elem_ty
            .size_of()
            .unwrap_or_else(|| i64_t.const_int(8, false));
        let elem_size = if raw_size.get_type().get_bit_width() == 64 {
            raw_size
        } else {
            self.builder
                .build_int_z_extend(raw_size, i64_t, "esz64")
                .unwrap()
        };

        let seed = i64_t.const_int(Self::FXHASH_SEED, false);
        let rotate_amt = i64_t.const_int(Self::FXHASH_ROTATE, false);
        let rotate_inv = i64_t.const_int(64 - Self::FXHASH_ROTATE, false);

        // Seed state with len: rotate_left(0, 5) = 0, so mix(0, len) collapses
        // to `len * SEED` (same shape the inline primitive fast path uses).
        let init_state = self.builder.build_int_mul(len, seed, "v.h.init").unwrap();

        // Loop i in 0..len: state = mix(state, elem_hash(data + i*size)).
        let pre_bb = self.builder.get_insert_block().unwrap();
        let hdr_bb = self.context.append_basic_block(hash_fn, "v.h.hdr");
        let bdy_bb = self.context.append_basic_block(hash_fn, "v.h.bdy");
        let exit_bb = self.context.append_basic_block(hash_fn, "v.h.exit");
        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        self.builder.position_at_end(hdr_bb);
        let i_phi = self.builder.build_phi(i64_t, "v.h.i").unwrap();
        let state_phi = self.builder.build_phi(i64_t, "v.h.state").unwrap();
        i_phi.add_incoming(&[(&i64_t.const_zero(), pre_bb)]);
        state_phi.add_incoming(&[(&init_state, pre_bb)]);
        let i_val = i_phi.as_basic_value().into_int_value();
        let state = state_phi.as_basic_value().into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::ULT, i_val, len, "v.h.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, bdy_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(bdy_bb);
        let offset = self
            .builder
            .build_int_mul(i_val, elem_size, "v.h.off")
            .unwrap();
        let elem_ptr = unsafe {
            self.builder
                .build_gep(i8_t, data_ptr, &[offset], "v.h.ep")
                .unwrap()
        };
        let elem_h = self
            .builder
            .build_call(elem_hash, &[elem_ptr.into()], "v.h.eh")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let shl = self
            .builder
            .build_left_shift(state, rotate_amt, "v.h.shl")
            .unwrap();
        let shr = self
            .builder
            .build_right_shift(state, rotate_inv, false, "v.h.shr")
            .unwrap();
        let rotated = self.builder.build_or(shl, shr, "v.h.rot").unwrap();
        let xored = self.builder.build_xor(rotated, elem_h, "v.h.xor").unwrap();
        let new_state = self.builder.build_int_mul(xored, seed, "v.h.mul").unwrap();
        let i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "v.h.i1")
            .unwrap();
        i_phi.add_incoming(&[(&i_next, bdy_bb)]);
        state_phi.add_incoming(&[(&new_state, bdy_bb)]);
        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        self.builder.build_return(Some(&state)).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        hash_fn
    }

    /// Emit (or reuse) `karac_eq_Vec_<elem>(*const Vec, *const Vec) -> i1` —
    /// CONTENT equality for a `Vec[T]` element/key: compare lengths, then each
    /// element via the per-element eq fn (recurses through the dispatcher, so a
    /// `Vec[String]` / nested `Vec[Vec[_]]` element compares by content too).
    /// The byte-loop fallback in `emit_eq_fn_for_type` compares the
    /// `{ptr,len,cap}` HEADER (pointer identity) — the `Set[Vec[T]]` dedup bug
    /// (B-2026-06-20-15). Mirror of the `String` eq shape, element-typed; the
    /// hash sibling is `emit_hash_fn_for_vec`.
    pub(super) fn emit_eq_fn_for_vec(&mut self, elem_te: &TypeExpr) -> FunctionValue<'ctx> {
        let elem_name = Self::display_mangle_te(elem_te);
        let fn_name = format!("karac_eq_Vec_{elem_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let bool_t = self.context.bool_type();
        let vec_ty = self.vec_struct_type();
        let elem_ty = self.llvm_type_for_type_expr(elem_te);
        // Recurse first — emit may switch the builder's insert block.
        let elem_eq = self.emit_eq_fn_for_type_expr(elem_te);

        let saved_bb = self.builder.get_insert_block();
        let eq_fn_ty = bool_t.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let eq_fn = self
            .module
            .add_function(&fn_name, eq_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(eq_fn, "entry");
        let neq_bb = self.context.append_basic_block(eq_fn, "neq");
        let loop_hdr = self.context.append_basic_block(eq_fn, "eq.hdr");
        let loop_bdy = self.context.append_basic_block(eq_fn, "eq.bdy");
        let loop_exit = self.context.append_basic_block(eq_fn, "eq.exit");

        // neq: lengths differ or an element mismatched → false.
        self.builder.position_at_end(neq_bb);
        self.builder
            .build_return(Some(&bool_t.const_int(0, false)))
            .unwrap();

        // entry: load both lens + data ptrs; equal len → loop, else → neq.
        self.builder.position_at_end(entry_bb);
        let a_ptr = eq_fn.get_nth_param(0).unwrap().into_pointer_value();
        let b_ptr = eq_fn.get_nth_param(1).unwrap().into_pointer_value();

        let la_p = self
            .builder
            .build_struct_gep(vec_ty, a_ptr, 1, "la.p")
            .unwrap();
        let lb_p = self
            .builder
            .build_struct_gep(vec_ty, b_ptr, 1, "lb.p")
            .unwrap();
        let len_a = self
            .builder
            .build_load(i64_t, la_p, "la")
            .unwrap()
            .into_int_value();
        let len_b = self
            .builder
            .build_load(i64_t, lb_p, "lb")
            .unwrap()
            .into_int_value();
        let da_p = self
            .builder
            .build_struct_gep(vec_ty, a_ptr, 0, "da.p")
            .unwrap();
        let db_p = self
            .builder
            .build_struct_gep(vec_ty, b_ptr, 0, "db.p")
            .unwrap();
        let data_a = self
            .builder
            .build_load(ptr_ty, da_p, "da")
            .unwrap()
            .into_pointer_value();
        let data_b = self
            .builder
            .build_load(ptr_ty, db_p, "db")
            .unwrap()
            .into_pointer_value();
        let raw_size = elem_ty
            .size_of()
            .unwrap_or_else(|| i64_t.const_int(8, false));
        let elem_size = if raw_size.get_type().get_bit_width() == 64 {
            raw_size
        } else {
            self.builder
                .build_int_z_extend(raw_size, i64_t, "esz64")
                .unwrap()
        };
        let len_eq = self
            .builder
            .build_int_compare(IntPredicate::EQ, len_a, len_b, "len.eq")
            .unwrap();
        let entry_end = self.builder.get_insert_block().unwrap();
        self.builder
            .build_conditional_branch(len_eq, loop_hdr, neq_bb)
            .unwrap();

        // hdr: i in 0..len_a ? compare element : all-equal → true.
        self.builder.position_at_end(loop_hdr);
        let i_phi = self.builder.build_phi(i64_t, "eq.i").unwrap();
        i_phi.add_incoming(&[(&i64_t.const_zero(), entry_end)]);
        let i_val = i_phi.as_basic_value().into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::ULT, i_val, len_a, "eq.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, loop_bdy, loop_exit)
            .unwrap();

        self.builder.position_at_end(loop_bdy);
        let offset = self
            .builder
            .build_int_mul(i_val, elem_size, "eq.off")
            .unwrap();
        let ea = unsafe {
            self.builder
                .build_gep(i8_t, data_a, &[offset], "eq.ea")
                .unwrap()
        };
        let eb = unsafe {
            self.builder
                .build_gep(i8_t, data_b, &[offset], "eq.eb")
                .unwrap()
        };
        let r = self
            .builder
            .build_call(elem_eq, &[ea.into(), eb.into()], "eq.r")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "eq.i1")
            .unwrap();
        i_phi.add_incoming(&[(&i_next, loop_bdy)]);
        self.builder
            .build_conditional_branch(r, loop_hdr, neq_bb)
            .unwrap();

        self.builder.position_at_end(loop_exit);
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        eq_fn
    }

    /// Per-field-recursive hash for a registered user struct. Uses the
    /// struct's LLVM type from `self.type_decls.struct_types` and the field
    /// TypeExprs cached during `declare_structs` in
    /// `self.type_decls.struct_field_type_exprs`. Shape mirrors
    /// `emit_hash_fn_for_tuple`.
    ///
    /// Only invoked for non-shared structs (value layout): shared
    /// structs flow through a different code path that's pointer-
    /// based already (the heap layout has a refcount prefix; identity
    /// equality / refcount hashing applies). Map-of-shared-struct keys
    /// route through `emit_hash_fn_for_type`'s integer/pointer path,
    /// not here.
    /// A shared box's USER AREA as its own struct type: the heap type minus
    /// the `base` header slots the refcount (and, for a weak-targeted type,
    /// the weak count) occupy. The same twin [`Self::shared_gep_layout`]
    /// builds for a headerless object — anonymous struct types are uniqued by
    /// LLVM, so rebuilding it per call site is free.
    fn shared_user_area_type(
        &self,
        heap_type: inkwell::types::StructType<'ctx>,
        base: u32,
    ) -> inkwell::types::StructType<'ctx> {
        let fields: Vec<BasicTypeEnum<'ctx>> = heap_type
            .get_field_types()
            .into_iter()
            .skip(base as usize)
            .collect();
        self.context.struct_type(&fields, false)
    }

    /// Structural EQ for a `shared struct` / `shared enum` KEY
    /// (B-2026-08-27-4).
    ///
    /// The struct arm of [`Self::emit_eq_fn_for_type_expr`] excludes shared
    /// types, so a shared key fell through to the byte-compare fallback — and
    /// a shared key's blob IS a pointer, so that compared POINTER IDENTITY.
    /// `m.contains_key(twin)` then answered `false` on the compiled backends
    /// for a structurally-equal key the interpreter matched, and `Set` failed
    /// to dedup. design.md § Equality is explicit that this is wrong: "`==`
    /// always means structural equality … The `Eq` trait determines `==`
    /// REGARDLESS of whether the compiler chose RC or owned representation.
    /// There is no reference-identity short-circuit". Reference identity is
    /// reachable, deliberately, under its own name (`ref_eq`).
    ///
    /// The rule this implements is exactly "a shared key behaves as its
    /// NON-SHARED twin would". It therefore inherits, rather than fixes, the
    /// separate defect that a plain ENUM key with a heap-bearing payload also
    /// compares its payload words instead of recursing — measured on
    /// `enum S { A { s: String } }` with no `shared` in sight.
    ///
    /// NO POINTER FAST PATH. `pa == pb → true` is tempting and would also
    /// terminate a cyclic comparison, but it is observable: a `f64` NaN field
    /// makes a value structurally unequal to ITSELF, and the fast path would
    /// answer `true` where the interpreter answers `false`. The null test
    /// below is a guard, not a fast path — it fires only where a field walk
    /// would fault.
    pub(super) fn emit_eq_fn_for_shared(&mut self, name: &str) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_eq_{name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        if let Some(f) = self.try_emit_user_impl_eq_fn(name, &fn_name) {
            return f;
        }
        let Some(info) = self.type_decls.shared_types.get(name).cloned() else {
            let key_ty = self.context.ptr_type(AddressSpace::default()).into();
            return self.emit_eq_fn_for_type(name, key_ty);
        };

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let bool_t = self.context.bool_type();
        let saved_bb = self.builder.get_insert_block();
        // DECLARED BEFORE THE BODY IS EMITTED, so a self-referential shared
        // struct (`Node { next: Option[Node] }`) finds itself in the module
        // and terminates instead of recursing forever in the emitter.
        let eq_fn = self.module.add_function(
            &fn_name,
            bool_t.fn_type(&[ptr_ty.into(), ptr_ty.into()], false),
            Some(Linkage::Internal),
        );

        // The STRUCT walk is not duplicated here (B-2026-08-27-5).
        // `emit_shared_struct_eq_fn` already owns it — same field list, same
        // `shared_gep_layout` base, and since B-2026-08-27-5 the same niche
        // handling — so this fn is the SLOT ADAPTER over it: load, null-guard,
        // delegate. Two independent structural comparators for one type is the
        // hazard, and they existed side by side (`karac_eq_<T>` for map keys,
        // `karac_sheq_<T>` for `==`) with different niche behaviour, which is
        // how `contains_key` and `==` came to answer differently for one pair.
        //
        // Shared ENUMS keep the area compare below: their payload area is not
        // a field list, so `emit_shared_struct_eq_fn` (which requires a
        // registered field list) does not apply, and a byte compare of the
        // user area is what a NON-shared enum key does — the rule this fn
        // follows. `base` below is for that branch; the struct branch needs no
        // layout of its own now that it delegates.
        //
        // Registering `eq_fn` above BEFORE this call is what makes the mutual
        // recursion terminate: a niche field sends
        // `emit_shared_struct_eq_fn` straight back here for the inner type,
        // and a self-referential `Node { next: Option[Node] }` closes that
        // loop on itself.
        let struct_walk = if info.is_enum {
            None
        } else {
            Some(self.emit_shared_struct_eq_fn(name))
        };

        let base = self.shared_gep_layout(name, info.heap_type).1;
        let entry_bb = self.context.append_basic_block(eq_fn, "entry");
        let neq_bb = self.context.append_basic_block(eq_fn, "neq");
        let null_bb = self.context.append_basic_block(eq_fn, "null");
        let fields_bb = self.context.append_basic_block(eq_fn, "fields");
        self.builder.position_at_end(neq_bb);
        self.builder
            .build_return(Some(&bool_t.const_int(0, false)))
            .unwrap();

        self.builder.position_at_end(entry_bb);
        let a_slot = eq_fn.get_nth_param(0).unwrap().into_pointer_value();
        let b_slot = eq_fn.get_nth_param(1).unwrap().into_pointer_value();
        let pa = self
            .builder
            .build_load(ptr_ty, a_slot, "sh.eq.a")
            .unwrap()
            .into_pointer_value();
        let pb = self
            .builder
            .build_load(ptr_ty, b_slot, "sh.eq.b")
            .unwrap()
            .into_pointer_value();
        let a_null = self.builder.build_is_null(pa, "sh.eq.a.null").unwrap();
        let b_null = self.builder.build_is_null(pb, "sh.eq.b.null").unwrap();
        let either_null = self
            .builder
            .build_or(a_null, b_null, "sh.eq.either.null")
            .unwrap();
        self.builder
            .build_conditional_branch(either_null, null_bb, fields_bb)
            .unwrap();

        // Either side null: equal iff BOTH are (two `None` niche slots).
        self.builder.position_at_end(null_bb);
        let both = self
            .builder
            .build_and(a_null, b_null, "sh.eq.both.null")
            .unwrap();
        self.builder.build_return(Some(&both)).unwrap();

        self.builder.position_at_end(fields_bb);
        if let Some(walk) = struct_walk {
            let r = self
                .builder
                .build_call(walk, &[pa.into(), pb.into()], "sh.eq.walk")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            self.builder.build_return(Some(&r)).unwrap();
        } else if info.is_enum {
            // The USER AREA only — the rc header differs between two
            // structurally-equal objects, so comparing the whole box would
            // answer `false` for every distinct pair. The area type is the
            // heap type minus its header slots, the same twin
            // `shared_gep_layout` builds for a headerless object, and the byte
            // loop reads its SIZE rather than its field offsets.
            let area_ty = self.shared_user_area_type(info.heap_type, base);
            let inner = self.emit_eq_fn_for_type(&format!("{name}__area"), area_ty.into());
            let aa = self
                .builder
                .build_struct_gep(info.heap_type, pa, base, "sh.eq.area.a")
                .unwrap();
            let ab = self
                .builder
                .build_struct_gep(info.heap_type, pb, base, "sh.eq.area.b")
                .unwrap();
            let eq = self
                .builder
                .build_call(inner, &[aa.into(), ab.into()], "sh.eq.area")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            self.builder.build_return(Some(&eq)).unwrap();
        }

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        eq_fn
    }

    /// Structural HASH for a `shared struct` / `shared enum` KEY — the twin of
    /// [`Self::emit_eq_fn_for_shared`], and required to move WITH it. Equal
    /// keys that hash differently never meet in the table, so fixing equality
    /// alone would leave the lookup missing exactly as before.
    pub(super) fn emit_hash_fn_for_shared(&mut self, name: &str) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_hash_{name}{}", self.hash_hasher.mangle_suffix());
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        let Some(info) = self.type_decls.shared_types.get(name).cloned() else {
            let key_ty = self.context.ptr_type(AddressSpace::default()).into();
            return self.emit_hash_fn_for_type(name, key_ty);
        };

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let saved_bb = self.builder.get_insert_block();
        // Declared before the body, for the reason the eq twin documents.
        let hash_fn = self.module.add_function(
            &fn_name,
            i64_t.fn_type(&[ptr_ty.into()], false),
            Some(Linkage::Internal),
        );

        let field_tes: Vec<crate::ast::TypeExpr> = if info.is_enum {
            Vec::new()
        } else {
            self.type_decls
                .struct_field_type_exprs
                .get(name)
                .cloned()
                .unwrap_or_default()
        };
        let child_fns: Vec<FunctionValue<'ctx>> = field_tes
            .iter()
            .enumerate()
            .map(
                |(i, te)| match info.niche_option_fields.get(i).and_then(|n| n.as_ref()) {
                    Some(inner) => {
                        let inner = inner.clone();
                        self.emit_hash_fn_for_shared(&inner)
                    }
                    None => self.emit_hash_fn_for_type_expr(te),
                },
            )
            .collect();

        let (gep_ty, base) = self.shared_gep_layout(name, info.heap_type);
        let entry_bb = self.context.append_basic_block(hash_fn, "entry");
        let null_bb = self.context.append_basic_block(hash_fn, "null");
        let body_bb = self.context.append_basic_block(hash_fn, "body");
        self.builder.position_at_end(null_bb);
        self.builder
            .build_return(Some(&i64_t.const_zero()))
            .unwrap();

        self.builder.position_at_end(entry_bb);
        let slot = hash_fn.get_nth_param(0).unwrap().into_pointer_value();
        let obj = self
            .builder
            .build_load(ptr_ty, slot, "sh.h.obj")
            .unwrap()
            .into_pointer_value();
        let is_null = self.builder.build_is_null(obj, "sh.h.null").unwrap();
        self.builder
            .build_conditional_branch(is_null, null_bb, body_bb)
            .unwrap();

        // A `None` niche slot hashes 0 — and must, since the eq twin calls two
        // of them equal.
        self.builder.position_at_end(body_bb);
        if info.is_enum {
            // User area only, for the reason the eq twin gives: hashing the rc
            // header would give two structurally-equal boxes different digests
            // and they would never meet in the table.
            let area_ty = self.shared_user_area_type(info.heap_type, base);
            let inner = self.emit_hash_fn_for_type(&format!("{name}__area"), area_ty.into());
            let ap = self
                .builder
                .build_struct_gep(info.heap_type, obj, base, "sh.h.area")
                .unwrap();
            let h = self
                .builder
                .build_call(inner, &[ap.into()], "sh.h.area.v")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            self.builder.build_return(Some(&h)).unwrap();
        } else {
            let seed = i64_t.const_int(Self::FXHASH_SEED, false);
            let rotate_amt = i64_t.const_int(Self::FXHASH_ROTATE, false);
            let rotate_inv = i64_t.const_int(64 - Self::FXHASH_ROTATE, false);
            let mut state: IntValue<'ctx> = i64_t.const_zero();
            for (i, child_fn) in child_fns.iter().enumerate() {
                let idx = base + i as u32;
                let fp = self
                    .builder
                    .build_struct_gep(gep_ty, obj, idx, &format!("sh.h.f{i}.p"))
                    .unwrap();
                let fh = self
                    .builder
                    .build_call(*child_fn, &[fp.into()], &format!("sh.h.f{i}"))
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let shl = self
                    .builder
                    .build_left_shift(state, rotate_amt, &format!("sh.h.f{i}.shl"))
                    .unwrap();
                let shr = self
                    .builder
                    .build_right_shift(state, rotate_inv, false, &format!("sh.h.f{i}.shr"))
                    .unwrap();
                let rot = self
                    .builder
                    .build_or(shl, shr, &format!("sh.h.f{i}.rot"))
                    .unwrap();
                let xored = self
                    .builder
                    .build_xor(rot, fh, &format!("sh.h.f{i}.xor"))
                    .unwrap();
                state = self
                    .builder
                    .build_int_mul(xored, seed, &format!("sh.h.f{i}.mul"))
                    .unwrap();
            }
            self.builder.build_return(Some(&state)).unwrap();
        }

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        hash_fn
    }

    pub(super) fn emit_hash_fn_for_struct(&mut self, struct_name: &str) -> FunctionValue<'ctx> {
        let fn_name = format!(
            "karac_hash_{struct_name}{}",
            self.hash_hasher.mangle_suffix()
        );
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        let field_tes = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)
            .cloned()
            .expect("emit_hash_fn_for_struct: struct must be registered");
        let struct_ty = *self
            .type_decls
            .struct_types
            .get(struct_name)
            .expect("emit_hash_fn_for_struct: struct LLVM type must be registered");
        let child_fns: Vec<FunctionValue<'ctx>> = field_tes
            .iter()
            .map(|te| self.emit_hash_fn_for_type_expr(te))
            .collect();

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let saved_bb = self.builder.get_insert_block();
        let hash_fn_ty = i64_t.fn_type(&[ptr_ty.into()], false);
        let hash_fn = self
            .module
            .add_function(&fn_name, hash_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(hash_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let key_ptr = hash_fn.get_nth_param(0).unwrap().into_pointer_value();

        // FxHash tail-mix, identical to the tuple combiner.
        let seed = i64_t.const_int(Self::FXHASH_SEED, false);
        let rotate_amt = i64_t.const_int(Self::FXHASH_ROTATE, false);
        let rotate_inv = i64_t.const_int(64 - Self::FXHASH_ROTATE, false);
        let mut state: IntValue<'ctx> = i64_t.const_zero();
        for (i, child_fn) in child_fns.iter().enumerate() {
            let field_ptr = self
                .builder
                .build_struct_gep(struct_ty, key_ptr, i as u32, &format!("s.f{i}.p"))
                .unwrap();
            let elem_hash = self
                .builder
                .build_call(*child_fn, &[field_ptr.into()], &format!("s.f{i}.h"))
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let shl = self
                .builder
                .build_left_shift(state, rotate_amt, &format!("s.f{i}.shl"))
                .unwrap();
            let shr = self
                .builder
                .build_right_shift(state, rotate_inv, false, &format!("s.f{i}.shr"))
                .unwrap();
            let rotated = self
                .builder
                .build_or(shl, shr, &format!("s.f{i}.rot"))
                .unwrap();
            let xored = self
                .builder
                .build_xor(rotated, elem_hash, &format!("s.f{i}.xor"))
                .unwrap();
            state = self
                .builder
                .build_int_mul(xored, seed, &format!("s.f{i}.mul"))
                .unwrap();
        }
        self.builder.build_return(Some(&state)).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        hash_fn
    }

    /// Per-field-recursive eq for a registered user struct. Mirrors
    /// `emit_eq_fn_for_tuple`; short-circuits to `false` on the first
    /// mismatching field.
    pub(super) fn emit_eq_fn_for_struct(&mut self, struct_name: &str) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_eq_{struct_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        let field_tes = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)
            .cloned()
            .expect("emit_eq_fn_for_struct: struct must be registered");
        let struct_ty = *self
            .type_decls
            .struct_types
            .get(struct_name)
            .expect("emit_eq_fn_for_struct: struct LLVM type must be registered");
        let child_fns: Vec<FunctionValue<'ctx>> = field_tes
            .iter()
            .map(|te| self.emit_eq_fn_for_type_expr(te))
            .collect();

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let bool_t = self.context.bool_type();

        let saved_bb = self.builder.get_insert_block();
        let eq_fn_ty = bool_t.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let eq_fn = self
            .module
            .add_function(&fn_name, eq_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(eq_fn, "entry");
        let neq_bb = self.context.append_basic_block(eq_fn, "neq");
        self.builder.position_at_end(neq_bb);
        self.builder
            .build_return(Some(&bool_t.const_int(0, false)))
            .unwrap();

        self.builder.position_at_end(entry_bb);
        let a_ptr = eq_fn.get_nth_param(0).unwrap().into_pointer_value();
        let b_ptr = eq_fn.get_nth_param(1).unwrap().into_pointer_value();

        for (i, child_fn) in child_fns.iter().enumerate() {
            let fa = self
                .builder
                .build_struct_gep(struct_ty, a_ptr, i as u32, &format!("s.fa{i}"))
                .unwrap();
            let fb = self
                .builder
                .build_struct_gep(struct_ty, b_ptr, i as u32, &format!("s.fb{i}"))
                .unwrap();
            let r = self
                .builder
                .build_call(*child_fn, &[fa.into(), fb.into()], &format!("s.eq{i}"))
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let next_bb = self
                .context
                .append_basic_block(eq_fn, &format!("eq.next{i}"));
            self.builder
                .build_conditional_branch(r, next_bb, neq_bb)
                .unwrap();
            self.builder.position_at_end(next_bb);
        }
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        eq_fn
    }

    /// Structural `==` for a `shared struct` value (C1, ledger B-2026-06-19-9).
    /// A `shared struct` is an RC heap POINTER, so it misses the value-wise
    /// `compile_struct_eq` path. This synthesizes `bool(ptr a_obj, ptr b_obj)`
    /// taking the two RC pointers BY VALUE and comparing field-by-field through
    /// the heap layout, matching the interpreter's structural `Value::SharedStruct`
    /// equality: an `Arc::ptr_eq` identity fast-path, then a recursive field walk.
    ///
    /// Registered in the module BEFORE recursing into child eq fns so a
    /// self-referential shared struct (`shared struct Node { next: Node }`)
    /// resolves to this same cached fn rather than looping the emitter. (Runtime
    /// cyclic *data* infinite-loops exactly as the interpreter's structural
    /// compare does — A/B parity, not a new footgun.)
    ///
    /// Field dispatch: a nested `shared struct` field holds an 8-byte RC pointer
    /// in its slot, so it's loaded and recursed structurally; every other field
    /// kind (scalar / String / by-value struct / tuple / enum) goes through the
    /// existing slot-based `emit_eq_fn_for_type_expr`, which loads + compares.
    pub(super) fn emit_shared_struct_eq_fn(&mut self, struct_name: &str) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_sheq_{struct_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        let info = self
            .type_decls
            .shared_types
            .get(struct_name)
            .expect("emit_shared_struct_eq_fn: shared type must be registered")
            .clone();
        let field_tes = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)
            .cloned()
            .expect("emit_shared_struct_eq_fn: struct fields must be registered");

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let bool_t = self.context.bool_type();
        let i64_t = self.context.i64_type();

        let saved_bb = self.builder.get_insert_block();
        let eq_fn_ty = bool_t.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        // Register BEFORE recursing so a self-referential shared struct finds
        // this fn cached instead of re-entering the emitter.
        let eq_fn = self
            .module
            .add_function(&fn_name, eq_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(eq_fn, "entry");
        let walk_bb = self.context.append_basic_block(eq_fn, "walk");
        let eq_ret_bb = self.context.append_basic_block(eq_fn, "eq");
        let neq_bb = self.context.append_basic_block(eq_fn, "neq");

        self.builder.position_at_end(eq_ret_bb);
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();
        self.builder.position_at_end(neq_bb);
        self.builder
            .build_return(Some(&bool_t.const_int(0, false)))
            .unwrap();

        // Entry: `Arc::ptr_eq` identity fast-path (also short-circuits a
        // self-compare and a cycle that revisits the same allocation).
        self.builder.position_at_end(entry_bb);
        let a_obj = eq_fn.get_nth_param(0).unwrap().into_pointer_value();
        let b_obj = eq_fn.get_nth_param(1).unwrap().into_pointer_value();
        let a_int = self
            .builder
            .build_ptr_to_int(a_obj, i64_t, "sheq.ai")
            .unwrap();
        let b_int = self
            .builder
            .build_ptr_to_int(b_obj, i64_t, "sheq.bi")
            .unwrap();
        let same = self
            .builder
            .build_int_compare(IntPredicate::EQ, a_int, b_int, "sheq.same")
            .unwrap();
        self.builder
            .build_conditional_branch(same, eq_ret_bb, walk_bb)
            .unwrap();

        // Walk: field-by-field through the heap layout (skip the RC header via
        // `base`).
        self.builder.position_at_end(walk_bb);
        let (gep_ty, base) = self.shared_gep_layout(struct_name, info.heap_type);
        for (i, field_te) in field_tes.iter().enumerate() {
            let idx = i as u32 + base;
            let fa = self
                .builder
                .build_struct_gep(gep_ty, a_obj, idx, &format!("sheq.fa{i}"))
                .unwrap();
            let fb = self
                .builder
                .build_struct_gep(gep_ty, b_obj, idx, &format!("sheq.fb{i}"))
                .unwrap();
            // B-2026-08-27-5 — a NICHE-encoded `Option[shared Inner]` field.
            // The slot is a BARE pointer (null = None), one word; the
            // comparator `emit_eq_fn_for_type_expr` builds for `Option[T]` is
            // for the conventional `{tag, w0, w1, w2}`, four words. Its byte
            // loop therefore ran off the end of the field: measured on
            // `shared struct Padded { v, next, p1, p2, p3 }`, where the
            // surplus 24 bytes consumed p1/p2/p3 and the answer came out right
            // only because those happened to be equal; with the niche field
            // LAST it reads past the object and two `None`s compared UNEQUAL.
            //
            // `emit_eq_fn_for_shared` is the right callee and needs no new
            // machinery: it takes the SLOT, loads it, and null-guards (both
            // null equal, one null unequal) before walking — which is exactly
            // the niche encoding's three cases. Checked BEFORE the bare-shared
            // arm below because a niche field's TypeExpr is `Option[Inner]`,
            // not `Inner`, so that arm never sees it.
            if let Some(inner) = info
                .niche_option_fields
                .get(i)
                .and_then(|n| n.as_ref())
                .cloned()
            {
                let inner_fn = self.emit_eq_fn_for_shared(&inner);
                let r = self
                    .builder
                    .build_call(inner_fn, &[fa.into(), fb.into()], &format!("sheq.n{i}"))
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let next_bb = self
                    .context
                    .append_basic_block(eq_fn, &format!("sheq.next{i}"));
                self.builder
                    .build_conditional_branch(r, next_bb, neq_bb)
                    .unwrap();
                self.builder.position_at_end(next_bb);
                continue;
            }
            // Nested shared-struct field: load the inner RC pointer and recurse
            // structurally. Shared *enums* (out of C1 scope) fall to the
            // slot-based dispatcher, which compares them by pointer identity.
            let inner_shared: Option<String> = match &field_te.kind {
                TypeKind::Path(p)
                    if p.segments.len() == 1
                        && self
                            .type_decls
                            .shared_types
                            .get(p.segments[0].as_str())
                            .map(|si| !si.is_enum)
                            .unwrap_or(false) =>
                {
                    Some(p.segments[0].clone())
                }
                _ => None,
            };
            let r = if let Some(inner) = inner_shared {
                let inner_fn = self.emit_shared_struct_eq_fn(&inner);
                let field_llvm = gep_ty.get_field_type_at_index(idx).unwrap();
                let ia = self
                    .builder
                    .build_load(field_llvm, fa, &format!("sheq.ia{i}"))
                    .unwrap()
                    .into_pointer_value();
                let ib = self
                    .builder
                    .build_load(field_llvm, fb, &format!("sheq.ib{i}"))
                    .unwrap()
                    .into_pointer_value();
                self.builder
                    .build_call(inner_fn, &[ia.into(), ib.into()], &format!("sheq.r{i}"))
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value()
            } else {
                let child = self.emit_eq_fn_for_type_expr(field_te);
                self.builder
                    .build_call(child, &[fa.into(), fb.into()], &format!("sheq.r{i}"))
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value()
            };
            let next_bb = self
                .context
                .append_basic_block(eq_fn, &format!("sheq.next{i}"));
            self.builder
                .build_conditional_branch(r, next_bb, neq_bb)
                .unwrap();
            self.builder.position_at_end(next_bb);
        }
        // All fields equal.
        self.builder.build_unconditional_branch(eq_ret_bb).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        eq_fn
    }

    /// Emit a per-field-recursive hash function for an n-tuple. Each field's
    /// hash is computed by recursing into `emit_hash_fn_for_type_expr` (so
    /// `(String, i64)` correctly hashes the String contents, not the struct
    /// bytes), then combined into a running state via the FxHash tail-mix
    /// `state = (state.rotate_left(5) ^ field_hash) * FXHASH_SEED`. Matches
    /// the per-K hash emission shape selected by the
    /// `bench/hash_quality/` investigation.
    pub(super) fn emit_hash_fn_for_tuple(
        &mut self,
        type_name: &str,
        elems: &[TypeExpr],
    ) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_hash_{type_name}{}", self.hash_hasher.mangle_suffix());
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        let elems_owned: Vec<TypeExpr> = elems.to_vec();
        let child_fns: Vec<FunctionValue<'ctx>> = elems_owned
            .iter()
            .map(|e| self.emit_hash_fn_for_type_expr(e))
            .collect();
        let field_tys: Vec<BasicTypeEnum<'ctx>> = elems_owned
            .iter()
            .map(|e| self.llvm_type_for_type_expr(e))
            .collect();
        let tuple_ty = self.context.struct_type(&field_tys, false);

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let saved_bb = self.builder.get_insert_block();
        let hash_fn_ty = i64_t.fn_type(&[ptr_ty.into()], false);
        let hash_fn = self
            .module
            .add_function(&fn_name, hash_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(hash_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let key_ptr = hash_fn.get_nth_param(0).unwrap().into_pointer_value();

        // FxHash tail-mix: state = (state.rotate_left(5) ^
        // field_hash) * FXHASH_SEED. Initial state = 0 collapses
        // the first field's mix to `field_hash_0 * SEED`,
        // matching the inline primitive fast path for a 1-element
        // "tuple". For n>1 fields, subsequent fields rotate and
        // chain.
        let seed = i64_t.const_int(Self::FXHASH_SEED, false);
        let rotate_amt = i64_t.const_int(Self::FXHASH_ROTATE, false);
        let rotate_inv = i64_t.const_int(64 - Self::FXHASH_ROTATE, false);
        let mut state: IntValue<'ctx> = i64_t.const_zero();
        for (i, child_fn) in child_fns.iter().enumerate() {
            let field_ptr = self
                .builder
                .build_struct_gep(tuple_ty, key_ptr, i as u32, &format!("t.f{i}.p"))
                .unwrap();
            let elem_hash = self
                .builder
                .build_call(*child_fn, &[field_ptr.into()], &format!("t.f{i}.h"))
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let shl = self
                .builder
                .build_left_shift(state, rotate_amt, &format!("t.f{i}.shl"))
                .unwrap();
            let shr = self
                .builder
                .build_right_shift(state, rotate_inv, false, &format!("t.f{i}.shr"))
                .unwrap();
            let rotated = self
                .builder
                .build_or(shl, shr, &format!("t.f{i}.rot"))
                .unwrap();
            let xored = self
                .builder
                .build_xor(rotated, elem_hash, &format!("t.f{i}.xor"))
                .unwrap();
            state = self
                .builder
                .build_int_mul(xored, seed, &format!("t.f{i}.mul"))
                .unwrap();
        }
        self.builder.build_return(Some(&state)).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        hash_fn
    }

    /// Emit a per-field-recursive eq function for an n-tuple. Each field is
    /// compared via the recursively-emitted per-field eq fn; the function
    /// short-circuits to `false` on the first mismatch.
    pub(super) fn emit_eq_fn_for_tuple(
        &mut self,
        type_name: &str,
        elems: &[TypeExpr],
    ) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_eq_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        let elems_owned: Vec<TypeExpr> = elems.to_vec();
        let child_fns: Vec<FunctionValue<'ctx>> = elems_owned
            .iter()
            .map(|e| self.emit_eq_fn_for_type_expr(e))
            .collect();
        let field_tys: Vec<BasicTypeEnum<'ctx>> = elems_owned
            .iter()
            .map(|e| self.llvm_type_for_type_expr(e))
            .collect();
        let tuple_ty = self.context.struct_type(&field_tys, false);

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let bool_t = self.context.bool_type();

        let saved_bb = self.builder.get_insert_block();
        let eq_fn_ty = bool_t.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let eq_fn = self
            .module
            .add_function(&fn_name, eq_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(eq_fn, "entry");
        let neq_bb = self.context.append_basic_block(eq_fn, "neq");
        self.builder.position_at_end(neq_bb);
        self.builder
            .build_return(Some(&bool_t.const_int(0, false)))
            .unwrap();

        self.builder.position_at_end(entry_bb);
        let a_ptr = eq_fn.get_nth_param(0).unwrap().into_pointer_value();
        let b_ptr = eq_fn.get_nth_param(1).unwrap().into_pointer_value();

        for (i, child_fn) in child_fns.iter().enumerate() {
            let fa = self
                .builder
                .build_struct_gep(tuple_ty, a_ptr, i as u32, &format!("t.fa{i}"))
                .unwrap();
            let fb = self
                .builder
                .build_struct_gep(tuple_ty, b_ptr, i as u32, &format!("t.fb{i}"))
                .unwrap();
            let r = self
                .builder
                .build_call(*child_fn, &[fa.into(), fb.into()], &format!("t.eq{i}"))
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let next_bb = self
                .context
                .append_basic_block(eq_fn, &format!("eq.next{i}"));
            self.builder
                .build_conditional_branch(r, next_bb, neq_bb)
                .unwrap();
            self.builder.position_at_end(next_bb);
        }
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        eq_fn
    }
}
