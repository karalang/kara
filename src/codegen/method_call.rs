//! Object method-call dispatch.
//!
//! Houses `compile_method_call` — the top-level dispatcher for
//! `object.method(args)` shapes. Recognises indexed-receiver,
//! field-receiver, entry-chain, and clone-on-collection shortcuts
//! before falling through to the impl-block lookup path. Also
//! handles primitive-type-receiver associated calls
//! (`i64.add(...)`) by delegating to `compile_assoc_call`, and the
//! receiver-form `cmp` (`lhs.cmp(rhs)` → Ordering tag synthesis).
//!
//! Lives in a sibling `impl<'ctx> super::Codegen<'ctx>` block.

use crate::ast::*;

use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValue, BasicValueEnum};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

/// Adaptor kind for one step of a fused iterator chain
/// (`peel_fused_map_filter_chain` / `build_fused_chain_body`). `Map`
/// transforms the element; every other kind is an identity passthrough that
/// gates which elements reach the downstream stages: `Filter` per-element,
/// `TakeWhile` ends the loop on the first failing element (`break`),
/// `SkipWhile` drops a prefix via a pre-loop latch flag, `Take`/`Skip`/
/// `StepBy` count elements reaching the stage via a pre-loop counter (the
/// count expr is bound once, before the loop), and `Inspect` runs its
/// closure for the side effect only. Pre-loop state is hoisted by
/// `fused_chain_prelude` (B-2026-07-14-8 legs). These mirror the collect
/// engine's `IterAdaptor` stage shapes exactly (same counter/latch
/// arithmetic), so both fused surfaces stay behaviorally identical.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FusedStepKind {
    Map,
    Filter,
    FilterMap,
    TakeWhile,
    SkipWhile,
    Take,
    Skip,
    StepBy,
    Inspect,
}

/// One peeled fused-chain step: `(kind, closure_param, body_or_pred)`.
/// For the count kinds (`Take`/`Skip`/`StepBy`) the param is `""` (no
/// closure) and the expr slot holds the count expression.
pub(super) type FusedChainStep = (FusedStepKind, String, Expr);

impl<'ctx> super::Codegen<'ctx> {
    /// `char.try_from(n) -> Result[char, i64]` (#10). Widen the codepoint arg
    /// to i64 (sign- or zero-extend per the source's signedness, so a negative
    /// signed input stays negative and fails the lower bound), validate it is a
    /// Unicode scalar value (`0 <= cp <= 0x10FFFF` and NOT in the surrogate
    /// range `0xD800..=0xDFFF`), then branch: `Ok(char)` with the codepoint
    /// truncated to the i32 `char` repr, or `Err(cp)` carrying the offending
    /// value. PHI-merge the two `Result` aggregates. Mirrors the branch+phi
    /// shape of `Vec.try_from_slice`.
    fn compile_char_try_from(&mut self, args: &[CallArg]) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err(format!(
                "char.try_from expects 1 argument, got {}",
                args.len()
            ));
        }
        let i64_t = self.context.i64_type();
        let i32_t = self.context.i32_type();
        let raw = self.compile_expr(&args[0].value)?;
        let iv = match raw {
            BasicValueEnum::IntValue(iv) => iv,
            _ => return Err("char.try_from expects an integer argument".to_string()),
        };
        let src_unsigned = self.expr_is_unsigned_int(&args[0].value);
        let cp = if iv.get_type().get_bit_width() < 64 {
            if src_unsigned {
                self.builder
                    .build_int_z_extend(iv, i64_t, "ctf.zx")
                    .unwrap()
            } else {
                self.builder
                    .build_int_s_extend(iv, i64_t, "ctf.sx")
                    .unwrap()
            }
        } else {
            iv
        };
        let zero = i64_t.const_int(0, false);
        let max = i64_t.const_int(0x10FFFF, false);
        let sur_lo = i64_t.const_int(0xD800, false);
        let sur_hi = i64_t.const_int(0xDFFF, false);
        let ge0 = self
            .builder
            .build_int_compare(IntPredicate::SGE, cp, zero, "ctf.ge0")
            .unwrap();
        let le_max = self
            .builder
            .build_int_compare(IntPredicate::SLE, cp, max, "ctf.lemax")
            .unwrap();
        let in_range = self.builder.build_and(ge0, le_max, "ctf.inrange").unwrap();
        let ge_sur = self
            .builder
            .build_int_compare(IntPredicate::SGE, cp, sur_lo, "ctf.gesur")
            .unwrap();
        let le_sur = self
            .builder
            .build_int_compare(IntPredicate::SLE, cp, sur_hi, "ctf.lesur")
            .unwrap();
        let is_sur = self.builder.build_and(ge_sur, le_sur, "ctf.issur").unwrap();
        let not_sur = self.builder.build_not(is_sur, "ctf.notsur").unwrap();
        let valid = self
            .builder
            .build_and(in_range, not_sur, "ctf.valid")
            .unwrap();

        let cur_fn = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or("char.try_from outside a function context")?;
        let ok_bb = self.context.append_basic_block(cur_fn, "ctf.ok");
        let err_bb = self.context.append_basic_block(cur_fn, "ctf.err");
        let merge_bb = self.context.append_basic_block(cur_fn, "ctf.merge");
        self.builder
            .build_conditional_branch(valid, ok_bb, err_bb)
            .unwrap();

        self.builder.position_at_end(ok_bb);
        let ch = self
            .builder
            .build_int_truncate(cp, i32_t, "ctf.ch")
            .unwrap();
        let ok_result = self.build_nonshared_enum_value("Result", "Ok", &[ch.into()])?;
        let ok_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(err_bb);
        let err_result = self.build_nonshared_enum_value("Result", "Err", &[cp.into()])?;
        let err_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(ok_result.get_type(), "ctf.result")
            .unwrap();
        phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
        Ok(phi.as_basic_value())
    }

    /// `<int>.try_from(x: <int>) -> Result[<int>, String]` — numeric narrowing /
    /// sign-changing conversion (design.md § Conversion Traits). Widens the
    /// source to `i128`, compares against the target's inclusive bounds, and
    /// branches `Ok(value)` / `Err("out of range for T")`. Every in-scope target
    /// bound fits the `i64`/`u64` domain, so the `i128` bound constants are
    /// exact; widening the source to `i128` keeps the comparison honest even for
    /// an unsigned `i64` source above `i64::MAX`. Structural mirror of
    /// `compile_char_try_from`; the `Err` `String` is a static (`cap=0`) value,
    /// so the error path allocates nothing and needs no drop. Parity with the
    /// interpreter's `numeric_try_from_value`; also the lowered target of the
    /// `.try_into()` desugar.
    pub(super) fn compile_numeric_try_from(
        &mut self,
        target: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err(format!(
                "{}.try_from expects 1 argument, got {}",
                target,
                args.len()
            ));
        }
        let i64_t = self.context.i64_type();
        let i128_t = self.context.i128_type();
        let raw = self.compile_expr(&args[0].value)?;
        let iv = match raw {
            BasicValueEnum::IntValue(iv) => iv,
            _ => return Err(format!("{}.try_from expects an integer argument", target)),
        };
        let src_unsigned = self.expr_is_unsigned_int(&args[0].value);
        // Normalize the source to i64 (the value model) preserving its value.
        let src64 = if iv.get_type().get_bit_width() < 64 {
            if src_unsigned {
                self.builder
                    .build_int_z_extend(iv, i64_t, "ntf.zx64")
                    .unwrap()
            } else {
                self.builder
                    .build_int_s_extend(iv, i64_t, "ntf.sx64")
                    .unwrap()
            }
        } else {
            iv
        };
        // Widen to i128 so the comparison can't itself overflow — an unsigned
        // i64 source above i64::MAX zero-extends to a positive i128.
        let src128 = if src_unsigned {
            self.builder
                .build_int_z_extend(src64, i128_t, "ntf.zx128")
                .unwrap()
        } else {
            self.builder
                .build_int_s_extend(src64, i128_t, "ntf.sx128")
                .unwrap()
        };
        let (min, max) = crate::numeric_conv::int_target_range(target)
            .ok_or_else(|| format!("{} is not an integer target", target))?;
        // min >= i64::MIN (sign-extend the i64 bit pattern), max <= u64::MAX
        // (zero-extend) — both in-domain for a single-word const_int.
        let min128 = i128_t.const_int(min as i64 as u64, true);
        let max128 = i128_t.const_int(max as u64, false);
        let ge = self
            .builder
            .build_int_compare(IntPredicate::SGE, src128, min128, "ntf.ge")
            .unwrap();
        let le = self
            .builder
            .build_int_compare(IntPredicate::SLE, src128, max128, "ntf.le")
            .unwrap();
        let valid = self.builder.build_and(ge, le, "ntf.valid").unwrap();

        let cur_fn = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or("<int>.try_from outside a function context")?;
        let ok_bb = self.context.append_basic_block(cur_fn, "ntf.ok");
        let err_bb = self.context.append_basic_block(cur_fn, "ntf.err");
        let merge_bb = self.context.append_basic_block(cur_fn, "ntf.merge");
        self.builder
            .build_conditional_branch(valid, ok_bb, err_bb)
            .unwrap();

        self.builder.position_at_end(ok_bb);
        // In range: the i64 payload word carries the value's bit pattern; a
        // match binding typed as the target re-reads it at the target width.
        let ok_result = self.build_nonshared_enum_value("Result", "Ok", &[src64.into()])?;
        let ok_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(err_bb);
        let msg = self.build_static_string_value(&format!("out of range for {}", target));
        let err_result = self.build_nonshared_enum_value("Result", "Err", &[msg.into()])?;
        let err_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(ok_result.get_type(), "ntf.result")
            .unwrap();
        phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
        Ok(phi.as_basic_value())
    }

    /// Coerce an integer value to `target` width: truncate when wider, zero- or
    /// sign-extend (per `unsigned`) when narrower, identity when equal.
    pub(super) fn coerce_int_to(
        &self,
        v: inkwell::values::IntValue<'ctx>,
        target: inkwell::types::IntType<'ctx>,
        unsigned: bool,
    ) -> inkwell::values::IntValue<'ctx> {
        let sw = v.get_type().get_bit_width();
        let tw = target.get_bit_width();
        if sw == tw {
            v
        } else if sw > tw {
            self.builder.build_int_truncate(v, target, "iw.tr").unwrap()
        } else if unsigned {
            self.builder.build_int_z_extend(v, target, "iw.zx").unwrap()
        } else {
            self.builder.build_int_s_extend(v, target, "iw.sx").unwrap()
        }
    }

    /// Does a USER impl block define `<type_head>.<method>`?
    ///
    /// The builtin container dispatchers (Vec/String, Slice, Map, Set) each need
    /// this to decide whether an unrecognized method name should loud-fail with
    /// their own "not yet implemented" error or fall through to the generic
    /// user-impl dispatch further down. Two tables have to be consulted, and
    /// each of those dispatchers previously checked only the first:
    ///
    ///  - `module`, for a NON-GENERIC impl method, emitted eagerly under the
    ///    unmangled `Type.method` name;
    ///  - `generic_fns`, for a GENERIC one (`impl[T] Zero for Vec[T]`), which
    ///    `make_generic_impl_method_function` routes through the monomorphizer
    ///    so its bodies are mangled per instantiation and NO unmangled symbol
    ///    ever exists.
    ///
    /// B-2026-08-13-8 half A: consulting only `module` made every generic impl
    /// on a builtin container check-green and interp-green with the build dead,
    /// even though the generic dispatch reads exactly this key and would have
    /// handled it. The same impl on a USER struct always worked, because no
    /// builtin dispatcher intercepts that receiver first — an asymmetry that
    /// made a gate look like a missing feature. One helper rather than four
    /// open-coded lookups, so the dispatchers cannot drift apart again.
    pub(super) fn user_impl_method_exists(
        &self,
        call_span: &crate::token::Span,
        type_head: &str,
        method: &str,
    ) -> bool {
        // B-2026-08-13-8 — ask about the name this call site will actually
        // dispatch to. When the program has two impls on one head, neither of
        // them owns the unqualified `Vec.describe` symbol any more, so a gate
        // asking for that name would decline and loud-fail the build on a
        // program the typechecker resolved fine.
        let head = self.impl_dispatch_segment_at(call_span, method, type_head);
        let qualified = format!("{head}.{method}");
        self.module.get_function(&qualified).is_some()
            || self.mono_state.generic_fns.contains_key(&qualified)
    }

    /// The dispatch type segment for a method call — the receiver's head name,
    /// or the QUALIFIED segment (`Vec[i64]`) when this call resolved to one of
    /// several impls that share a head.
    ///
    /// B-2026-08-13-8. Codegen cannot work this out for itself:
    /// `inferred_receiver_type` reads `var_type_names`, which holds head names,
    /// so `Vec[i64]` and `Vec[String]` receivers are indistinguishable here.
    /// The typechecker compared the impls' `target_args` vector-wise at check
    /// time and recorded the winner for this exact call site; this reads it.
    ///
    /// The lookup is keyed by `(span, method)`. Every sibling table keyed by
    /// span alone has to re-check its method segment afterwards, because the
    /// parser sets `MethodCall.span == receiver.span` and a chain shares one
    /// span; carrying the method name in the key makes that class of mistake
    /// unrepresentable rather than merely guarded.
    pub(super) fn impl_dispatch_segment_at(
        &self,
        call_span: &crate::token::Span,
        method: &str,
        head: &str,
    ) -> String {
        self.span_tables
            .method_impl_dispatch
            .get(&((call_span.offset, call_span.length), method.to_string()))
            .cloned()
            .unwrap_or_else(|| head.to_string())
    }

    /// Recover the receiver's declared integer width + signedness for a
    /// width-dependent scalar method (`pow`, the bit intrinsics). Codegen widens
    /// narrow integers to i64 in value flow, so the LLVM value type is unreliable;
    /// the typechecker's `method_callee_types["<recv>.<method>"]` entry (keyed by
    /// the call/receiver span) carries the exact source type. When an OUTER chained
    /// call has clobbered that span's entry (its method segment no longer matches
    /// `method`), fall back to the receiver expression's declared type / literal
    /// suffix — matching the interpreter's non-aliased `args_close_span` recovery.
    /// Defaults to signed 64-bit (the language's default integer).
    pub(super) fn receiver_int_kind(
        &self,
        object: &Expr,
        call_span: &crate::token::Span,
        method: &str,
    ) -> (u32, bool) {
        fn parse(name: &str) -> Option<(u32, bool)> {
            Some(match name {
                "i8" => (8, false),
                "i16" => (16, false),
                "i32" => (32, false),
                "i64" | "isize" => (64, false),
                "u8" => (8, true),
                "u16" => (16, true),
                "u32" => (32, true),
                "u64" | "usize" => (64, true),
                // 128-bit (B-2026-08-19-8 stage 3). Without these the names
                // fell to `None` and the caller's 64-bit default, so a
                // width-sensitive intrinsic on an `i128` answered for 64 bits —
                // `(2^100).count_ones()` returned 0 instead of 1.
                "i128" => (128, false),
                "u128" => (128, true),
                _ => return None,
            })
        }
        // Prefer the RECEIVER's own resolved type over the span-keyed
        // `method_callee_types` table. The parser aliases a chained call's
        // `MethodCall.span` to its receiver's span, so every call in a chain
        // (`x.leading_zeros().leading_zeros()`) shares ONE table key — the
        // last insert wins, and an inner width-sensitive int method
        // (`leading_zeros` / `rotate_left` / …) would read the OUTER call's
        // receiver width and miscompile (interp != codegen; B-2026-07-18-36).
        // `type_name_of_expr` resolves the receiver by name / layout, unaffected
        // by the span aliasing, so it is authoritative when it answers; the
        // table stays a fallback for receivers it can't name (a fresh temp /
        // builtin-returning chain link), where the table's own last-insert value
        // is the right one for that outer call.
        if let Some(name) = self.type_name_of_expr(object) {
            if let Some(k) = parse(&name) {
                return k;
            }
        }
        if let Some(callee) = self
            .span_tables
            .method_callee_types
            .get(&(call_span.offset, call_span.length))
        {
            if let Some((recv, m)) = callee.split_once('.') {
                if m == method {
                    if let Some(k) = parse(recv) {
                        return k;
                    }
                }
            }
        }
        if let ExprKind::Integer(_, Some(suf)) = &object.kind {
            use crate::token::IntSuffix::*;
            return match suf {
                I8 => (8, false),
                I16 => (16, false),
                I32 => (32, false),
                I64 => (64, false),
                // 128 was folded into the 64-bit arm here (B-2026-08-19-8
                // stage 3b) — harmless while the type was unusable, wrong the
                // moment it is not.
                I128 => (128, false),
                U8 => (8, true),
                U16 => (16, true),
                U32 => (32, true),
                U64 => (64, true),
                U128 => (128, true),
                // Pointer-width, 64-bit in Kāra (B-2026-08-19-29).
                Usize => (64, true),
                Isize => (64, false),
            };
        }
        (64, false)
    }

    /// Raw-pointer instance methods on `*const T` / `*mut T` (design.md §
    /// raw pointers; additive-interop Slice 4 Path A, `B-2026-07-08-4`).
    /// Returns `Ok(Some(v))` when the call is a pointer method on a
    /// raw-pointer receiver, `Ok(None)` to fall through to normal dispatch
    /// (the receiver is not raw-pointer-typed — e.g. a user `Reader.read()`
    /// or a builder `.write()`).
    ///
    /// - `.offset(i)` / `.add(i)` — element-scaled pointer arithmetic (GEP
    ///   over the pointee type), returning a pointer.
    /// - `.read()` / `.read_unaligned()` / `.read_volatile()` — load the
    ///   pointee (unaligned sets align 1; volatile sets the volatile flag).
    /// - `.write(v)` / `.write_unaligned(v)` / `.write_volatile(v)` — store
    ///   `v` through the pointer, returning unit.
    ///
    /// The pointee `TypeExpr` is recovered from `raw_pointer_pointee_types`
    /// (keyed by the receiver's span; the lowering pass records it for every
    /// pointer-typed expression), so chained receivers
    /// (`p.offset(i).write(v)`) resolve — the inner `.offset` recurses
    /// through `compile_expr` → here and carries its own pointee entry. The
    /// `unsafe { }` requirement is enforced by the typechecker; codegen just
    /// lowers.
    fn compile_pointer_instance_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        if !matches!(
            method,
            "offset"
                | "add"
                | "read"
                | "read_unaligned"
                | "read_volatile"
                | "write"
                | "write_unaligned"
                | "write_volatile"
                | "is_null"
        ) {
            return Ok(None);
        }
        // The receiver's raw-pointer-ness is confirmed by a pointee entry (the
        // typechecker records one for every pointer method — see
        // `pointer_method_receiver_pointees`); a same-named user method on a
        // non-pointer receiver has no entry and falls through to normal dispatch.
        if !self
            .span_tables
            .raw_pointer_pointee_types
            .contains_key(&(object.span.offset, object.span.length))
        {
            return Ok(None);
        }
        // `p.is_null() -> bool` — pointee-agnostic null-bits check, so it needs no
        // pointee LLVM type and is handled before the sized-op pointee lookup.
        if method == "is_null" {
            let ptr_val = self.compile_expr(object)?.into_pointer_value();
            let is_null = self
                .builder
                .build_is_null(ptr_val, "ptr.is_null")
                .map_err(|e| format!("ptr.is_null: {e:?}"))?;
            return Ok(Some(is_null.into()));
        }
        let pointee_te = self
            .span_tables
            .raw_pointer_pointee_types
            .get(&(object.span.offset, object.span.length))
            .cloned()
            .expect("pointee entry present (checked above)");
        let pointee_llvm = self.llvm_type_for_type_expr(&pointee_te);
        let ptr_val = self.compile_expr(object)?.into_pointer_value();
        match method {
            "offset" | "add" => {
                let idx = self.compile_expr(&args[0].value)?.into_int_value();
                let ep = unsafe {
                    self.builder
                        .build_in_bounds_gep(pointee_llvm, ptr_val, &[idx], "ptr.offset")
                        .map_err(|e| format!("ptr.{method}: {e:?}"))?
                };
                Ok(Some(ep.into()))
            }
            "read" | "read_unaligned" | "read_volatile" => {
                let loaded = self
                    .builder
                    .build_load(pointee_llvm, ptr_val, "ptr.read")
                    .map_err(|e| format!("ptr.{method}: {e:?}"))?;
                let inst = loaded
                    .as_instruction_value()
                    .expect("build_load yields an instruction value");
                if method == "read_unaligned" {
                    inst.set_alignment(1)
                        .map_err(|e| format!("ptr.read_unaligned align: {e:?}"))?;
                } else if method == "read_volatile" {
                    inst.set_volatile(true)
                        .map_err(|e| format!("ptr.read_volatile: {e:?}"))?;
                }
                Ok(Some(loaded))
            }
            "write" | "write_unaligned" | "write_volatile" => {
                let v = self.compile_expr(&args[0].value)?;
                let store = self
                    .builder
                    .build_store(ptr_val, v)
                    .map_err(|e| format!("ptr.{method}: {e:?}"))?;
                if method == "write_unaligned" {
                    store
                        .set_alignment(1)
                        .map_err(|e| format!("ptr.write_unaligned align: {e:?}"))?;
                } else if method == "write_volatile" {
                    store
                        .set_volatile(true)
                        .map_err(|e| format!("ptr.write_volatile: {e:?}"))?;
                }
                // Store methods return unit (the `i64 0` void placeholder).
                Ok(Some(self.context.i64_type().const_int(0, false).into()))
            }
            _ => Ok(None),
        }
    }

    /// Codegen for the read-only method surface of a fixed-size `Array[T, N]`
    /// with a SCALAR element (`get`/`first`/`last`/`contains`/`is_empty`), over
    /// the array's stack storage. `elem0_ptr` is the element-0 address (a `T*`),
    /// `elem_ty` the LLVM element type, `n` the STATIC length from the array
    /// type. `len`/`as_ptr`/`iter` are handled by their own arms; this closes
    /// the rest of the surface the interpreter already runs (array dispatched as
    /// "Vec") so `karac build` matches `karac run`/`--interp` (B-2026-07-17-19).
    /// Mirrors the Vec `get`/`first`/`last` lowering (bounds-check → GEP → load →
    /// `Option[T]` via phis) but the length is a compile-time constant, so
    /// out-of-storage `first`/`last` on an (impossible) empty array fold to a
    /// static `None`.
    pub(super) fn compile_fixed_array_read(
        &mut self,
        elem0_ptr: inkwell::values::PointerValue<'ctx>,
        elem_ty: BasicTypeEnum<'ctx>,
        n: u64,
        method: &str,
        args: &[CallArg],
        // The element's source-level `TypeExpr`, when the binding registered
        // one. Only `is_sorted` needs it — every other arm here either does no
        // comparison at all or compares for equality, both of which are
        // signedness-blind. `is_sorted` is not: an `Array[u64, N]` element past
        // `i64::MAX` orders differently signed and unsigned, so the arm
        // declines rather than guess when this is `None`.
        elem_te: Option<TypeExpr>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        match method {
            "is_empty" => Ok(self
                .context
                .bool_type()
                .const_int((n == 0) as u64, false)
                .into()),
            // `is_sorted()` over a fixed array (B-2026-08-21-10) — the
            // `Vec`/`Slice` twin. `N` is static and small, so the pairwise
            // compares unroll instead of looping, exactly as `contains` does.
            // The per-pair comparator is the same `karac_cmp_<T>` the `Vec`
            // arm calls, which is what keeps the answer identical to the
            // interpreter's `value_compare` on unsigned elements.
            "is_sorted" => {
                let bool_t = self.context.bool_type();
                if n < 2 {
                    return Ok(bool_t.const_int(1, false).into());
                }
                let elem_te = elem_te.ok_or_else(|| {
                    "Array.is_sorted: no source element type for the receiver".to_string()
                })?;
                let cmp_fn = self.emit_cmp_fn_for_type_expr(&elem_te).ok_or_else(|| {
                    "Array.is_sorted() in codegen supports integer, char, bool, String, float, \
                     tuple, nested-Vec and derived-`Ord` struct/enum element types; add \
                     `#[derive(Ord, Eq)]` to the element type"
                        .to_string()
                })?;
                let mut acc = bool_t.const_int(1, false);
                for i in 1..n {
                    let prev_ptr = unsafe {
                        self.builder
                            .build_gep(
                                elem_ty,
                                elem0_ptr,
                                &[i64_t.const_int(i - 1, false)],
                                "arr.is.prev",
                            )
                            .map_err(|e| format!("Array.is_sorted gep: {e}"))?
                    };
                    let cur_ptr = unsafe {
                        self.builder
                            .build_gep(
                                elem_ty,
                                elem0_ptr,
                                &[i64_t.const_int(i, false)],
                                "arr.is.cur",
                            )
                            .map_err(|e| format!("Array.is_sorted gep: {e}"))?
                    };
                    let sign = self
                        .builder
                        .build_call(cmp_fn, &[prev_ptr.into(), cur_ptr.into()], "arr.is.cmp")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_int_value();
                    let ordered = self
                        .builder
                        .build_int_compare(
                            IntPredicate::SLE,
                            sign,
                            i64_t.const_zero(),
                            "arr.is.ordered",
                        )
                        .unwrap();
                    acc = self.builder.build_and(acc, ordered, "arr.is.acc").unwrap();
                }
                Ok(acc.into())
            }
            "get" | "first" | "last" => {
                // Resolve the index and whether it is statically out of storage
                // (only the degenerate empty-array `first`/`last`).
                let (idx_val, static_none) = match method {
                    "get" => {
                        if args.is_empty() {
                            return Err("Array.get requires an index argument".to_string());
                        }
                        let raw = self.compile_expr(&args[0].value)?.into_int_value();
                        // Normalize to i64 so the bounds compare is width-uniform
                        // (the typechecker types the index as i64, but be robust).
                        let idx = if raw.get_type().get_bit_width() == 64 {
                            raw
                        } else {
                            self.builder
                                .build_int_z_extend(raw, i64_t, "arr.idx.zext")
                                .unwrap()
                        };
                        (idx, false)
                    }
                    "first" => (i64_t.const_zero(), n == 0),
                    _ /* last */ => {
                        if n == 0 {
                            (i64_t.const_zero(), true)
                        } else {
                            // B-2026-08-20-39 — `last(k)` counts BACK from the
                            // end, `k` defaulting to 0, so the index is
                            // `(n - 1) - k` against the array's static length.
                            //
                            // Both out-of-range directions fall out of the
                            // UNSIGNED bounds compare below and need no test of
                            // their own: `k >= n` makes the subtraction wrap to
                            // a huge unsigned value, and `k < 0` makes the index
                            // exceed `n - 1` directly. Either way `ULT idx, n`
                            // is false and the arm yields `None`, which is the
                            // same answer the interpreter gives.
                            let last_idx = i64_t.const_int(n - 1, false);
                            match args.first() {
                                Some(a) => {
                                    let raw = self.compile_expr(&a.value)?.into_int_value();
                                    let k = if raw.get_type().get_bit_width() == 64 {
                                        raw
                                    } else {
                                        self.builder
                                            .build_int_s_extend(raw, i64_t, "arr.last.k.sext")
                                            .unwrap()
                                    };
                                    (
                                        self.builder
                                            .build_int_sub(last_idx, k, "arr.last.idx")
                                            .unwrap(),
                                        false,
                                    )
                                }
                                None => (last_idx, false),
                            }
                        }
                    }
                };
                if static_none {
                    let option_ty = self.type_decls.enum_layouts["Option"].llvm_type;
                    return Ok(option_ty.const_zero().into());
                }
                let n_val = i64_t.const_int(n, false);
                let fn_val = self.current_fn.unwrap();
                let oob_bb = self.context.append_basic_block(fn_val, "arr.get.oob");
                let valid_bb = self.context.append_basic_block(fn_val, "arr.get.valid");
                let merge_bb = self.context.append_basic_block(fn_val, "arr.get.merge");
                let in_bounds = self
                    .builder
                    .build_int_compare(IntPredicate::ULT, idx_val, n_val, "arr.in_bounds")
                    .unwrap();
                self.builder
                    .build_conditional_branch(in_bounds, valid_bb, oob_bb)
                    .unwrap();
                // Out-of-bounds → None.
                self.builder.position_at_end(oob_bb);
                self.builder.build_unconditional_branch(merge_bb).unwrap();
                // In-bounds → Some(elem[idx]).
                self.builder.position_at_end(valid_bb);
                let elem_ptr = unsafe {
                    self.builder
                        .build_gep(elem_ty, elem0_ptr, &[idx_val], "arr.elem.ptr")
                        .map_err(|e| format!("Array.{method} gep: {e}"))?
                };
                let elem_val = self
                    .builder
                    .build_load(elem_ty, elem_ptr, "arr.elem")
                    .unwrap();
                let some_words = self.coerce_to_payload_words(elem_val, 3)?;
                let valid_end = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();
                self.builder.position_at_end(merge_bb);
                Ok(self.build_option_some_via_phis(&some_words, valid_end, oob_bb, "arr.opt"))
            }
            "contains" => {
                if args.is_empty() {
                    return Err("Array.contains requires an argument".to_string());
                }
                let needle = self.compile_expr(&args[0].value)?;
                let bool_t = self.context.bool_type();
                // Unrolled OR of `elem[i] == needle` — fixed arrays are small and
                // N is static, so no runtime loop is needed. Empty array → false.
                let mut acc = bool_t.const_zero();
                for i in 0..n {
                    let idx = i64_t.const_int(i, false);
                    let elem_ptr = unsafe {
                        self.builder
                            .build_gep(elem_ty, elem0_ptr, &[idx], "arr.c.ptr")
                            .map_err(|e| format!("Array.contains gep: {e}"))?
                    };
                    let elem_val = self
                        .builder
                        .build_load(elem_ty, elem_ptr, "arr.c.elem")
                        .unwrap();
                    let eq = match (elem_val, needle) {
                        (BasicValueEnum::IntValue(e), BasicValueEnum::IntValue(x)) => self
                            .builder
                            .build_int_compare(IntPredicate::EQ, e, x, "arr.c.eq")
                            .unwrap(),
                        (BasicValueEnum::FloatValue(e), BasicValueEnum::FloatValue(x)) => self
                            .builder
                            .build_float_compare(inkwell::FloatPredicate::OEQ, e, x, "arr.c.eq")
                            .unwrap(),
                        _ => return Err("Array.contains supports scalar elements only".to_string()),
                    };
                    acc = self.builder.build_or(acc, eq, "arr.c.acc").unwrap();
                }
                Ok(acc.into())
            }
            other => Err(format!("no fixed-array read arm for '{other}'")),
        }
    }

    /// Does the RECEIVER spine of a method-call expression contain an
    /// `Iterator.rev()` step? Walks `object` links only (an argument closure's
    /// own `.rev()` is a separate scope). Used to bail codegen loudly for any
    /// chain that includes the deferred `rev` adaptor (B-2026-07-18-41).
    pub(super) fn chain_receiver_contains_rev(expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::MethodCall { object, method, .. } => {
                method == "rev" || Self::chain_receiver_contains_rev(object)
            }
            _ => false,
        }
    }

    /// Rebuild `expr`'s receiver spine with the (single) `.rev()` node removed —
    /// splicing its receiver in place (`v.iter().rev().map(f)` → `v.iter().map(f)`),
    /// preserving every surviving node's original span. The stripped chain is
    /// re-dispatched with `pending_reverse_iter` set so its base loop reverses.
    pub(super) fn strip_rev_node(expr: &Expr) -> Expr {
        match &expr.kind {
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } if method == "rev" && args.is_empty() => (**object).clone(),
            ExprKind::MethodCall {
                object,
                method,
                turbofish,
                args,
                args_close_span,
            } => Expr {
                kind: ExprKind::MethodCall {
                    object: Box::new(Self::strip_rev_node(object)),
                    method: method.clone(),
                    turbofish: turbofish.clone(),
                    args: args.clone(),
                    args_close_span: *args_close_span,
                },
                span: expr.span,
            },
            _ => expr.clone(),
        }
    }

    /// Is `expr` an `Iterator.rev()` chain that the REVERSE-ITERATE lowering can
    /// service correctly and safely (B-2026-07-18-41 codegen leg)? Requirements:
    ///   * EXACTLY one `.rev()` on the receiver spine;
    ///   * every OTHER adaptor is order-INDEPENDENT (`map`/`filter`/`inspect`) —
    ///     a positional adaptor (`enumerate`/`take`/`skip`/`step_by`/`*_while`)
    ///     combined with `rev` is NOT a reverse-iterate (`take(2).rev()` keeps
    ///     the first two then flips; reverse-iterating would keep the LAST two)
    ///     so it stays deferred;
    ///   * the base source is EITHER a BOUND `Vec` identifier's
    ///     `.iter()`/`.into_iter()` (routes through the reverse-aware
    ///     `compile_for_vec_var`) OR a BARE range `(a..b)` / `(a..=b)` directly
    ///     under the `.rev()` (routes through the descending `compile_for_range`).
    ///     Both consume the `pending_reverse_iter` flag, so restricting here
    ///     guarantees it is CONSUMED — never a silent forward iteration over an
    ///     unhandled base (temp Vec / Set / chars / adaptor-over-range).
    pub(super) fn rev_chain_reverse_iterable(&self, expr: &Expr) -> bool {
        let mut seen_rev = false;
        let mut cur = expr;
        loop {
            let ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } = &cur.kind
            else {
                return false;
            };
            match method.as_str() {
                "rev" if args.is_empty() => {
                    if seen_rev {
                        return false;
                    }
                    seen_rev = true;
                    // Bare `(a..b).rev()` / `(a..=b).rev()` — the reversal is
                    // directly over a range, serviced by the descending
                    // `compile_for_range` loop (which consumes the signal). No
                    // adaptor may sit between `rev` and the range: `map`/`filter`
                    // over a range route through the fused-chain for-loop, NOT
                    // `compile_for_range`, so they would leave the signal
                    // unconsumed → the loud bail. Only the direct-range shape
                    // qualifies here.
                    if matches!(&object.kind, ExprKind::Range { .. }) {
                        return true;
                    }
                    cur = object;
                }
                "map" | "filter" | "inspect" if args.len() == 1 => {
                    cur = object;
                }
                "iter" | "into_iter" if args.is_empty() => {
                    return seen_rev
                        && matches!(
                            &object.kind,
                            ExprKind::Identifier(n) if self.var_types.vec_elem_types.contains_key(n.as_str())
                        );
                }
                _ => return false,
            }
        }
    }

    /// `cpu.supports("avx2") -> bool` — emit a call to the runtime
    /// `karac_cpu_supports(feature_ptr, feature_len) -> i32`, returning an `i1`
    /// bool (`result != 0`). The feature name is a string literal lowered to a
    /// global constant's `{ptr, len}` (len = byte length, no NUL terminator). The
    /// runtime wraps std's cached `is_*_feature_detected!`, so this is a cheap
    /// probe; the `#[multiversion]` dispatch thunk desugars onto this intrinsic.
    fn compile_cpu_supports(&mut self, args: &[CallArg]) -> Result<BasicValueEnum<'ctx>, String> {
        let feat = match args.first().map(|a| &a.value.kind) {
            Some(ExprKind::StringLit(s)) => s.clone(),
            _ => {
                return Err("cpu.supports expects a string-literal feature name, \
                            e.g. `cpu.supports(\"avx2\")`"
                    .to_string())
            }
        };
        let i64_t = self.context.i64_type();
        let feat_ptr = self
            .builder
            .build_global_string_ptr(&feat, "cpu.feat")
            .map_err(|e| format!("cpu.supports feature-name constant failed: {e}"))?
            .as_pointer_value();
        let feat_len = i64_t.const_int(feat.len() as u64, false);
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let i32_t = self.context.i32_type();
        let fn_ty = i32_t.fn_type(&[ptr_t.into(), i64_t.into()], false);
        let f = self
            .module
            .get_function("karac_cpu_supports")
            .unwrap_or_else(|| self.module.add_function("karac_cpu_supports", fn_ty, None));
        let res = self
            .builder
            .build_call(f, &[feat_ptr.into(), feat_len.into()], "cpu.supports")
            .map_err(|e| format!("cpu.supports call failed: {e}"))?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let is_supported = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                res,
                i32_t.const_zero(),
                "cpu.supp",
            )
            .map_err(|e| format!("cpu.supports compare failed: {e}"))?;
        Ok(is_supported.into())
    }

    pub(super) fn compile_method_call(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
        // The call's closing-paren span, used ONLY to disambiguate the
        // method_unwrap_* side-table reads for CHAINED calls. Synthetic callers
        // pass `call_span` here — `method_call_key` then falls back to the
        // receiver span, preserving prior behavior. Other side-tables still key
        // on `call_span` (their inserts are unchanged); see the span-collision
        // fix, Slice 1.
        //
        // The premise this parameter was BUILT on is gone as of
        // B-2026-08-18-24 — the parser no longer sets `MethodCall.span ==
        // receiver.span`, so `call_span` is unique per chain step and no
        // METHOD chain needs a second key any more. It does NOT follow that the
        // parameter is redundant, and B-2026-08-18-30 measured why: `??` is now
        // its live client. `NilCoalesce` still copies its LHS's span, so both
        // nodes of `a ?? b ?? c` carry `a`'s, and `desugar_nil_coalesce` passes
        // the FALLBACK's span here precisely because that differs per node.
        // Collapsing `method_call_key` onto the receiver span makes the outer
        // `??` read the inner's payload type and fail the build with
        // "'unwrap_or' expected struct receiver, got IntValue" — while all
        // ~14k tests pass, which is why `chained_nil_coalesce_keeps_its_two_
        // nodes_apart` now exists.
        //
        // Retiring this parameter therefore means widening `NilCoalesce` first,
        // exactly as the five postfix arms were: producer and consumer move
        // together, never one alone.
        args_close_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Materialized iterator binding (B-2026-07-11-19): `let it =
        // <iter-chain>` recorded its chain instead of codegen'ing a runtime
        // iterator; if this call's receiver bottoms out at such a name, inline
        // the chain and re-dispatch (`it.fold(..)` → `v.iter().fold(..)`), which
        // the fused terminals/adaptors handle. Guarded on a non-empty table so
        // the common no-materialized-iter program pays nothing.
        if !self.iter_let_bindings.is_empty() {
            if let Some(sub) = self.substitute_iter_let_receiver(object) {
                // STATEFUL `next()` on a materialized iterator binding must NOT
                // be inlined: substitution rewrites `it.next()` to
                // `<chain>.next()`, and the chain-receiver first-yield arm
                // (B-2026-07-21-2) would then return the FIRST element on
                // EVERY pull — `it.next(); it.next()` silently yielding
                // element 0 twice. Bail loud instead (this shape was already a
                // loud no-handler before; keep it loud with a better message).
                if method == "next" && args.is_empty() {
                    return Err(
                        "stateful `Iterator.next()` on a materialized iterator binding \
                         (`let it = ...; it.next()`) is not supported under `karac build` \
                         (codegen has no runtime iterator value); re-run with `--interp` \
                         (or `KARAC_RUN_JIT=0`), or call `.next()` directly on the chain \
                         for a single first-element read."
                            .to_string(),
                    );
                }
                return self.compile_method_call(&sub, method, args, call_span, args_close_span);
            }
        }

        // Cooperative cancel check before each call inside a par-branch.
        // The receiver's `Type.method` key is precomputed by lowering and
        // stored in `method_callee_types`; consult it so a provably pure
        // method elides the check, mirroring the narrowing applied to
        // free-function calls in `compile_call`.
        let callee_key = self
            .span_tables
            .method_callee_types
            .get(&(call_span.offset, call_span.length))
            .cloned();
        self.emit_branch_cancel_check("mcall", callee_key.as_deref());

        // `cpu.supports("avx2") -> bool` — runtime CPU-feature probe (design.md §
        // Multiversioning; the `#[multiversion]` dispatch primitive). Recognised
        // as a namespace intrinsic only when no local binding shadows `cpu`
        // (prelude-shadow rule, mirroring `ptr.const`). Emits a call to the
        // runtime `karac_cpu_supports` with the literal feature name's `{ptr,len}`.
        if method == "supports" {
            if let ExprKind::Identifier(m) = &object.kind {
                if m == "cpu" && !self.variables.contains_key("cpu") {
                    return self.compile_cpu_supports(args);
                }
            }
        }

        // `<iter-chain>.rev()` — reverse iteration. The interpreter implements it
        // (drain-reverse-replay); codegen defers it because the forward-only
        // fused-chain lowering (`peel_fused_map_filter_chain` + the synthetic
        // `for elem in <base>` desugar) cannot express a reversal without
        // materialize-and-reverse plumbing threaded through every terminal. Bail
        // LOUD (not a silent skip / confusing generic "no handler" error) as soon
        // as `rev` appears anywhere on the receiver spine — whether this call IS
        // `.rev()` (`v.iter().rev()`) or a terminal/adaptor OVER a rev chain
        // (`v.iter().rev().collect()`, `v.iter().map(f).rev().sum()`). Only the
        // receiver spine is walked (a `.rev()` inside a closure arg is a separate
        // scope). B-2026-07-18-41 (typecheck+interp shipped; codegen deferred).
        // A terminal/adaptor OVER a rev chain (`v.iter().rev().sum()`,
        // `v.iter().map(f).rev().collect()`): if the chain is reverse-iterate
        // SAFE (order-independent steps over a bound-Vec base), strip the
        // `.rev()`, set the one-shot reverse signal, and re-dispatch the stripped
        // chain — its base for-loop then iterates `len-1-i` (B-2026-07-18-41).
        // Otherwise (positional adaptor + rev, non-Vec base, or a BARE `.rev()`
        // that has no downstream terminal to iterate) bail LOUD to `--interp`.
        if method != "rev" && Self::chain_receiver_contains_rev(object) {
            if self.rev_chain_reverse_iterable(object) {
                let stripped = Self::strip_rev_node(object);
                let saved = self.pending_reverse_iter;
                self.pending_reverse_iter = true;
                let r =
                    self.compile_method_call(&stripped, method, args, call_span, args_close_span);
                let consumed = !self.pending_reverse_iter;
                self.pending_reverse_iter = saved;
                if consumed {
                    return r;
                }
            }
            return Err(
                "`Iterator.rev()` is not yet supported under `karac build`/`karac run` \
                 (codegen) for this chain shape; it works under the tree-walk \
                 interpreter. Re-run with `--interp` (or `KARAC_RUN_JIT=0`)."
                    .to_string(),
            );
        }
        if method == "rev" && args.is_empty() {
            return Err(
                "`Iterator.rev()` is not yet supported under `karac build`/`karac run` \
                 (codegen) as a bare iterator value; chain a terminal (`.collect()` \
                 / `.sum()` / a `for` loop) or re-run with `--interp`."
                    .to_string(),
            );
        }

        // `gpu.dispatch(kernel, buffer)` (spike slice-0c). The typechecker
        // baked the kernel's WGSL into `gpu_dispatch_wgsl`; lower to a call to
        // the runtime GPU dispatch symbol with the shader constant + the input
        // buffer, wrapping the returned buffer as an owned `Vec[f32]`. Gated on
        // `gpu` not being a real local (mirrors the `process.exit` guard) so a
        // user binding named `gpu` is never hijacked.
        if method == "dispatch" {
            if let ExprKind::Identifier(name) = &object.kind {
                if name == "gpu" && !self.variables.contains_key("gpu") {
                    return self.compile_gpu_dispatch(args);
                }
            }
        }
        // `gpu.sum` / `gpu.prod` / `gpu.min` / `gpu.max` (B-2026-08-19-10,
        // extended by B-2026-08-19-13) — whole-buffer reductions, which return
        // ONE value rather than a buffer. Same `gpu`-not-a-local guard as
        // dispatch.
        if matches!(method, "sum" | "prod" | "min" | "max" | "mean") {
            if let ExprKind::Identifier(name) = &object.kind {
                if name == "gpu" && !self.variables.contains_key("gpu") {
                    return self.compile_gpu_reduce(args, method);
                }
            }
        }
        // The two-pass statistics need their own lowering: the runtime hands
        // back a sum of squares, and the divisor and the square root are
        // decided here.
        if matches!(method, "variance" | "stddev") {
            if let ExprKind::Identifier(name) = &object.kind {
                if name == "gpu" && !self.variables.contains_key("gpu") {
                    return self.compile_gpu_variance(args, call_span, method == "stddev");
                }
            }
        }
        // The prefix sum's result is a BUFFER, so it neither returns an
        // `Option` nor shares any of the reduce lowering.
        if method == "prefix_sum" {
            if let ExprKind::Identifier(name) = &object.kind {
                if name == "gpu" && !self.variables.contains_key("gpu") {
                    return self.compile_gpu_prefix_sum(args, call_span);
                }
            }
        }
        // `gpu.matmul(a, b)` takes TENSORS, not `Vec`s — it is the only op
        // here whose meaning depends on a shape — and returns one.
        if method == "matmul" {
            if let ExprKind::Identifier(name) = &object.kind {
                if name == "gpu" && !self.variables.contains_key("gpu") {
                    return self.compile_gpu_matmul(args, call_span);
                }
            }
        }
        // The Arg family reports an INDEX, so it returns `Option[i64]`
        // regardless of the element type and needs its own lowering.
        if matches!(method, "argmin" | "argmax") {
            if let ExprKind::Identifier(name) = &object.kind {
                if name == "gpu" && !self.variables.contains_key("gpu") {
                    return self.compile_gpu_arg(args, call_span);
                }
            }
        }
        // `gpu.dot(a, b)` reads TWO buffers and needs two shaders, so it has
        // its own lowering rather than a wider `compile_gpu_reduce`.
        if method == "dot" {
            if let ExprKind::Identifier(name) = &object.kind {
                if name == "gpu" && !self.variables.contains_key("gpu") {
                    return self.compile_gpu_dot(args);
                }
            }
        }
        // GPU-SLIP-4b: `gpu.upload(vec)` moves a SoA `Vec[S]` to a resident device
        // buffer, yielding a `GpuBuffer[S]` handle value `{i64 handle, i64 n}`;
        // `gpu.download(buf)` moves the handle back to a host AoS `Vec[S]`. Same
        // `gpu`-not-a-local guard as dispatch.
        if (method == "upload" || method == "download")
            && matches!(&object.kind, ExprKind::Identifier(n) if n == "gpu")
            && !self.variables.contains_key("gpu")
        {
            if method == "upload" {
                return self.compile_gpu_upload(args);
            }
            return self.compile_gpu_download(args);
        }

        // Raw-pointer instance methods (`*const T` / `*mut T`): `.offset` /
        // `.add` (arithmetic), `.read` / `.write` (+ `_unaligned` /
        // `_volatile` variants) — the inherent pointer surface from
        // design.md § raw pointers (additive-interop Slice 4, Path A;
        // B-2026-07-08-4). Gated on the receiver being raw-pointer-typed
        // (via `raw_pointer_pointee_types`), so a same-named user method on
        // a non-pointer receiver (`Reader.read()`, a builder `.write()`)
        // falls through to normal dispatch. Handles chained receivers
        // (`p.offset(i).write(v)`) — the inner `.offset` recurses here.
        if let Some(v) = self.compile_pointer_instance_method(object, method, args)? {
            return Ok(v);
        }

        // `<string>.chars()` as a STANDALONE value (e.g. `let it = s.chars()`).
        // Codegen has no first-class iterator value, so materialize the eager
        // `Vec[char]` snapshot — the faithful representation of a char-iterator
        // — by reusing the `.chars().collect()` lowering (`for c in s.chars() {
        // v.push(c) }`). This fires ONLY when `chars()` is compiled as a value:
        // `for c in s.chars()` is special-cased in the for-loop codegen (the
        // iterable never reaches here), and `s.chars().collect()` is caught by
        // the chain intercept below (its inner `chars()` is never compiled
        // standalone). The let-binding handler registers the binding as
        // `Vec[char]` so `it.collect()` / `for c in it` dispatch as Vec ops
        // (B-2026-06-18-5). `chars()` exists only on `String`, so the method
        // name alone identifies the shape.
        if method == "chars" && args.is_empty() {
            let chars_call = Expr {
                kind: ExprKind::MethodCall {
                    object: Box::new(object.clone()),
                    method: "chars".to_string(),
                    turbofish: None,
                    args: vec![],
                    args_close_span: *call_span,
                },
                span: *call_span,
            };
            return self.compile_chars_collect_to_vec(&chars_call, call_span);
        }

        // `<it>.collect()` where `it` is an identifier the codegen materialized
        // as a `Vec` (a bound `s.chars()`, B-2026-06-18-5). The eager snapshot
        // already IS the collected Vec, so return an independent copy (collect
        // yields a fresh owned Vec). `collect()` only typechecks on an
        // `Iterator`, so a Vec-typed receiver here is always such a materialized
        // iterator — never a user Vec. Placed before the identifier
        // → `compile_vec_method` dispatch, which has no `collect` arm. (The
        // `s.chars().collect()` chain, whose `collect` receiver is a `MethodCall`
        // not an identifier, is handled by the chain intercept further below.)
        if method == "collect" && args.is_empty() {
            if let ExprKind::Identifier(name) = &object.kind {
                if self.var_types.vec_elem_types.contains_key(name.as_str()) {
                    if let Some(v) = self.try_compile_clone(object)? {
                        return Ok(v);
                    }
                }
            }
        }

        // `process.exit(code: i32) -> !` — lower to libc `exit`. The typechecker
        // registers `process.exit` as a dotted free function and the interpreter
        // (eval_call.rs) handles it as a path-call, but the parser hands codegen a
        // method call with `process` as a (pseudo-variable) identifier receiver.
        // Match the interpreter's semantics: evaluate the code as i32, call libc
        // `exit` (declared `void exit(i32)` in `Codegen::new`), and terminate the
        // block with `unreachable` — the call is `Never`, so no value flows out.
        // Gated on `process` not being a real local (mirrors the ambient-resource
        // guard below) so a user binding named `process` is never hijacked.
        if method == "exit" {
            if let ExprKind::Identifier(name) = &object.kind {
                if name == "process" && !self.variables.contains_key("process") {
                    let i32_ty = self.context.i32_type();
                    // Default code is 0 (matches the interpreter's no-arg path).
                    let code = match args.first() {
                        Some(arg) => {
                            let iv = self.compile_expr(&arg.value)?.into_int_value();
                            let w = iv.get_type().get_bit_width();
                            match w.cmp(&32) {
                                std::cmp::Ordering::Greater => self
                                    .builder
                                    .build_int_truncate(iv, i32_ty, "exit.code.tr")
                                    .unwrap(),
                                std::cmp::Ordering::Less => self
                                    .builder
                                    .build_int_s_extend(iv, i32_ty, "exit.code.sx")
                                    .unwrap(),
                                std::cmp::Ordering::Equal => iv,
                            }
                        }
                        None => i32_ty.const_int(0, false),
                    };
                    let exit_fn = self
                        .module
                        .get_function("exit")
                        .expect("libc `exit` extern declared in Codegen::new");
                    self.builder
                        .build_call(exit_fn, &[code.into()], "process_exit")
                        .unwrap();
                    self.builder.build_unreachable().unwrap();
                    // Block is terminated; this placeholder is never read (every
                    // value-consuming caller respects the terminator guard).
                    return Ok(self.context.i64_type().const_int(0, false).into());
                }
            }
        }

        // Fallible-allocation instance companions (phase-8-stdlib-floor item 8).
        // Companions whose codegen lowering has landed
        // (`CODEGEN_FALLIBLE_INSTANCE_BASES`, e.g. `try_push`) fall through to
        // their dispatcher (`compile_vec_method`) and emit real fallible
        // allocation + `Result`. The remaining companions are still
        // interpreter-only; reject those at `karac build` with a clear message
        // when the receiver is a builtin collection. Gated on the collection
        // side-tables so a user type's own `try_*` method (which dispatches
        // through the qualified user-method path below) is never blocked.
        if let Some(base) = crate::fallible_alloc::instance_companion_base(method) {
            if !crate::fallible_alloc::instance_companion_has_codegen(method) {
                if let ExprKind::Identifier(name) = &object.kind {
                    let n = name.as_str();
                    let is_builtin_coll = self.var_types.vec_elem_types.contains_key(n)
                        || self.mapset.map_key_types.contains_key(n)
                        || self.mapset.set_elem_types.contains_key(n)
                        || self
                            .var_types
                            .var_type_names
                            .get(n)
                            .is_some_and(|t| t == "String" || t.starts_with("String"));
                    if is_builtin_coll {
                        return Err(format!(
                            "codegen: fallible-allocation companion `.{method}(...)` is \
                             interpreter-only in v1; its codegen lowering is phase-8-stdlib-floor \
                             item 8. Run under `karac run`, or use the panicking `.{base}(...)` \
                             base method under `karac build`."
                        ));
                    }
                }
            }
        }

        // Borrow-returning method call used outside a `let x = recv.m()`
        // binding: the result is a `ptr` (the borrow's address); any other
        // context would mishandle it as a value. The let arm sets
        // `compiling_ref_return_let_rhs` for the sanctioned site; reject
        // elsewhere rather than miscompile (sibling of the free-fn gate in
        // `compile_call`). The MethodCall expr shares the receiver's span,
        // which is the key the lowering pass used for the call's result
        // type. Direct use is a tracked follow-on (B-2026-06-07-5).
        if !self.compiling_ref_return_let_rhs
            && self.user_ref_method_names.contains(method)
            && self
                .ref_return_inner_types
                .contains_key(&(object.span.offset, object.span.length))
        {
            return Err(format!(
                "borrow-returning method call `.{method}(...)` must be bound directly with \
                 `let x = ...{method}(...)` before use; direct use of a `-> ref T` result \
                 is not yet supported (B-2026-06-07-5)"
            ));
        }

        // A method whose RECEIVER is a borrow-returning user accessor
        // (`h.view().is_empty()` where `view() -> ref Vec[i64]`). Materialize
        // the borrow into a synthetic local and re-dispatch — B-2026-07-29-12.
        //
        // Placed HERE, ahead of every arm that compiles the receiver, and not
        // further down: a later arm emits the receiver and then falls through,
        // so this helper's own `compile_expr(object)` produced a SECOND call to
        // the accessor with the first result discarded (`%usermethod = call ptr
        // @H.view(...)` twice in the emitted IR — B-2026-07-29-15). Harmless
        // for a pure borrow accessor, but wrong for one with side effects, and
        // for an allocating accessor the discarded result is leaked. The helper
        // self-gates to a borrow-returning user call, so running it first
        // changes nothing else.
        if let Some(result) = self.try_compile_ref_return_receiver_method(object, method, args)? {
            return Ok(result);
        }

        // Chained-call span collision guard. The parser sets
        // `MethodCall.span == receiver.span`, so in `recv.inner().outer()`
        // the inner and outer calls share one `method_callee_types` key, and
        // it resolves to the *inner* call's `Type.method` (the effect-checker
        // relies on that — see the unwrap-family skip in
        // `typechecker/expr_method_call.rs`). For DISPATCH below we must not
        // let the inner key drive the outer call: e.g. compiling the `unwrap`
        // of `listener.accept().unwrap()` sees `key == "TcpListener.accept"`
        // and would re-lower `accept` on its own result (a double-lowering +
        // type mismatch). Require the key's method segment to match THIS
        // call's `method` before using it to pick a builtin / state-machine
        // lowering; the conservative cancel-check above keeps the raw key.
        let dispatch_key = callee_key
            .as_ref()
            .filter(|k| {
                k.rsplit_once('.')
                    .map(|(_, m)| m == method)
                    .unwrap_or(false)
            })
            .cloned();

        // B-2026-07-18-48: a NON-identifier receiver (struct/enum literal, call
        // result) whose typechecker-resolved `Type.method` names a USER impl
        // method must dispatch to that method BEFORE the builtin Vec/String
        // routing below. A single-heap-field struct (`struct R { v: String }`)
        // shares String's `{ptr,len,cap}` LLVM shape, so for a literal receiver
        // — whose user type isn't in `var_type_names` — a same-named builtin
        // method (`get`/`take`/`unwrap`/…) hijacked the call via shape detection
        // (`R { v: "x" }.get()` → "Vec.get requires an index argument"; interp
        // resolved `R.get`). `try_compile_freshtemp_user_method` self-gates
        // (returns `Ok(None)` unless the receiver is a non-identifier fresh-temp
        // user struct/enum whose `dispatch_key` `Type.method` exists), so this is
        // a no-op for identifier/self receivers and for genuine builtin calls;
        // when it fires it materializes the receiver and re-dispatches through
        // the identifier path (which resolves the user method). The identical
        // call remains as a fallback further below for any path that reaches it.
        //
        // Gated to receivers that are true TEMPORARIES (a struct/enum literal or
        // a call result) — NOT a PLACE expression (`v[i]`, `s.field`, `t.0`).
        // Materializing a place into a temp would break write-back for a
        // `mut ref self` method (`v[0].bump()` must mutate the element in place,
        // not a copy — the indexed-receiver / field-receiver specialized paths
        // below handle those and MUST keep priority). Place receivers still reach
        // the identical fallback call further down after those paths, unchanged.
        if !matches!(
            &object.kind,
            ExprKind::Index { .. } | ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. }
        ) {
            if let Some(result) = self.try_compile_freshtemp_user_method(
                object,
                method,
                args,
                dispatch_key.as_deref(),
                call_span,
            )? {
                return Ok(result);
            }
        }

        // Distinct-type `.raw()` unwrap (design.md § Distinct Types). A
        // distinct type is a zero-cost wrapper — its compiled value already
        // IS the base value (layout-identical), so `.raw()` returns the
        // compiled receiver unchanged. `.raw()` is reserved to distinct types
        // by the typechecker, so a zero-arg `.raw()` reaching codegen is
        // always this unwrap.
        if method == "raw" && args.is_empty() {
            return self.compile_expr(object);
        }

        // `re.is_match(s: String) -> bool` on a `Regex { pattern }` receiver
        // (B-2026-07-14-19) — the AOT backend for regex.kara's
        // `#[compiler_builtin]` stub. Extract the receiver's pattern String and
        // the subject String, call the runtime `karac_regex_is_match` (which
        // re-compiles `pattern` per call, matching the interpreter). Gated on
        // the receiver's static type being `Regex`, so a same-named user method
        // never routes here. `find` / `find_all` / `replace_all` are the
        // slice-2 siblings just below.
        if method == "is_match"
            && args.len() == 1
            && self.type_name_of_expr(object).as_deref() == Some("Regex")
        {
            // B-2026-08-02-13 — a user struct shadowing `Regex` / `RegexError`
            // / `Match` takes over that stdlib type's codegen identity, and the
            // mismatch does NOT degrade gracefully here: the receiver arrives
            // as an i64 rather than the seeded struct and the coercion below
            // panicked with a Rust backtrace ("expected the StructValue
            // variant"), which is the one outcome the coding standard rules
            // out. Refuse with the rename instead.
            self.reject_shadowed_prelude_types(
                &format!("Regex.{method}"),
                &["Regex", "RegexError", "Match"],
            )?;
            let recv = self.compile_expr(object)?;
            let recv_sv = self.require_struct_value(recv, &format!("Regex.{method}"))?;
            let (pat_data, pat_len) = self.regex_pattern_data_len(recv_sv);
            let s_val = self.compile_expr(&args[0].value)?;
            let s_sv = self.require_struct_value(s_val, &format!("Regex.{method} subject"))?;
            let (s_data, s_len) = self.str_data_len(s_sv);

            let is_match_fn = self
                .module
                .get_function("karac_regex_is_match")
                .expect("karac_regex_is_match declared in Codegen::new");
            let res = self
                .builder
                .build_call(
                    is_match_fn,
                    &[pat_data.into(), pat_len.into(), s_data.into(), s_len.into()],
                    "rx.is_match",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            // u8 (0/1) → Kāra `bool` (i1).
            let bool_val = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::NE,
                    res,
                    self.context.i8_type().const_zero(),
                    "rx.is_match.bool",
                )
                .unwrap();
            return Ok(bool_val.into());
        }

        // `re.find(s) -> Option[Match]` / `re.find_all(s) -> Vec[Match]`
        // (B-2026-07-14-19 slice 2) — the AOT backend for regex.kara's
        // remaining `#[compiler_builtin]` stubs. Each recompiles the receiver's
        // pattern per call (matching the interpreter) through a `karac_regex_*`
        // entrypoint that returns primitive byte offsets; codegen owns all
        // `Match` / `Vec` / `String` layout and slices the subject for each
        // `Match.text` via `build_owned_string_from_parts` (a fresh owned copy,
        // so the text never aliases the soon-dropped subject buffer).
        if (method == "find" || method == "find_all")
            && args.len() == 1
            && self.type_name_of_expr(object).as_deref() == Some("Regex")
        {
            // B-2026-08-02-13 — a user struct shadowing `Regex` / `RegexError`
            // / `Match` takes over that stdlib type's codegen identity, and the
            // mismatch does NOT degrade gracefully here: the receiver arrives
            // as an i64 rather than the seeded struct and the coercion below
            // panicked with a Rust backtrace ("expected the StructValue
            // variant"), which is the one outcome the coding standard rules
            // out. Refuse with the rename instead.
            self.reject_shadowed_prelude_types(
                &format!("Regex.{method}"),
                &["Regex", "RegexError", "Match"],
            )?;
            let recv = self.compile_expr(object)?;
            let recv_sv = self.require_struct_value(recv, &format!("Regex.{method}"))?;
            let (pat_data, pat_len) = self.regex_pattern_data_len(recv_sv);
            let s_val = self.compile_expr(&args[0].value)?;
            let s_sv = self.require_struct_value(s_val, &format!("Regex.{method} subject"))?;
            let (s_data, s_len) = self.str_data_len(s_sv);
            let i64_t = self.context.i64_type();
            let fn_val = self.current_fn.unwrap();

            if method == "find" {
                let start_slot = self.create_entry_alloca(fn_val, "rx.find.start", i64_t.into());
                let end_slot = self.create_entry_alloca(fn_val, "rx.find.end", i64_t.into());
                let find_fn = self
                    .module
                    .get_function("karac_regex_find")
                    .expect("karac_regex_find declared in Codegen::new");
                let res = self
                    .builder
                    .build_call(
                        find_fn,
                        &[
                            pat_data.into(),
                            pat_len.into(),
                            s_data.into(),
                            s_len.into(),
                            start_slot.into(),
                            end_slot.into(),
                        ],
                        "rx.find",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let found = self
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        res,
                        self.context.i8_type().const_zero(),
                        "rx.find.found",
                    )
                    .unwrap();
                let some_bb = self.context.append_basic_block(fn_val, "rx.find.some");
                let none_bb = self.context.append_basic_block(fn_val, "rx.find.none");
                let merge_bb = self.context.append_basic_block(fn_val, "rx.find.merge");
                self.builder
                    .build_conditional_branch(found, some_bb, none_bb)
                    .unwrap();

                // Some(Match { text: s[start..end], start, end }).
                self.builder.position_at_end(some_bb);
                let start_iv = self
                    .builder
                    .build_load(i64_t, start_slot, "rx.find.start.v")
                    .unwrap()
                    .into_int_value();
                let end_iv = self
                    .builder
                    .build_load(i64_t, end_slot, "rx.find.end.v")
                    .unwrap()
                    .into_int_value();
                let text = self.build_regex_match_text(s_data, start_iv, end_iv);
                let m = self.build_match_struct(text, start_iv, end_iv)?;
                let some_val = self.build_nonshared_enum_value("Option", "Some", &[m])?;
                let some_end = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // None.
                self.builder.position_at_end(none_bb);
                let none_val = self.build_nonshared_enum_value("Option", "None", &[])?;
                let none_end = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                self.builder.position_at_end(merge_bb);
                let phi = self
                    .builder
                    .build_phi(some_val.get_type(), "rx.find.result")
                    .unwrap();
                phi.add_incoming(&[(&some_val, some_end), (&none_val, none_end)]);
                return Ok(phi.as_basic_value());
            }

            // find_all -> Vec[Match]. The runtime hands back a malloc'd
            // `[start0,end0,…]` offset array (or null when empty) plus a count;
            // codegen builds each `Match` into a fresh `Vec` buffer, then frees
            // the offset array (`free(null)` is a no-op for the empty case).
            let match_ty = self
                .type_decls
                .struct_types
                .get("Match")
                .copied()
                .ok_or_else(|| {
                    "codegen: Regex.find_all needs the `Match` struct layout \
                 (regex.kara not registered in compiled_stdlib_programs)"
                        .to_string()
                })?;
            let count_slot = self.create_entry_alloca(fn_val, "rx.fa.count", i64_t.into());
            self.builder
                .build_store(count_slot, i64_t.const_zero())
                .unwrap();
            let fa_fn = self
                .module
                .get_function("karac_regex_find_all")
                .expect("karac_regex_find_all declared in Codegen::new");
            let arr = self
                .builder
                .build_call(
                    fa_fn,
                    &[
                        pat_data.into(),
                        pat_len.into(),
                        s_data.into(),
                        s_len.into(),
                        count_slot.into(),
                    ],
                    "rx.fa.arr",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let count = self
                .builder
                .build_load(i64_t, count_slot, "rx.fa.count.v")
                .unwrap()
                .into_int_value();

            // buf = alloc(count * sizeof(Match)).
            let match_size = match_ty.size_of().unwrap();
            let alloc_bytes = self
                .builder
                .build_int_mul(count, match_size, "rx.fa.bytes")
                .unwrap();
            let buf = self
                .builder
                .build_call(
                    self.runtime_fns.alloc_or_panic_fn,
                    &[alloc_bytes.into()],
                    "rx.fa.buf",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();

            // for i in 0..count { buf[i] = Match { s[arr[2i]..arr[2i+1]], .. } }
            let counter = self.create_entry_alloca(fn_val, "rx.fa.i", i64_t.into());
            self.builder
                .build_store(counter, i64_t.const_zero())
                .unwrap();
            let cond_bb = self.context.append_basic_block(fn_val, "rx.fa.cond");
            let body_bb = self.context.append_basic_block(fn_val, "rx.fa.body");
            let exit_bb = self.context.append_basic_block(fn_val, "rx.fa.exit");
            self.builder.build_unconditional_branch(cond_bb).unwrap();

            self.builder.position_at_end(cond_bb);
            let cur = self
                .builder
                .build_load(i64_t, counter, "rx.fa.cur")
                .unwrap()
                .into_int_value();
            let cond = self
                .builder
                .build_int_compare(IntPredicate::ULT, cur, count, "rx.fa.lt")
                .unwrap();
            self.builder
                .build_conditional_branch(cond, body_bb, exit_bb)
                .unwrap();

            self.builder.position_at_end(body_bb);
            let two = i64_t.const_int(2, false);
            let one = i64_t.const_int(1, false);
            let base = self.builder.build_int_mul(cur, two, "rx.fa.base").unwrap();
            let end_idx = self
                .builder
                .build_int_add(base, one, "rx.fa.end.idx")
                .unwrap();
            let start_ptr = unsafe {
                self.builder
                    .build_gep(i64_t, arr, &[base], "rx.fa.start.ptr")
                    .unwrap()
            };
            let end_ptr = unsafe {
                self.builder
                    .build_gep(i64_t, arr, &[end_idx], "rx.fa.end.ptr")
                    .unwrap()
            };
            let start_iv = self
                .builder
                .build_load(i64_t, start_ptr, "rx.fa.start.v")
                .unwrap()
                .into_int_value();
            let end_iv = self
                .builder
                .build_load(i64_t, end_ptr, "rx.fa.end.v")
                .unwrap()
                .into_int_value();
            let text = self.build_regex_match_text(s_data, start_iv, end_iv);
            let m = self.build_match_struct(text, start_iv, end_iv)?;
            let dst = unsafe {
                self.builder
                    .build_gep(match_ty, buf, &[cur], "rx.fa.dst")
                    .unwrap()
            };
            self.builder.build_store(dst, m).unwrap();
            let next = self.builder.build_int_add(cur, one, "rx.fa.next").unwrap();
            self.builder.build_store(counter, next).unwrap();
            self.builder.build_unconditional_branch(cond_bb).unwrap();

            self.builder.position_at_end(exit_bb);
            self.builder
                .build_call(self.runtime_fns.free_fn, &[arr.into()], "")
                .unwrap();
            return Ok(self.build_vec_value(buf, count, count));
        }

        // `re.replace_all(s, repl) -> String` (B-2026-07-14-19 slice 2). The
        // runtime returns a fresh malloc'd result buffer + byte length; codegen
        // adopts it as an owned `String` (`cap = max(len, 1) > 0`) so the
        // scope-exit `free` matches the runtime's allocator.
        if method == "replace_all"
            && args.len() == 2
            && self.type_name_of_expr(object).as_deref() == Some("Regex")
        {
            // B-2026-08-02-13 — a user struct shadowing `Regex` / `RegexError`
            // / `Match` takes over that stdlib type's codegen identity, and the
            // mismatch does NOT degrade gracefully here: the receiver arrives
            // as an i64 rather than the seeded struct and the coercion below
            // panicked with a Rust backtrace ("expected the StructValue
            // variant"), which is the one outcome the coding standard rules
            // out. Refuse with the rename instead.
            self.reject_shadowed_prelude_types(
                &format!("Regex.{method}"),
                &["Regex", "RegexError", "Match"],
            )?;
            let recv = self.compile_expr(object)?;
            let recv_sv = self.require_struct_value(recv, &format!("Regex.{method}"))?;
            let (pat_data, pat_len) = self.regex_pattern_data_len(recv_sv);
            let s_val = self.compile_expr(&args[0].value)?;
            let s_sv = self.require_struct_value(s_val, &format!("Regex.{method} subject"))?;
            let (s_data, s_len) = self.str_data_len(s_sv);
            let r_val = self.compile_expr(&args[1].value)?;
            let r_sv = self.require_struct_value(r_val, &format!("Regex.{method} replacement"))?;
            let (r_data, r_len) = self.str_data_len(r_sv);
            let i64_t = self.context.i64_type();
            let fn_val = self.current_fn.unwrap();

            let len_slot = self.create_entry_alloca(fn_val, "rx.ra.len", i64_t.into());
            self.builder
                .build_store(len_slot, i64_t.const_zero())
                .unwrap();
            let ra_fn = self
                .module
                .get_function("karac_regex_replace_all")
                .expect("karac_regex_replace_all declared in Codegen::new");
            let ptr = self
                .builder
                .build_call(
                    ra_fn,
                    &[
                        pat_data.into(),
                        pat_len.into(),
                        s_data.into(),
                        s_len.into(),
                        r_data.into(),
                        r_len.into(),
                        len_slot.into(),
                    ],
                    "rx.ra.ptr",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let len = self
                .builder
                .build_load(i64_t, len_slot, "rx.ra.len.v")
                .unwrap()
                .into_int_value();
            // cap = max(len, 1) — the runtime always allocated max(len,1) bytes.
            let one = i64_t.const_int(1, false);
            let len_pos = self
                .builder
                .build_int_compare(IntPredicate::UGT, len, i64_t.const_zero(), "rx.ra.pos")
                .unwrap();
            let cap = self
                .builder
                .build_select(len_pos, len, one, "rx.ra.cap")
                .unwrap()
                .into_int_value();
            return Ok(self.build_vec_value(ptr, len, cap));
        }

        // `std.process` builtins — `Command.spawn`, the `Child` family,
        // and the captured-pipe handle methods (`src/codegen/process.rs`,
        // phase-8 P1). Self-gating on the receiver's static type name
        // (Command / Child / ChildStdout / ChildStderr / ChildStdin), so
        // the same-named builder methods (`Command.stdout(cfg)`) and
        // unrelated `write` / `close` / `read_to_string` methods fall
        // through to their own dispatchers.
        if let Some(v) = self.try_compile_process_method(object, method, args)? {
            return Ok(v);
        }

        // Backpressure primitives — `Semaphore.acquire`/`.release` and
        // `RateLimiter.try_acquire` (`src/codegen/backpressure.rs`, phase-8).
        // Self-gating on the receiver's static type name, so an unrelated
        // `acquire` / `release` / `try_acquire` method falls through.
        if let Some(v) = self.try_compile_backpressure_method(object, method, args)? {
            return Ok(v);
        }

        // `Pool.acquire` / `Pool.release` (`src/codegen/pool.rs`, phase-8).
        // Self-gating on the receiver's static type name.
        if let Some(v) = self.try_compile_pool_method(object, method, args, call_span)? {
            return Ok(v);
        }

        // Tensor shape-transform family (`reshape` / `permute` / `slice`
        // / `squeeze`, phase-11 numerical stdlib — `src/codegen/tensor.rs`).
        // Handled here (before the rest of dispatch) so both identifier
        // and chained / value receivers route uniformly; returns `None`
        // when the method isn't a transform or the receiver isn't a
        // statically-ranked tensor. `iter_axis` is a separate follow-on
        // slice and is NOT handled here (it errors in the identifier
        // block below).
        if let Some(v) = self.try_compile_tensor_transform(object, method, args, call_span)? {
            return Ok(v);
        }

        // LazyFrame / LazyExpr codegen twin (phase-11 LazyDataFrame,
        // `src/codegen/lazyframe.rs`): the plan builders (`select` /
        // `limit` / `filter`), `collect` / `explain`, and the LazyExpr
        // predicate surface, keyed off the receiver's STATIC Lazy type
        // (recursive classifier — span-collision-immune for chains).
        // `None` for non-Lazy receivers; unsupported Lazy methods (sort /
        // group_by / join / with_columns / the aggregates) bail loudly
        // inside with a `karac run` pointer.
        if let Some(v) = self.try_compile_lazy_method(object, method, args, call_span)? {
            return Ok(v);
        }

        // Column instance methods (`push` / `push_null` / `len` /
        // `null_count` / `valid_count` / `is_null`, phase-11 data-science
        // stdlib — `src/codegen/column.rs`). Identifier receiver only
        // (gated on `column_var_infos`, span-collision-immune); returns
        // `None` when the receiver isn't a column or the method isn't one
        // of ours. The Vec-returning transforms (`iter` / `iter_valid` /
        // `fillna` / `dropna`) are a follow-on slice and stay on
        // `karac run`.
        if let Some(v) = self.try_compile_column_method(object, method, args, call_span)? {
            return Ok(v);
        }
        // DataFrame methods (`insert` / `column` / `has_column` / `width`
        // / `height`) — gated on `dataframe_var_infos` (identifier
        // receiver). `None` for a non-DataFrame receiver. See
        // `src/codegen/dataframe.rs`.
        if let Some(v) = self.try_compile_dataframe_method(object, method, args, call_span)? {
            return Ok(v);
        }

        // Tensor reductions — `sum`/`mean`/`prod`/`min`/`max` (→ scalar) and
        // `sum_axis`/`mean_axis` (→ rank-1-lower tensor), phase-11 line 47
        // Slice B. Handled here so identifier / chained / value receivers
        // route uniformly; `None` when the method isn't a reduce or the
        // receiver isn't a tensor.
        if let Some(v) = self.try_compile_tensor_reduce(object, method, args, call_span)? {
            return Ok(v);
        }

        // Tensor broadcasting — `broadcast_add`/`broadcast_sub`/`broadcast_mul`
        // /`broadcast_div` apply an element-wise op with NumPy-style shape
        // broadcasting (size-1 dims expand; shapes align from the right).
        // Identifier receiver only (like reductions; span-collision-immune);
        // `None` for a value / chained receiver (bind to a `let` first) or a
        // non-tensor receiver. `src/codegen/tensor.rs`.
        if let Some(v) = self.try_compile_tensor_broadcast(object, method, args, call_span)? {
            return Ok(v);
        }

        // SIMD static constructor — `Vector[T, N].splat(x)` (design.md
        // § Portable SIMD). The receiver is the bare vector type-path, not a
        // value, so intercept before the receiver is compiled as an
        // expression. Broadcast the scalar across all `N` lanes.
        if method == "splat"
            || method == "from_array"
            || method == "from_slice"
            || method == "load_masked"
            || method == "gather"
            || method == "cast_from"
        {
            if let ExprKind::Path {
                segments,
                generic_args: Some(ga),
            } = &object.kind
            {
                if segments.len() == 1 && segments[0] == "Vector" {
                    return match method {
                        "splat" => self.compile_vector_splat(ga, args),
                        "from_array" => self.compile_vector_from_array(ga, args),
                        "load_masked" => self.compile_vector_load_masked(ga, args),
                        "gather" => self.compile_vector_gather(ga, args),
                        "cast_from" => self.compile_vector_cast_from(ga, args),
                        _ => self.compile_vector_from_slice(ga, args),
                    };
                }
            }
        }

        // `Vector[T, N]` instance methods (design.md § Portable SIMD, slice 2):
        // the two core Vector→scalar reductions. The receiver compiles to an
        // `<N x T>` VectorValue; reductions fold via extractelement + scalar
        // binop (LLVM re-vectorizes where profitable). dispatch_key is
        // `"Vector.<method>"` from `method_callee_type_name`.
        // B-2026-07-29-7: `dispatch_key` alone is not enough in a CHAIN. The
        // parser sets `MethodCall.span == receiver.span`, so in
        // `v.reduce_sum().to_string()` the outer link's `f32.to_string` insert
        // clobbers the inner `Vector.reduce_sum` at the shared key — and the
        // method-segment guard above then (correctly) refuses to let
        // `to_string` drive the inner call, leaving it with NO key. The vector
        // dispatch fell through and the call died in the catch-all with "no
        // handler for method 'reduce_sum'". Only the unwrap-family is exempted
        // from that clobber upstream, and widening that exemption per outer
        // method name is whack-a-mole.
        //
        // `vector_method_call_spans` is immune to the collision: only the
        // VECTOR call writes it (`to_string` on the f32 result records
        // nothing), so a hit at this span means a vector instance-method call
        // lives here. Requiring the method name to also be in the set below
        // keeps it exact — an outer link with a name from that set would have
        // its own vector-typed receiver anyway.
        let vector_span_hit = self
            .span_tables
            .vector_method_call_spans
            .contains(&(object.span.offset, object.span.length));
        let vector_key = dispatch_key
            .clone()
            .or_else(|| vector_span_hit.then(|| format!("Vector.{method}")));
        if let Some(ref key) = vector_key {
            if matches!(
                key.as_str(),
                "Vector.dot"
                    | "Vector.cross"
                    | "Vector.reduce_sum"
                    | "Vector.reduce_product"
                    | "Vector.reduce_min"
                    | "Vector.reduce_max"
                    | "Vector.reduce_and"
                    | "Vector.reduce_or"
                    | "Vector.reduce_xor"
                    | "Vector.select"
                    | "Vector.reverse"
                    | "Vector.rotate_lanes_left"
                    | "Vector.rotate_lanes_right"
                    | "Vector.replace"
                    | "Vector.shuffle"
                    | "Vector.store_masked"
                    | "Vector.scatter"
                    // std.simd.math transcendentals + rounding (phase-11 numerical stdlib)
                    | "Vector.sqrt"
                    | "Vector.exp"
                    | "Vector.ln"
                    | "Vector.tanh"
                    | "Vector.sigmoid"
                    | "Vector.floor"
                    | "Vector.ceil"
                    | "Vector.round"
                    | "Vector.trunc"
                    | "Vector.to_bits"
                    | "Vector.bits_as_f32"
                    | "Vector.bits_as_f64"
            ) {
                return self.compile_vector_method(object, method, args);
            }
        }

        // `CStr` method dispatch (design.md § C-String Literals). The
        // receiver compiles to the `{ptr, i64}` slice-struct the
        // CStringLit lowering produces (see `compile_expr`); every method
        // is an extract/compare on that aggregate, so one helper serves
        // literal, local-binding, and call-result receivers alike. Keyed
        // off the typechecker-recorded `CStr.<method>` (the same pattern
        // as the Vector arm above) — `cstr_vars` exists for *binding*
        // registration heuristics, not dispatch.
        if let Some(ref key) = dispatch_key {
            if matches!(
                key.as_str(),
                "CStr.as_ptr" | "CStr.len" | "CStr.is_empty" | "CStr.as_bytes"
            ) {
                return self.compile_cstr_method(object, method);
            }
            // `CStr.to_string() -> Result[String, Utf8Error]` — the UTF-8-
            // validating read of a C string (FFI/host-fn `char*` boundary).
            // Unlike the borrowed-surface methods above, it allocates a heap
            // String and builds a Result enum, so it has its own helper.
            if key.as_str() == "CStr.to_string" {
                return self.compile_cstr_to_string(object);
            }
            // `CStr.to_string_slice() -> Result[StringSlice, Utf8Error]` — the
            // zero-copy sibling: validates UTF-8 but returns a borrowed
            // `{ptr, len, cap=0}` view over the receiver's bytes instead of an
            // owning heap copy.
            if key.as_str() == "CStr.to_string_slice" {
                return self.compile_cstr_to_string_slice(object);
            }
            // `CString` method dispatch (design.md § C-String Literals, owning
            // form). The receiver compiles to the `{ptr, len, cap}` String-shaped
            // aggregate `to_cstring` built; `as_ptr` / `len` / `is_empty` extract
            // fields 0/1 exactly like `CStr`, but `as_bytes` must rebuild a 2-word
            // `Slice[u8]` from ptr+len (the receiver is 3 words, not a slice), so
            // `CString` gets its own helper.
            if matches!(
                key.as_str(),
                "CString.as_ptr" | "CString.len" | "CString.is_empty" | "CString.as_bytes"
            ) {
                return self.compile_cstring_method(object, method);
            }
            // `String.to_cstring() -> Result[CString, NulError]` — the outbound
            // conversion (copy + trailing NUL, interior-NUL reject). Keyed off the
            // typechecker-recorded `String.to_cstring` so a user type's own
            // `to_cstring` method (resolved through the impl path) is never
            // hijacked.
            if key.as_str() == "String.to_cstring" {
                return self.compile_string_to_cstring(object);
            }
        }

        // Phase 6 line 17 — stdlib `TcpListener` / `TcpStream`
        // compiler-builtin dispatch. Routes through the lowerings in
        // `src/codegen/tcp.rs`, each of which composes a
        // `karac_park_on_fd(self.fd, direction)` state-machine
        // invocation with a raw-syscall FFI call. Runs ahead of the
        // state-machine intercept below so the compiler-builtin shape
        // takes precedence over the generic network-boundary lowering
        // (the baked stdlib's bodies are stubs — without these arms,
        // the generic dispatch would emit a call into a non-existent
        // symbol).
        if let Some(ref key) = dispatch_key {
            if key == "TcpListener.accept" {
                let self_val = self.compile_expr(object)?;
                return self.lower_tcp_listener_accept(self_val);
            }
            // Phase 8 `File` handle slice F4: instance method
            // dispatch. `file.read(buf: mut Slice[u8])` /
            // `file.write(buf: Slice[u8])` / `file.flush()` lower
            // through `karac_runtime_file_*` externs; the
            // KaracIoResult return unpacks into `Result[usize/Unit,
            // IoError]` via `Codegen::lower_kara_io_result`. The
            // receiver `self_val` is the `File` opaque pointer (per
            // F3's `File` → opaque ptr lowering).
            if key == "File.read" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.compile_file_read(self_val, buf_val);
            }
            if key == "File.write" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.compile_file_write(self_val, buf_val);
            }
            if key == "File.flush" && args.is_empty() {
                let self_val = self.compile_expr(object)?;
                return self.compile_file_flush(self_val);
            }
            // B-2026-08-10-3 — `file.seek(whence: SeekFrom, offset: i64)`.
            if key == "File.seek" && args.len() == 2 {
                let self_val = self.compile_expr(object)?;
                let whence_val = self.compile_expr(&args[0].value)?;
                let offset_val = self.compile_expr(&args[1].value)?;
                return self.compile_file_seek(self_val, whence_val, offset_val);
            }
            if (key == "File.sync_all" || key == "File.sync_data") && args.is_empty() {
                let self_val = self.compile_expr(object)?;
                return self.compile_file_sync(self_val, key == "File.sync_data");
            }
            if key == "TcpStream.read" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_tcp_stream_read(self_val, buf_val);
            }
            if key == "TcpStream.write" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_tcp_stream_write(self_val, buf_val);
            }
            if key == "TcpStream.write_all" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_tcp_stream_write_all(self_val, buf_val);
            }
            if key == "TcpStream.try_clone" && args.is_empty() {
                // `dup(2)` the socket into a second owned handle — splits a
                // connection into read-half + write-half for a full-duplex
                // splice. Dispatched here (before the generic Vec/String
                // `try_clone` deep-copy arm) so TcpStream gets the fd-dup
                // lowering, not the buffer-clone one.
                let self_val = self.compile_expr(object)?;
                return self.lower_tcp_stream_try_clone(self_val);
            }
            if key == "TcpStream.shutdown_write" && args.is_empty() {
                // Half-close the write side (`shutdown(SHUT_WR)`) — sends a
                // FIN so a proxy can propagate one direction's EOF across a
                // full-duplex splice.
                let self_val = self.compile_expr(object)?;
                return self.lower_tcp_stream_shutdown_write(self_val);
            }
            // Phase 6 line 236 slice 2 — TLS-side method dispatch. Same
            // shape as the TCP dispatch above; lowerings in
            // `src/codegen/tls.rs` route through `karac_runtime_tls_*`.
            if key == "TlsListener.accept" {
                let self_val = self.compile_expr(object)?;
                return self.lower_tls_listener_accept(self_val);
            }
            if key == "TlsStream.read" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_tls_stream_read(self_val, buf_val);
            }
            if key == "TlsStream.write" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_tls_stream_write(self_val, buf_val);
            }
            if key == "TlsStream.write_all" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_tls_stream_write_all(self_val, buf_val);
            }
            // Phase 6 line 17 slice 9e.1 — stdlib `WebSocket` dispatch.
            // Same compose-at-leaf shape as TcpStream above:
            // `karac_park_on_fd(self.fd, direction)` then the encode +
            // write or read + decode FFI. The runtime FFIs
            // (`karac_runtime_ws_send_text` / `_recv_text`) handle the
            // RFC 6455 framing details.
            if key == "WebSocket.send_text" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_websocket_send_text(self_val, buf_val);
            }
            if key == "WebSocket.recv_text" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_websocket_recv_text(self_val, buf_val);
            }
            // Phase 6 line 17 slice 9e.3 — binary frame send/recv.
            // Mirror of send_text / recv_text but routes through
            // the binary-opcode FFIs.
            if key == "WebSocket.send_binary" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_websocket_send_binary(self_val, buf_val);
            }
            if key == "WebSocket.recv_binary" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_websocket_recv_binary(self_val, buf_val);
            }
            // Phase 6 line 17 slice 9e.4 — client-side masked send
            // for kara binaries acting as WebSocket clients
            // (RFC 6455 §5.1 client→server frames require MASK=1).
            if key == "WebSocket.send_text_masked" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_websocket_send_text_masked(self_val, buf_val);
            }
            if key == "WebSocket.send_binary_masked" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                let elem_ty: BasicTypeEnum = self.context.i8_type().into();
                let buf_val = match self.coerce_to_slice(&args[0].value, elem_ty)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                return self.lower_websocket_send_binary_masked(self_val, buf_val);
            }
            // Phase 6 line 218 slice 5: `tg.spawn(closure)` — synthesize
            // the SpawnFn wrapper + malloc/populate env + call
            // karac_runtime_spawn (same path as free `spawn`), then
            // register the returned handle with the TaskGroup so the
            // group's drop can wait for the child. The receiver carries
            // the runtime-side group pointer in its `i64 id` field
            // (`TaskGroup.new()` lowers to ptrtoint of a Box<KaracTaskGroupHandle>).
            if key == "TaskGroup.spawn" && args.len() == 1 {
                let self_val = self.compile_expr(object)?;
                return self.lower_taskgroup_spawn(self_val, &args[0].value);
            }
            // A2 slice 5b-1: `tg.cancel()` — flip every registered child's
            // per-task cancel flag via karac_runtime_taskgroup_cancel. Inert
            // until the dispatcher routes the flag to parked coroutines
            // (slice 5c). Returns unit.
            if key == "TaskGroup.cancel" && args.is_empty() {
                let self_val = self.compile_expr(object)?;
                return self.lower_taskgroup_cancel(self_val);
            }
            // Phase 6 line 218 slice 4: `h.join()` dispatch. Lowers to
            // `karac_runtime_task_join(handle, &out_slot)` then reads
            // T from the slot. The return type T is recovered from the
            // enclosing function's `let v: T = h.join()` annotation
            // (typechecker doesn't bind T from receiver for the
            // `impl[T] T<T> { fn m(self) -> T }` shape today — see slice
            // 1's surfaced typechecker gap). Falls back to i64 when no
            // annotation is recoverable.
            if key == "TaskHandle.join" && args.is_empty() {
                let self_val = self.compile_expr(object)?;
                let return_ty = self.recover_task_handle_join_return_ty(call_span);
                return self.lower_task_handle_join(self_val, return_ty);
            }
            // `BoundedChannel.send` / `.recv` (`src/codegen/bounded_channel.rs`).
            // Routed here off the `dispatch_key` the typechecker's
            // `infer_bounded_channel_method` records — ahead of the unbounded
            // `channel_elem_types` gate below, so a bounded `recv` (whose `T`
            // also lives in `channel_elem_types`) is never misrouted to the
            // unbounded `*mut KaracChannel` lowering.
            if key == "BoundedChannel.send" && args.len() == 1 {
                return self.compile_bounded_channel_send(object, args);
            }
            if key == "BoundedChannel.recv" && args.is_empty() {
                return self.compile_bounded_channel_recv(object, call_span);
            }
        }

        // Phase 6 line 26 slice 8g: method-call network-boundary intercept.
        // Mirrors slice 8d's free-function intercept (`compile_call`) for
        // `obj.method(args)` shapes where the resolved `Type.method` key
        // is in `state_machine_state_constructors`. The receiver `obj`
        // becomes `self` and stores into state struct field 1 (slice 4's
        // layout puts `self` at position 0). Method args follow at
        // fields 2..K. Runs ahead of every other method-call dispatch
        // path so the intercept fires before any receiver-shape
        // shortcuts (Option/Result, indexed-receiver, field-receiver,
        // entry-chain, clone-on-collection) — for a network-boundary
        // method those shortcuts would emit an inappropriate direct
        // call. Receiver compilation routes through the standard
        // `compile_expr` path, matching slice 8f's arg-store handling.
        if let Some(ref key) = dispatch_key {
            // A2 slice 2b.4(b): coroutine-compiled method handler. Same
            // dispatcher-driven slot-wait drive as the free-fn intercept
            // (call_dispatch.rs), but the receiver `object` is the ramp's first
            // arg (self at param index 0), method args follow at 1..K, and the
            // hidden completion slot is last. The caller never resumes — the
            // dispatcher drives via the unchanged 2b.1 shim. Runs ahead of the
            // degenerate poll-loop intercept below so a coro method key takes the
            // coroutine path.
            if self.is_coroutine_compiled(key) {
                let ramp = self
                    .module
                    .get_function(key)
                    .expect("coroutine method ramp declared in declare_function");
                let ref_flags = self
                    .fn_sig
                    .fn_param_ref
                    .get(key)
                    .cloned()
                    .unwrap_or_default();
                let slice_elems = self
                    .fn_sig
                    .fn_param_slice_elem
                    .get(key)
                    .cloned()
                    .unwrap_or_default();
                let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> =
                    Vec::with_capacity(args.len() + 2);
                // self (param index 0), dispatched by its declared mode.
                let self_is_ref = ref_flags.first().copied().unwrap_or(false);
                let self_val: BasicValueEnum<'ctx> = if self_is_ref {
                    if let ExprKind::Identifier(var_name) = &object.kind {
                        if let Some(ptr) = self.get_data_ptr(var_name) {
                            ptr.into()
                        } else {
                            let v = self.compile_expr(object)?;
                            self.materialize_rvalue_for_ref_arg(v, usize::MAX)
                        }
                    } else {
                        let v = self.compile_expr(object)?;
                        self.materialize_rvalue_for_ref_arg(v, usize::MAX)
                    }
                } else {
                    // Owned receiver moved into the coroutine method — the
                    // coroutine owns + drops it at completion, so suppress the
                    // caller's drop (mirrors the free-fn coroutine arg path in
                    // `call_dispatch`). No-op for non-`UserDrop` receivers; the
                    // channel-end sibling suppresses an early `DropChannelEnd`
                    // close on a moved `Sender`/`Receiver` receiver.
                    if let ExprKind::Identifier(var_name) = &object.kind {
                        self.suppress_user_drop_for_var(var_name);
                        self.suppress_channel_drop_for_var(var_name);
                    }
                    self.compile_expr(object)?
                };
                call_args.push(self_val.into());
                // Method args at param indices 1..K.
                for (i, arg) in args.iter().enumerate() {
                    let param_idx = i + 1;
                    let is_ref = ref_flags.get(param_idx).copied().unwrap_or(false);
                    if !is_ref {
                        // B-2026-07-28-4: by-value struct arg whose param
                        // declined the entry copy — move it, don't leave both
                        // sides owning it.
                        self.move_declined_copy_struct_arg(&arg.value);
                    }
                    let slice_elem = slice_elems.get(param_idx).copied().flatten();
                    let val: BasicValueEnum<'ctx> = if is_ref {
                        if let ExprKind::Identifier(var_name) = &arg.value.kind {
                            if let Some(ptr) = self.get_data_ptr(var_name) {
                                ptr.into()
                            } else {
                                let v = self.compile_expr(&arg.value)?;
                                self.materialize_rvalue_for_ref_arg(v, i)
                            }
                        } else {
                            let v = self.compile_expr(&arg.value)?;
                            self.materialize_rvalue_for_ref_arg(v, i)
                        }
                    } else if let Some(elem_ty) = slice_elem {
                        match self.coerce_to_slice(&arg.value, elem_ty)? {
                            Some(slice_val) => slice_val,
                            None => self.compile_expr(&arg.value)?,
                        }
                    } else {
                        // Owned method arg moved into the coroutine — suppress the
                        // caller's drop (see the receiver case above), including
                        // an early channel-end close on a moved `Sender`/
                        // `Receiver`.
                        if let ExprKind::Identifier(var_name) = &arg.value.kind {
                            self.suppress_user_drop_for_var(var_name);
                            self.suppress_channel_drop_for_var(var_name);
                        }
                        self.compile_expr(&arg.value)?
                    };
                    call_args.push(val.into());
                }
                // Hidden trailing completion slot. A2 slice 5a — inside a
                // `__spawn_coro_wrap` body (`self.conc.coro_spawn_slot` is `Some`)
                // the runtime owns the slot and binds it to the `TaskHandle`;
                // we ramp and return (worker freed). Otherwise the caller owns
                // it: allocate, ramp, block, free (the inline drive).
                let spawn_slot = self.conc.coro_spawn_slot;
                let slot = match spawn_slot {
                    Some(s) => s,
                    None => {
                        let slot_new = self
                            .module
                            .get_function("karac_runtime_park_slot_new")
                            .expect("karac_runtime_park_slot_new declared in Codegen::new");
                        self.builder
                            .build_call(slot_new, &[], "kara.coro.slot")
                            .expect("call karac_runtime_park_slot_new")
                            .try_as_basic_value()
                            .unwrap_basic()
                            .into_pointer_value()
                    }
                };
                call_args.push(slot.into());
                self.builder
                    .build_call(ramp, &call_args, "kara.coro.drive")
                    .expect("call coroutine method ramp");
                if spawn_slot.is_none() {
                    let wait_fn = self
                        .module
                        .get_function("karac_runtime_park_slot_wait")
                        .expect("karac_runtime_park_slot_wait declared in Codegen::new");
                    self.builder
                        .build_call(wait_fn, &[slot.into()], "")
                        .expect("call karac_runtime_park_slot_wait");
                    let free_fn = self
                        .module
                        .get_function("karac_runtime_park_slot_free")
                        .expect("karac_runtime_park_slot_free declared in Codegen::new");
                    self.builder
                        .build_call(free_fn, &[slot.into()], "")
                        .expect("call karac_runtime_park_slot_free");
                }
                return Ok(self.context.i64_type().const_int(0, false).into());
            }
            if let Some(ctor_fn) = self.conc.state_machine_state_constructors.get(key).copied() {
                let poll_fn = self
                    .conc
                    .state_machine_poll_fns
                    .get(key)
                    .copied()
                    .expect("poll-fn co-emitted with state-machine constructor");
                let state_struct = self
                    .conc
                    .state_struct_types
                    .get(key)
                    .copied()
                    .expect("state struct type co-emitted with constructor");
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let i8_ty = self.context.i8_type();
                let cur_fn = self
                    .builder
                    .get_insert_block()
                    .and_then(|bb| bb.get_parent())
                    .expect("compile_method_call inside a function context");
                // Slice 8ae: consult the method's ref / slice tables
                // so `self` and method args dispatch by mode (ref →
                // data ptr; mut Slice → coerce_to_slice; owned →
                // loaded value), mirroring slice 8z (per-mono
                // intercept in `compile_generic_call`) and slice 8ad
                // (non-generic free-fn intercept in `compile_call`).
                // Without this, a method whose param is `ref T` /
                // `mut Slice[T]` would store the wrong-shape value
                // into the ptr- or Slice-struct-shaped state-struct
                // field. `fn_param_ref` / `fn_param_slice_elem` are
                // keyed on the impl-method's dotted name (e.g.
                // `"Hub.run"`) — populated by `declare_function`
                // against the synthesized impl-method function whose
                // `params[0]` is self after `make_impl_method_function`
                // promotes the `SelfParam` into a real `Param`. So
                // `ref_flags[0]` covers `ref self` / `mut ref self`;
                // `ref_flags[1..]` covers method args at param indices
                // 1..K.
                let ref_flags = self
                    .fn_sig
                    .fn_param_ref
                    .get(key)
                    .cloned()
                    .unwrap_or_default();
                let slice_elems = self
                    .fn_sig
                    .fn_param_slice_elem
                    .get(key)
                    .cloned()
                    .unwrap_or_default();

                // Allocate the state struct via the constructor.
                let state_call = self
                    .builder
                    .build_call(ctor_fn, &[], "kara.state")
                    .expect("call state-struct constructor");
                let state_ptr = state_call
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                // Store the receiver into state struct field 1 (self
                // is at layout position 0 → state struct field 1
                // after the i32 tag at field 0). Dispatch by self's
                // declared mode: `ref self` / `mut ref self` route
                // through `get_data_ptr` for Identifier receivers (or
                // materialize an rvalue temp); plain `self` stores
                // the loaded value as before.
                let self_field_ptr = self
                    .builder
                    .build_struct_gep(state_struct, state_ptr, 1, "kara.self.field_ptr")
                    .expect("GEP state struct field 1 for self");
                let self_is_ref = ref_flags.first().copied().unwrap_or(false);
                let self_to_store: BasicValueEnum<'ctx> = if self_is_ref {
                    if let ExprKind::Identifier(var_name) = &object.kind {
                        if let Some(ptr) = self.get_data_ptr(var_name) {
                            ptr.into()
                        } else {
                            let val = self.compile_expr(object)?;
                            self.materialize_rvalue_for_ref_arg(val, usize::MAX)
                        }
                    } else {
                        let val = self.compile_expr(object)?;
                        self.materialize_rvalue_for_ref_arg(val, usize::MAX)
                    }
                } else {
                    self.compile_expr(object)?
                };
                self.builder
                    .build_store(self_field_ptr, self_to_store)
                    .expect("store self into state struct field 1");
                // Method args follow at fields 2..K. ref_flags /
                // slice_elems param indices are offset by 1 (self at
                // index 0, so method arg `i` is at param index
                // `i + 1`).
                for (i, arg) in args.iter().enumerate() {
                    let field_idx = (i + 2) as u32;
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            state_struct,
                            state_ptr,
                            field_idx,
                            &format!("kara.arg{i}.field_ptr"),
                        )
                        .expect("GEP state struct field for method arg");

                    let param_idx = i + 1;
                    let is_ref = ref_flags.get(param_idx).copied().unwrap_or(false);
                    if !is_ref {
                        // B-2026-07-28-4: by-value struct arg whose param
                        // declined the entry copy — move it, don't leave both
                        // sides owning it.
                        self.move_declined_copy_struct_arg(&arg.value);
                    }
                    let slice_elem = slice_elems.get(param_idx).copied().flatten();

                    let to_store: BasicValueEnum<'ctx> = if is_ref {
                        if let ExprKind::Identifier(var_name) = &arg.value.kind {
                            if let Some(ptr) = self.get_data_ptr(var_name) {
                                ptr.into()
                            } else {
                                let val = self.compile_expr(&arg.value)?;
                                self.materialize_rvalue_for_ref_arg(val, i)
                            }
                        } else {
                            let val = self.compile_expr(&arg.value)?;
                            self.materialize_rvalue_for_ref_arg(val, i)
                        }
                    } else if let Some(elem_ty) = slice_elem {
                        match self.coerce_to_slice(&arg.value, elem_ty)? {
                            Some(slice_val) => slice_val,
                            None => self.compile_expr(&arg.value)?,
                        }
                    } else {
                        self.compile_expr(&arg.value)?
                    };

                    self.builder
                        .build_store(field_ptr, to_store)
                        .expect("store method arg into state struct field");
                }
                // Poll loop + cooperative yield + done + free — same
                // shape as slice 8d/8e for the free-function intercept.
                let loop_bb = self.context.append_basic_block(cur_fn, "kara.poll_loop");
                let yield_bb = self.context.append_basic_block(cur_fn, "kara.poll_yield");
                let done_bb = self.context.append_basic_block(cur_fn, "kara.poll_done");
                self.builder
                    .build_unconditional_branch(loop_bb)
                    .expect("br to poll loop");
                self.builder.position_at_end(loop_bb);
                let null_cancel = ptr_ty.const_null();
                let poll_call = self
                    .builder
                    .build_call(
                        poll_fn,
                        &[state_ptr.into(), null_cancel.into()],
                        "kara.poll_result",
                    )
                    .expect("call poll-fn");
                let poll_result = poll_call
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let is_pending = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        poll_result,
                        i8_ty.const_int(0, false),
                        "kara.is_pending",
                    )
                    .expect("icmp eq i8 result, 0");
                self.builder
                    .build_conditional_branch(is_pending, yield_bb, done_bb)
                    .expect("br on poll discriminant");
                self.builder.position_at_end(yield_bb);
                self.builder
                    .build_call(self.runtime_fns.sched_yield_fn, &[], "kara.yield_result")
                    .expect("call sched_yield");
                self.builder
                    .build_unconditional_branch(loop_bb)
                    .expect("br back to poll loop after yield");
                self.builder.position_at_end(done_bb);
                // Slice 8i: load the callee's terminal return-value
                // field before `free`. Mirrors the call_dispatch.rs
                // intercept's load-before-free ordering — once the
                // state struct is freed, the field is no longer
                // dereferenceable.
                let call_result =
                    if let Some(ret_ty) = self.conc.state_machine_return_types.get(key).copied() {
                        let n_fields = state_struct.count_fields();
                        let terminal_idx = n_fields - 1;
                        let terminal_ptr = self
                            .builder
                            .build_struct_gep(
                                state_struct,
                                state_ptr,
                                terminal_idx,
                                "kara.return.field_ptr",
                            )
                            .expect("GEP terminal return-value field on caller side (method call)");
                        self.builder
                            .build_load(ret_ty, terminal_ptr, "kara.return.value")
                            .expect("load callee return value from terminal field (method call)")
                    } else {
                        self.context.i64_type().const_int(0, false).into()
                    };
                self.builder
                    .build_call(self.runtime_fns.free_fn, &[state_ptr.into()], "")
                    .expect("call free on state struct");
                return Ok(call_result);
            }
        }

        // Strict-provenance `ptr` module — `ptr.addr(p)` /
        // `ptr.with_addr(p, a)` / `ptr.expose(p)` / `ptr.from_exposed(a)`
        // (and the `_mut` variants), per `design.md § Pointer
        // Provenance` (v60 item 20). Skipped when a local binding
        // shadows `ptr` — the prelude module loses to a user-scope
        // binding by the standard shadow rule. The seven entries are
        // also registered in `env.functions` for the typechecker (see
        // `src/typechecker/env_build.rs`), so the dispatch shapes line
        // up between the two phases. Helper's docstring covers the
        // pragmatic-lowering rationale under the current i64-pointer
        // ABI plus the follow-up path to a provenance-preserving
        // variant.
        if let ExprKind::Identifier(name) = &object.kind {
            if name == "ptr" && !self.variables.contains_key("ptr") {
                if let Some(value) = self.compile_ptr_module_call(method, args)? {
                    return Ok(value);
                }
            }
        }

        // `critical_section.acquire()` (design.md § Critical sections) — call
        // the runtime interrupt-mask primitive and yield the restore token as
        // the guard value. The `CriticalSectionGuard` is a single-`i64`-field
        // stdlib struct represented as its bare `i64` word (like the socket
        // structs — see tcp.rs): the token IS the guard, stored in an i64
        // slot the hand-rolled `@CriticalSectionGuard.drop` GEPs back as
        // `{i64}` field 0 at scope exit. Guarded on no local `critical_section`
        // shadow, matching the typechecker/resolver.
        if let ExprKind::Identifier(name) = &object.kind {
            if name == "critical_section"
                && method == "acquire"
                && !self.variables.contains_key("critical_section")
            {
                return self.compile_critical_section_acquire();
            }
        }

        // Slice OR (2026-05-16): Option/Result `unwrap`/`expect`/`is_*`
        // dispatch is receiver-shape-agnostic — the receiver may be any
        // Option-/Result-valued expression (identifier, method chain,
        // field access, index, …). Lower the receiver to its
        // `{ i64 tag, i64 w0, i64 w1, i64 w2 }` aggregate, dispatch on
        // the tag, and either reconstitute the payload (`unwrap`/`expect`)
        // or yield a bool (`is_some`/`is_none`/`is_ok`/`is_err`). The
        // inner `T` for payload reconstitution is recovered from the
        // typechecker-populated `method_unwrap_inner_types` side-table.
        // Routing this dispatch BEFORE the Index/FieldAccess
        // synth-identifier arms is intentional: those arms mint a synth
        // tied to the *receiver's storage*, which doesn't exist for
        // method-chain receivers like `m.get(k).unwrap()`. Keeping the
        // receiver as a temporary SSA value sidesteps that constraint
        // entirely.
        if matches!(
            method,
            "unwrap"
                | "expect"
                | "is_some"
                | "is_none"
                | "is_ok"
                | "is_err"
                | "unwrap_or"
                | "unwrap_err"
                | "expect_err"
                | "map"
                // Option/Result combinators, non-closure batch (B-2026-07-14-6).
                | "ok"
                | "err"
                | "or"
                | "and"
                | "ok_or"
                | "flatten"
                | "take"
                | "get_or_insert"
                // Option/Result combinators, closure batch (B-2026-07-14-6).
                | "unwrap_or_else"
                | "map_or"
                | "map_or_else"
                | "map_err"
                | "and_then"
                | "or_else"
                | "filter"
        ) {
            // B-2026-07-30-11 (Option/Result leg) — a CONSUMING combinator on
            // a named Option/Result binding moves the payload out (`let r =
            // a.unwrap();`): the result's owner runs the payload's Drop body,
            // so the source binding's `__karac_dropelems_*` walk must be
            // retracted or the body prints twice. Prefix-keyed, so a binding
            // that registered no walk is a no-op; the `is_*` probes are
            // excluded (reads, not moves), as is `get_or_insert` (in-place
            // mutation — the receiver retains ownership). Interp twin: the
            // same method list in `try_eval_option_result_method`.
            if !matches!(
                method,
                "is_some" | "is_none" | "is_ok" | "is_err" | "get_or_insert"
            ) {
                if let ExprKind::Identifier(recv) = &object.kind {
                    let recv = recv.clone();
                    self.suppress_container_elem_bodies_for_var(&recv);
                }
            }
            if let Some(value) =
                self.try_compile_option_result_method(object, method, args, call_span)?
            {
                return Ok(value);
            }
        }

        // A BARE `Iterator.flatten()` VALUE — a materialized flatten iterator
        // (`let it = xs.iter().flatten()`) with no terminal / for-loop to drive
        // it — has no codegen representation (mirrors the bare-`.rev()` value
        // bail). The DRIVEN shapes are lowered elsewhere: the fused TERMINALS
        // (collect/sum/fold/count/…) treat a flatten receiver as a structural
        // fused base via `peel_base_is_structural_adaptor` (slice 3), and the
        // `for x in <recv>.flatten()` loop via `try_compile_for_flatten` (slice
        // 2). Placed AFTER the Option/Result combinator dispatch above so
        // `Option[Option[T]].flatten()` / `Result.flatten()` (handled there,
        // returned) never reach here — any `.flatten()` that does is an ITERATOR
        // flatten. Any still-unhandled driven shape (a flatten under a
        // non-fused adaptor like `zip`) falls through to the generic loud
        // "no handler" diagnostic below, never a silent skip.
        if method == "flatten" && args.is_empty() {
            return Err(
                "`Iterator.flatten()` as a bare iterator value is not yet supported under \
                 `karac build`/`karac run` (codegen); chain a terminal (`.collect()` / \
                 `.sum()` / a `for` loop) or re-run with `--interp`."
                    .to_string(),
            );
        }

        // Slice MR (2026-05-09): indexed-receiver method dispatch. When the
        // receiver expression is `obj[i]` (an `Index` node), lower the index
        // access to obtain a pointer into the outer container's storage,
        // synthesize an identifier bound to that pointer with the element's
        // type registries populated, and re-dispatch the method through the
        // existing identifier path. Closes the LeetCode 3629 kata's primary
        // blocker (`factors[j].push(i)`). MR5: chained `a[i][j].method()` is
        // rejected with a clear diagnostic — bind to a temporary first.
        if let ExprKind::Index {
            object: inner,
            index,
        } = &object.kind
        {
            // B-2026-08-14-20 — a RANGE index is not an element access.
            // `v[a..b]` produces a `Slice[T]` VIEW, so the element-pointer
            // lowering below is the wrong shape twice over: it registers the
            // synth from the container's element `TypeExpr` (`i64` for a
            // `Vec[i64]`, so the view looks like a scalar) and it GEPs to one
            // element instead of building a `{ptr, len}` header. Every method
            // on such a receiver then fell through to the loud "no handler on
            // variable '__indexed_elem_N'" error while `--interp` ran it —
            // `v[1..3].len()` as much as `v[1..3].to_vec()`, so this is
            // method-agnostic, not specific to the method added here. Route it
            // to the slice-temporary materialization instead, which builds the
            // header the view actually is.
            //
            // Gated by the same `string_typed_exprs` test the String-slice arm
            // at the head of `compile_indexed_receiver_method` uses, so the two
            // are mutually exclusive by construction: `s[a..b]` on a String is
            // a fresh OWNED String, not a view, and must keep its own path.
            // B-2026-08-18-14 leg 2 — ask the STATIC type walk first, and only
            // fall back to the span table when it has no answer. The table is
            // span-keyed, and in a method CHAIN the inner receiver's span
            // collides with a String-typed node's (the same
            // `MethodCall.span == receiver.span` collision B-2026-08-18-7 and
            // -9 were about): `v[0..3].first_or(-1).to_string()` reported the
            // `Vec[i64]` receiver as string-typed purely because the chain ends
            // in `.to_string()`. That sent the view to the element-pointer
            // lowering, which compiled the RANGE as a subscript and died —
            // while the SAME call with the `.to_string()` split onto its own
            // line built fine. A discriminator that depends on what happens
            // LATER in the chain cannot be right; `type_name_of_expr` resolves
            // the receiver itself.
            let inner_is_string = match self.type_name_of_expr(inner).as_deref() {
                Some(name) => name == "String",
                None => self
                    .span_tables
                    .string_typed_exprs
                    .contains(&(inner.span.offset, inner.span.length)),
            };
            if matches!(&index.kind, ExprKind::Range { .. }) && !inner_is_string {
                if let Some(value) = self.try_compile_nonident_slice_method(
                    object,
                    method,
                    args,
                    call_span,
                    args_close_span,
                )? {
                    return Ok(value);
                }
            }
            return self.compile_indexed_receiver_method(inner, index, method, args, call_span);
        }

        // Slice FR (2026-05-16): field-receiver method dispatch. Sibling to
        // the MR slice above — when the receiver is `outer.field` (a
        // `FieldAccess`), GEP into the struct (shared or plain) to the field
        // pointer, mint a synth identifier bound to that pointer with the
        // field type's side tables populated, and re-dispatch the method.
        // Closes the LeetCode 133 kata's primary blocker
        // (`curr_clone.neighbors.push(nb_clone)` on a `shared struct Node`
        // with `mut neighbors: Vec[Node]`). Returns `Some(_)` only when the
        // receiver shape is one we know how to lower; otherwise the regular
        // dispatch below runs (so the generic field-by-value extract path
        // and the fall-through diagnostic still apply for unsupported
        // shapes).
        if let ExprKind::FieldAccess {
            object: inner,
            field,
        } = &object.kind
        {
            // `self.field.method()` — `self` parses as `SelfValue`, which the
            // shared `lower_field_access_ptr` (used by the helper below)
            // deliberately leaves at `Ok(None)` so the atomic-on-self path
            // (`self.count.fetch_add(...)`, dispatched further down via
            // `is_atomic_receiver` → `compile_atomic_method`) keeps its
            // dedicated handler. For NON-atomic self-field receivers we
            // normalise to a synthetic `Identifier("self")` (self is registered
            // under the name "self" in every per-binding registry) so String /
            // Vec field methods dispatch through the field-receiver helper.
            // Gated on `!is_atomic_receiver(object)` so the atomic fall-through
            // is byte-identical. Self-hosting lexer: `self.src.substring(a, b)`.
            let self_ident;
            let inner: &Expr =
                if matches!(inner.kind, ExprKind::SelfValue) && !self.is_atomic_receiver(object) {
                    self_ident = Expr {
                        kind: ExprKind::Identifier("self".to_string()),
                        span: inner.span,
                    };
                    &self_ident
                } else {
                    inner
                };
            if let Some(value) =
                self.try_compile_field_receiver_method(inner, field, method, args, call_span)?
            {
                return Ok(value);
            }
        }

        // `h.m.0.method()` — a method on a Map/Set TUPLE element (#26). The
        // `FieldAccess` arm above handles `s.m.method()`; this is the
        // tuple-index sibling. Returns `Some` only for a Map/Set element (the
        // ptr-handle case that needs a named handle slot); Vec/scalar/struct
        // tuple elements fall through to the value-extraction path below.
        if matches!(object.kind, ExprKind::TupleIndex { .. }) {
            if let Some(value) =
                self.try_compile_tuple_index_receiver_method(object, method, args, call_span)?
            {
                return Ok(value);
            }
        }

        // Trailing-method dispatch on an entry-chain receiver — e.g.
        // `bucket.entry(p).or_insert(Vec.new()).push(j)`. The chain
        // produces a slot pointer (`*mut V`); the synth-identifier
        // pattern (mirrors MR-slice indexed-receiver dispatch) wraps it
        // so the recursive call resolves `.method(args)` through the
        // regular identifier-keyed flow. Returns Some(_) only when the
        // receiver is a recognised or_insert / or_insert_with chain.
        // B-2026-08-18-34 — a Map entry chain rooted at a struct FIELD
        // (`h.buckets.entry(k).or_insert(d).push(v)`, `self.buckets…`). Binds
        // the field's address to a synth identifier and re-dispatches, so both
        // the `or_insert` terminal and the trailing `.push` below run through
        // their existing identifier-keyed lowerings. Must precede both, since
        // each declines a non-identifier map root.
        if let Some(value) = self.try_compile_field_rooted_entry_chain(
            object,
            method,
            args,
            call_span,
            args_close_span,
        )? {
            return Ok(value);
        }

        if let Some(value) =
            self.compile_entry_chain_receiver_method(object, method, args, call_span)?
        {
            return Ok(value);
        }

        // Map.entry(k) chain dispatch — `m.entry(k){.and_modify(f)}*.{or_insert(d)|
        // or_insert_with(f)|and_modify(f)}` is lowered as a single sequence
        // around one `karac_map_entry` call so the slot pointer stays valid
        // and there's exactly one hash. Returns Some(_) only when the receiver
        // chain is recognised; otherwise the regular dispatch below runs.
        if let Some(value) = self.try_compile_entry_chain(object, method, args)? {
            return Ok(value);
        }

        // `clone()` dispatch on collection variables — Vec[T], String,
        // Map[K, V], Set[T]. Routes through the per-type clone-fn machinery
        // (`emit_clone_fn_for_type_expr`); see the `Clone trait surface for
        // collections` bullet in `phase-8-stdlib-floor.md`. Returns Some(_)
        // when the receiver is an identifier-bound collection variable;
        // otherwise the regular dispatch below runs (so user `impl X { fn
        // clone(...) }` continues to resolve through the impl-block path).
        // Is this call's receiver a scalar `Copy` primitive (int / float /
        // bool / char)? Read it from the static receiver type the typechecker
        // recorded for this call span (`dispatch_key` = "<Type>.<method>"),
        // NOT from the compiled value's LLVM kind — so we can gate `clone` /
        // `to_string` below WITHOUT pre-compiling the receiver, which keeps a
        // single evaluation for any receiver form (literal, `(expr)`, field,
        // call) and never double-evaluates a side-effecting receiver.
        //
        // B-2026-08-11-22: `dispatch_key` alone is not enough, for the same
        // reason it was not enough above — it is span-keyed, and a chain shares
        // one key. In `n.to_string().to_string()` the inner link reads the
        // outer's `String.to_string`, so the receiver looks like a String here
        // and the scalar arms below decline; the call then died in the catch-all
        // as "no handler for method 'to_string'" even though `n` is an i64.
        // `type_name_of_expr` is a static lookup keyed by the receiver
        // EXPRESSION, so it cannot be shadowed, and consulting it costs nothing
        // — in particular it does NOT pre-compile the receiver, which is the
        // property this gate was written to preserve.
        fn is_scalar_primitive_name(t: &str) -> bool {
            matches!(
                t,
                "i8" | "i16"
                    | "i32"
                    | "i64"
                    | "u8"
                    | "u16"
                    | "u32"
                    | "u64"
                    | "usize"
                    | "isize"
                    // 128-bit (B-2026-08-19-8 stage 4). Their absence made
                    // `u.to_string()` on an `i128`/`u128` fall through method
                    // dispatch entirely — a loud "no handler" codegen error,
                    // not a wrong answer, but still a gap.
                    | "i128"
                    | "u128"
                    | "f32"
                    | "f64"
                    | "bool"
                    | "char"
            )
        }
        // B-2026-08-13-2 — the THIRD instance of the gap the comment above
        // describes, and the one neither existing arm can close. `dispatch_key`
        // is span-keyed and a chain shares one, so in `7.to_string().len()` the
        // inner link reads the outer's `String.len`; `type_name_of_expr` is a
        // NAME lookup, so it answers for `n.to_string()` (B-2026-08-11-22's fix)
        // and returns `None` for a receiver that has no name — a literal, a
        // parenthesized expression, a cast. Both arms decline and the scalar
        // `to_string` lowering below never fires, which is why every failing
        // shape on that row is a non-identifier scalar: `'x'`, `7`, `3.5`,
        // `true`, `(7 + 1)`.
        //
        // Answered SYNTACTICALLY, which is exactly what the other two arms
        // cannot be: the receiver's scalar-ness is visible in the expression
        // itself, and reading it costs no compilation of the receiver — the
        // property the comment above notes this gate was written to preserve.
        //
        // Primitive arithmetic arrives as an intrinsic CALL (`7 + 1` desugars
        // to `i64.add(7, 1)` before either backend sees it), so the `Call` arm
        // is what covers `(7 + 1).to_string()`; without it that shape alone
        // kept failing.
        fn expr_is_syntactic_scalar(e: &Expr) -> bool {
            match &e.kind {
                ExprKind::Integer(..)
                | ExprKind::Float(..)
                | ExprKind::Bool(..)
                | ExprKind::CharLit(..)
                | ExprKind::ByteLit(..)
                | ExprKind::ByteStringLit(..) => true,
                ExprKind::Cast { ty, .. } => matches!(
                    &ty.kind,
                    TypeKind::Path(p)
                        if p.segments.last().is_some_and(|s| is_scalar_primitive_name(s))
                ),
                ExprKind::Unary { operand, .. } => expr_is_syntactic_scalar(operand),
                ExprKind::Binary { left, right, .. } => {
                    expr_is_syntactic_scalar(left) && expr_is_syntactic_scalar(right)
                }
                ExprKind::Call { callee, args } => {
                    matches!(&callee.kind, ExprKind::Path { segments, .. }
                        if segments.len() == 2 && is_scalar_primitive_name(&segments[0]))
                        && args.iter().all(|a| expr_is_syntactic_scalar(&a.value))
                }
                _ => false,
            }
        }
        let recv_is_scalar_primitive = dispatch_key
            .as_deref()
            .and_then(|k| k.rsplit_once('.'))
            .map(|(t, _)| is_scalar_primitive_name(t))
            .unwrap_or(false)
            || self
                .type_name_of_expr(object)
                .is_some_and(|t| is_scalar_primitive_name(&t))
            || expr_is_syntactic_scalar(object);

        if method == "clone" && args.is_empty() {
            if let Some(value) = self.try_compile_clone(object)? {
                return Ok(value);
            }
            // Scalar `Copy` primitive — clone is identity.
            if recv_is_scalar_primitive {
                return self.compile_expr(object);
            }
        }

        // `recv.try_clone() -> Result[Self, AllocError]` — the fallible
        // companion of `clone` (phase-8-stdlib-floor item 8). Routed here
        // (before the receiver-type dispatch below) so Vec/VecDeque/String
        // share one lowering; Map/Set-bearing receivers are rejected loudly
        // inside `try_compile_try_clone` (blocked on a fallible
        // `karac_map_*` runtime API).
        if method == "try_clone" && args.is_empty() {
            if let Some(value) = self.try_compile_try_clone(object)? {
                return Ok(value);
            }
        }

        // Scalar-primitive `x.to_string() -> String` (typed in
        // expr_method_call.rs). Render the value via the same path f-strings
        // use, then copy the bytes into an owning `String`. `char` lowers to
        // i32, so render it as a glyph rather than the integer codepoint.
        // String/struct receivers (whose explicit `.to_string()` is a
        // separate, unimplemented codegen path) are not scalar primitives and
        // fall through unchanged.
        if method == "to_string" && args.is_empty() && recv_is_scalar_primitive {
            let v = self.compile_expr(object)?;
            let (src_ptr, src_len) = if self.expr_is_char(object) {
                self.emit_codepoint_to_utf8(v.into_int_value())
            } else {
                self.compile_fstr_part_to_cstr(v, object)
            };
            return Ok(self.build_owned_string_from_parts(src_ptr, src_len));
        }

        // `String.to_string()` — an owning copy. The receiver's static type is
        // `String` when `dispatch_key`'s receiver segment is "String". Compile
        // the receiver to its `{data,len,cap}` value and copy the bytes into a
        // fresh heap String, so it works for any receiver form (identifier,
        // literal, expression) and the result owns its buffer.
        //
        // `StringSlice.to_string()` is the borrowed-view escape hatch (design.md
        // § StringSlice: "To store a slice beyond the borrow, call .to_string()")
        // — the same copy: a `StringSlice` is `{ptr,len,cap=0}`, so copying its
        // `len` bytes yields an independent owned `String`.
        // The dispatch-key gate handles the terminal call; `expr_is_string_like`
        // additionally covers a `to_string` whose span-keyed dispatch_key is
        // shadowed by an outer chained call (`s.to_string().to_uppercase()`,
        // B-2026-07-16-20) by recognising a statically String/StringSlice
        // receiver directly.
        // B-2026-08-11-22: a statically-known non-String receiver VETOES the
        // `dispatch_key` half of this test — and only that half.
        //
        // `dispatch_key` is span-keyed, and the parser sets a MethodCall's span
        // equal to its RECEIVER's, so in `n.to_string().to_string()` both links
        // share one key. The chained-call collision guard where `dispatch_key`
        // is computed cannot separate them here, because it works by requiring
        // the key's method segment to match this call's method — and both links
        // ARE named `to_string`. The inner link therefore read the outer's
        // `String.to_string`, entered this String-copy path with an `i64`
        // receiver, and panicked unwrapping an IntValue as a struct.
        //
        // The receiver's own type is the reliable signal, and it separates the
        // two links exactly: the inner one reports `Some("i64")`, the outer one
        // `None` (a `to_string` call's type is not resolvable here), so nothing
        // that previously worked is vetoed. The `expr_is_string_like` half is
        // left alone — it reads the receiver expression rather than a span
        // table, so it cannot be shadowed.
        let recv_known_non_string = self
            .type_name_of_expr(object)
            .is_some_and(|t| t != "String" && t != "StringSlice");
        if method == "to_string"
            && args.is_empty()
            && (dispatch_key
                .as_deref()
                .and_then(|k| k.rsplit_once('.'))
                .map(|(t, _)| t == "String" || t == "StringSlice")
                .unwrap_or(false)
                && !recv_known_non_string
                || self.expr_is_string_like(object))
        {
            let v = self.compile_expr(object)?.into_struct_value();
            let data = self
                .builder
                .build_extract_value(v, 0, "ts.s.data")
                .unwrap()
                .into_pointer_value();
            let len = self
                .builder
                .build_extract_value(v, 1, "ts.s.len")
                .unwrap()
                .into_int_value();
            let copied = self.build_owned_string_from_parts(data, len);
            // Free the intermediate receiver temp when it is a fresh owned
            // String (a chained `x.trim().to_string()` /
            // `x.to_uppercase().to_string()` receiver): the copy above is an
            // independent buffer, and nothing else owns the receiver's — so
            // without this it leaks once per call (unbounded in a loop). A
            // place-expr / identifier / literal receiver is NOT a fresh temp
            // (`expr_yields_fresh_owned_temp` matches Call/MethodCall only), and
            // the `cap > 0` guard in `free_str_vec_buffer_if_heap` additionally
            // no-ops on a borrowed (cap == 0) view. B-2026-07-16-21.
            if self.expr_yields_fresh_owned_temp(object)
                || self.expr_is_fresh_owned_string_slice(object)
            {
                self.free_str_vec_buffer_if_heap(v.into());
            }
            return Ok(copied);
        }

        // `myStruct.to_string()` for a `#[derive(Display)]` / `impl Display`
        // struct → render to an owning `String` in declaration order (matches
        // the interpreter). See `synth_display.rs`.
        //
        // A user `impl Display` (a compiled `<Type>.to_string`) wins: skip the
        // built-in renderers below so the call falls through to the generic
        // user-method dispatch, which invokes the user body. GAP-W4.
        if method == "to_string" && args.is_empty() && self.user_display_impl_type(object).is_none()
        {
            if let Some(sname) = self.expr_user_struct_name(object) {
                return self.compile_struct_display_string(object, &sname);
            }
            // All-unit enum → owning String of the variant name.
            if let Some(ename) = self.expr_user_enum_name(object) {
                let (ptr, len) = self.compile_unit_enum_display(object, &ename)?;
                return Ok(self.build_owned_string_from_parts(ptr, len));
            }
            // Payload-carrying `#[derive(Display)]` enum → render via its
            // value-driven Display fn (the same path f-strings / `println` use,
            // which handles payload variants — `Other(disk full)`), returning an
            // owning String. The typechecker now types `.to_string()` for these
            // (the all-unit restriction was stale once the payload-enum Display
            // renderer landed); this wires the matching codegen so build == run.
            // `expr_user_enum_name_any` also matches all-unit enums, but those
            // returned via the dedicated select-chain above, so only payload
            // enums reach here. (A bare `self.to_string()` — `self` a
            // `SelfValue` — is deliberately NOT handled here: `self` is a `ref`
            // receiver, and naively rendering it as an owned identifier
            // double-frees / misreads; it is a separate ref-aware follow-on
            // tracked in the bug ledger.)
            if let Some(ename) = self.expr_user_enum_name_any(object) {
                let (_acc, sval) = self.render_user_enum_display(object, &ename)?;
                return Ok(sval);
            }
            // Collection (Vec/Map/Set) → owning String via its Display fn. The
            // returned value owns the rendered buffer (the binding frees it);
            // the throwaway acc alloca is not separately tracked.
            if let Some((_acc, sval)) = self.try_compile_collection_display(object)? {
                return Ok(sval);
            }
        }

        // Type-receiver associated calls: `T.method(...)` where `T` is a
        // primitive type name. Receiver `T` is an identifier naming a type,
        // not a variable, so the normal receiver pipeline would fail. Handle
        // `.from` (numeric widening = passthrough) and the operator methods
        // (add/sub/eq/lt/bitand/not/…) by delegating to `compile_assoc_call`,
        // which already knows the primitive fast-path.
        if let ExprKind::Identifier(type_name) = &object.kind {
            let is_primitive = matches!(
                type_name.as_str(),
                "i8" | "i16"
                    | "i32"
                    | "i64"
                    | "u8"
                    | "u16"
                    | "u32"
                    | "u64"
                    | "usize"
                    | "isize"
                    | "f32"
                    | "f64"
                    | "bool"
                    | "char"
                    | "String"
            );
            if is_primitive {
                const OP_METHODS: &[&str] = &[
                    "from", "add", "sub", "mul", "div", "rem", "neg", "eq", "ne", "lt", "le", "gt",
                    "ge", "bitand", "bitor", "bitxor", "shl", "shr", "not",
                ];
                if OP_METHODS.contains(&method) {
                    return self.compile_assoc_call(type_name.as_str(), method, args);
                }
                // `<int_type>.parse(s: String) -> Option[i64]` — base-10
                // signed parse. Extends the primitive-type-receiver
                // dispatch already used by binop methods.
                if method == "parse"
                    && matches!(
                        type_name.as_str(),
                        "i8" | "i16"
                            | "i32"
                            | "i64"
                            | "u8"
                            | "u16"
                            | "u32"
                            | "u64"
                            | "usize"
                            | "isize"
                    )
                {
                    return self.compile_assoc_call(type_name.as_str(), method, args);
                }
                // `<int_type>.from_str_radix(s, radix) -> Option[i64]` — radix
                // parse; same delegation as `parse` (impl in assoc_call.rs).
                if method == "from_str_radix"
                    && matches!(
                        type_name.as_str(),
                        "i8" | "i16"
                            | "i32"
                            | "i64"
                            | "u8"
                            | "u16"
                            | "u32"
                            | "u64"
                            | "usize"
                            | "isize"
                    )
                {
                    return self.compile_assoc_call(type_name.as_str(), method, args);
                }
                // `f64.parse(s) -> Option[f64]` — float parse; same delegation
                // as int `parse` (impl in assoc_call.rs).
                if method == "parse" && type_name.as_str() == "f64" {
                    return self.compile_assoc_call(type_name.as_str(), method, args);
                }
                // `char.try_from(n) -> Result[char, i64]` — fallible codepoint→
                // char conversion (#10; the `E_INT_AS_CHAR` rejection of
                // `n as char` redirects here). Validates the Unicode scalar
                // range and returns `Ok(char)` / `Err(codepoint)`.
                if method == "try_from" && type_name.as_str() == "char" {
                    return self.compile_char_try_from(args);
                }
                // `<int>.try_from(x: <int>) -> Result[<int>, String]` — numeric
                // narrowing / sign-changing conversion (design.md § Conversion
                // Traits). Range-checks the source against the target's bounds
                // and returns `Ok(value)` / `Err("out of range for T")`. Also
                // the lowered target of the `.try_into()` desugar. Parity with
                // the interpreter's `numeric_try_from_value`.
                if method == "try_from"
                    && matches!(
                        type_name.as_str(),
                        "i8" | "i16"
                            | "i32"
                            | "i64"
                            | "u8"
                            | "u16"
                            | "u32"
                            | "u64"
                            | "usize"
                            | "isize"
                    )
                {
                    return self.compile_numeric_try_from(type_name.as_str(), args);
                }
            }
        }

        // Receiver-form `lhs.cmp(rhs)` — synthesizes an `Ordering` enum
        // value from a signed-integer comparison. The receiver may be an
        // identifier (closure param or local) or an arbitrary expression
        // (e.g., `(b.1 - b.0).cmp(...)`), so we evaluate both sides and
        // dispatch on the LLVM value kind. Tag layout matches the
        // declaration order in `runtime/stdlib/ordering.kara` (Less=0,
        // Equal=1, Greater=2); the `Vec.sort_by` bridge thunk relies on
        // that ordering to turn the tag into a `-1 / 0 / +1` comparator
        // via `tag - 1`.
        // Built-in `abs` on signed-integer / float primitives (typed in
        // expr_method_call.rs). Integer abs reuses the checked-neg lowering:
        // `abs(x) = select(x < 0, 0 - x, x)` where `0 - x` goes through the
        // same `ssub.with.overflow` trap path as unary `-`, so `iN::MIN.abs()`
        // traps as `integer overflow` (the neg is computed for all x but only
        // overflows at `iN::MIN`; for x ≥ 0, `0 - x` is in range). Float abs is
        // `select(x < 0.0, -x, x)` — correct for finite values (−0.0/NaN sign
        // edge cases are immaterial here and not exercised).
        if method == "abs" && args.is_empty() {
            let v = self.compile_expr(object)?;
            match v {
                BasicValueEnum::IntValue(iv) => {
                    let zero = iv.get_type().const_zero();
                    let is_neg = self
                        .builder
                        .build_int_compare(IntPredicate::SLT, iv, zero, "abs.isneg")
                        .unwrap();
                    let neg = self
                        .compile_unaryop(&UnaryOp::Neg, iv.into())?
                        .into_int_value();
                    let r = self.builder.build_select(is_neg, neg, iv, "abs").unwrap();
                    return Ok(r);
                }
                BasicValueEnum::FloatValue(fv) => {
                    let zero = fv.get_type().const_zero();
                    let is_neg = self
                        .builder
                        .build_float_compare(inkwell::FloatPredicate::OLT, fv, zero, "fabs.isneg")
                        .unwrap();
                    let neg = self.builder.build_float_neg(fv, "fabs.neg").unwrap();
                    let r = self.builder.build_select(is_neg, neg, fv, "fabs").unwrap();
                    return Ok(r);
                }
                _ => {}
            }
        }

        // `signum` (typed in expr_method_call.rs, signed-int / float only).
        // Int → nested `select(x > 0, 1, select(x < 0, -1, 0))` at the receiver
        // width (signed `icmp`). Float → `select(isnan, x, copysign(1.0, x))`:
        // the `llvm.copysign` intrinsic carries `x`'s sign onto 1.0 (so ±0.0
        // yield ±1.0, matching Rust `f64::signum`), and a NaN receiver returns
        // itself via the ordered-vs-unordered guard.
        if method == "signum" && args.is_empty() {
            let v = self.compile_expr(object)?;
            match v {
                BasicValueEnum::IntValue(iv) => {
                    let ity = iv.get_type();
                    let zero = ity.const_zero();
                    let one = ity.const_int(1, false);
                    let neg_one = ity.const_all_ones();
                    let is_pos = self
                        .builder
                        .build_int_compare(IntPredicate::SGT, iv, zero, "sgn.pos")
                        .unwrap();
                    let is_neg = self
                        .builder
                        .build_int_compare(IntPredicate::SLT, iv, zero, "sgn.neg")
                        .unwrap();
                    let neg_or_zero = self
                        .builder
                        .build_select(is_neg, neg_one, zero, "sgn.lo")
                        .unwrap();
                    let r = self
                        .builder
                        .build_select(is_pos, one.into(), neg_or_zero, "signum")
                        .unwrap();
                    return Ok(r);
                }
                BasicValueEnum::FloatValue(fv) => {
                    let fty = fv.get_type();
                    let one = fty.const_float(1.0);
                    let intrinsic = inkwell::intrinsics::Intrinsic::find("llvm.copysign")
                        .unwrap_or_else(|| panic!("llvm.copysign intrinsic must exist"));
                    let decl = intrinsic
                        .get_declaration(&self.module, &[fty.into()])
                        .unwrap_or_else(|| panic!("llvm.copysign declaration for float type"));
                    let signed_one = self
                        .builder
                        .build_call(decl, &[one.into(), fv.into()], "sgn.cs")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic();
                    // `fcmp uno x, x` is true iff `x` is NaN — return `x` then.
                    let is_nan = self
                        .builder
                        .build_float_compare(inkwell::FloatPredicate::UNO, fv, fv, "sgn.nan")
                        .unwrap();
                    let r = self
                        .builder
                        .build_select(is_nan, fv.into(), signed_one, "signum")
                        .unwrap();
                    return Ok(r);
                }
                _ => {}
            }
        }

        // Built-in float arithmetic helpers (typed in expr_method_call.rs,
        // float-only): `recip` → `fdiv 1.0, x`; `to_degrees` / `to_radians` →
        // `fmul x, C` with the SAME constants Rust `f64::to_degrees`/
        // `to_radians` use; `fract` → `fsub x, trunc(x)` (Rust `f64::fract` =
        // `self - self.trunc()`). All bit-exact with the interpreter's `f64::*`;
        // `const_float` rounds the f64 constant to the receiver width.
        if matches!(method, "recip" | "to_degrees" | "to_radians" | "fract") && args.is_empty() {
            let v = self.compile_expr(object)?;
            if let BasicValueEnum::FloatValue(fv) = v {
                let fty = fv.get_type();
                let r = match method {
                    "recip" => {
                        let one = fty.const_float(1.0);
                        self.builder.build_float_div(one, fv, "recip").unwrap()
                    }
                    "to_degrees" => {
                        // Rust's `PIS_IN_180` (180/π) — the f64 nearest to the
                        // `57.2957795130823208767981548141051703` literal that
                        // `f64::to_degrees` multiplies by. `const_float` rounds
                        // it to the receiver width.
                        let c = fty.const_float(57.29577951308232);
                        self.builder.build_float_mul(fv, c, "to_deg").unwrap()
                    }
                    "to_radians" => {
                        // Rust's `RADS_PER_DEG` = π / 180.
                        let c = fty.const_float(std::f64::consts::PI / 180.0);
                        self.builder.build_float_mul(fv, c, "to_rad").unwrap()
                    }
                    _ => {
                        // `fract` = `x - trunc(x)` (round toward zero).
                        let intrinsic = inkwell::intrinsics::Intrinsic::find("llvm.trunc")
                            .unwrap_or_else(|| panic!("llvm.trunc intrinsic must exist"));
                        let decl = intrinsic
                            .get_declaration(&self.module, &[fty.into()])
                            .unwrap_or_else(|| panic!("llvm.trunc declaration for float type"));
                        let truncated = self
                            .builder
                            .build_call(decl, &[fv.into()], "fract.tr")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic()
                            .into_float_value();
                        self.builder
                            .build_float_sub(fv, truncated, "fract")
                            .unwrap()
                    }
                };
                return Ok(r.into());
            }
        }

        // `min` / `max` on a numeric scalar (typed in expr_method_call.rs):
        // ints → `select` on a signed/unsigned `icmp`; floats → the overloaded
        // `llvm.minnum`/`llvm.maxnum` intrinsics (NaN-quieting, matching Rust
        // `f64::min`/`max` and the interpreter's `f64::min`/`max`).
        if matches!(method, "min" | "max") && args.len() == 1 {
            let a = self.compile_expr(object)?;
            let b = self.compile_expr(&args[0].value)?;
            match (a, b) {
                (BasicValueEnum::IntValue(av), BasicValueEnum::IntValue(mut bv)) => {
                    // Harmonize a bare-literal arg (default i64) down/up to the
                    // receiver width so the `icmp` operands match.
                    let aw = av.get_type().get_bit_width();
                    let bw = bv.get_type().get_bit_width();
                    if bw != aw {
                        bv = if bw > aw {
                            self.builder
                                .build_int_truncate(bv, av.get_type(), "mm.tr")
                                .unwrap()
                        } else if self.expr_is_unsigned_int(object) {
                            self.builder
                                .build_int_z_extend(bv, av.get_type(), "mm.zx")
                                .unwrap()
                        } else {
                            self.builder
                                .build_int_s_extend(bv, av.get_type(), "mm.sx")
                                .unwrap()
                        };
                    }
                    let unsigned = self.expr_is_unsigned_int(object);
                    let pred = match (method, unsigned) {
                        ("min", false) => IntPredicate::SLT,
                        ("max", false) => IntPredicate::SGT,
                        ("min", true) => IntPredicate::ULT,
                        _ => IntPredicate::UGT,
                    };
                    let cmp = self
                        .builder
                        .build_int_compare(pred, av, bv, "mm.cmp")
                        .unwrap();
                    let r = self.builder.build_select(cmp, av, bv, method).unwrap();
                    return Ok(r);
                }
                (BasicValueEnum::FloatValue(av), BasicValueEnum::FloatValue(bv)) => {
                    let iname = if method == "min" {
                        "llvm.minnum"
                    } else {
                        "llvm.maxnum"
                    };
                    let intrinsic = inkwell::intrinsics::Intrinsic::find(iname)
                        .unwrap_or_else(|| panic!("{iname} intrinsic must exist"));
                    let decl = intrinsic
                        .get_declaration(&self.module, &[av.get_type().into()])
                        .unwrap_or_else(|| panic!("{iname} declaration for float type"));
                    let r = self
                        .builder
                        .build_call(decl, &[av.into(), bv.into()], "fmm")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic();
                    return Ok(r);
                }
                _ => {}
            }
        }

        // `clamp` on a numeric scalar (typed in expr_method_call.rs):
        // `v.clamp(lo, hi)` lowers to the nested-bound `select(v < lo, lo,
        // select(v > hi, hi, v))` — `lo` wins on an inverted range, matching
        // the interpreter and the `clamp` free fn. Ints use signed/unsigned
        // `icmp`; floats use ordered `fcmp` (NaN `v` returns `v`, as in Rust).
        if method == "clamp" && args.len() == 2 {
            let v = self.compile_expr(object)?;
            let lo = self.compile_expr(&args[0].value)?;
            let hi = self.compile_expr(&args[1].value)?;
            match (v, lo, hi) {
                (
                    BasicValueEnum::IntValue(vv),
                    BasicValueEnum::IntValue(lov),
                    BasicValueEnum::IntValue(hiv),
                ) => {
                    // Harmonize a bare-literal bound (default i64) to the
                    // receiver width so the `icmp` operands match.
                    let vw = vv.get_type().get_bit_width();
                    let unsigned = self.expr_is_unsigned_int(object);
                    let harmonize = |b: inkwell::values::IntValue<'ctx>| {
                        let bw = b.get_type().get_bit_width();
                        if bw == vw {
                            b
                        } else if bw > vw {
                            self.builder
                                .build_int_truncate(b, vv.get_type(), "cl.tr")
                                .unwrap()
                        } else if unsigned {
                            self.builder
                                .build_int_z_extend(b, vv.get_type(), "cl.zx")
                                .unwrap()
                        } else {
                            self.builder
                                .build_int_s_extend(b, vv.get_type(), "cl.sx")
                                .unwrap()
                        }
                    };
                    let lov = harmonize(lov);
                    let hiv = harmonize(hiv);
                    let (lt, gt) = if unsigned {
                        (IntPredicate::ULT, IntPredicate::UGT)
                    } else {
                        (IntPredicate::SLT, IntPredicate::SGT)
                    };
                    let v_gt_hi = self
                        .builder
                        .build_int_compare(gt, vv, hiv, "cl.gt")
                        .unwrap();
                    let upper = self
                        .builder
                        .build_select(v_gt_hi, hiv, vv, "cl.hi")
                        .unwrap();
                    let v_lt_lo = self
                        .builder
                        .build_int_compare(lt, vv, lov, "cl.lt")
                        .unwrap();
                    let r = self
                        .builder
                        .build_select(v_lt_lo, lov.into(), upper, "clamp")
                        .unwrap();
                    return Ok(r);
                }
                (
                    BasicValueEnum::FloatValue(vv),
                    BasicValueEnum::FloatValue(lov),
                    BasicValueEnum::FloatValue(hiv),
                ) => {
                    let v_gt_hi = self
                        .builder
                        .build_float_compare(inkwell::FloatPredicate::OGT, vv, hiv, "cl.gt")
                        .unwrap();
                    let upper = self
                        .builder
                        .build_select(v_gt_hi, hiv, vv, "cl.hi")
                        .unwrap();
                    let v_lt_lo = self
                        .builder
                        .build_float_compare(inkwell::FloatPredicate::OLT, vv, lov, "cl.lt")
                        .unwrap();
                    let r = self
                        .builder
                        .build_select(v_lt_lo, lov.into(), upper, "clamp")
                        .unwrap();
                    return Ok(r);
                }
                _ => {}
            }
        }

        // Built-in `sqrt` on float primitives (typed in expr_method_call.rs):
        // `x.sqrt() -> Self`, lowered to the overloaded `llvm.sqrt` intrinsic —
        // a single `f64.sqrt` instruction on wasm (and `sqrtsd` on x86), no
        // libm dependency. Float-only; other receivers fall through.
        if method == "sqrt" && args.is_empty() {
            let v = self.compile_expr(object)?;
            if let BasicValueEnum::FloatValue(fv) = v {
                let intrinsic = inkwell::intrinsics::Intrinsic::find("llvm.sqrt")
                    .expect("llvm.sqrt intrinsic must exist");
                let decl = intrinsic
                    .get_declaration(&self.module, &[fv.get_type().into()])
                    .expect("llvm.sqrt declaration for float type");
                let r = self
                    .builder
                    .build_call(decl, &[fv.into()], "fsqrt")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                return Ok(r);
            }
        }

        // IEEE-754 bit reinterpretation (typed in expr_method_call.rs; mirrors
        // the interpreter arm in `interpreter/method_call.rs`). Pure bitcasts —
        // no runtime helper, no allocation, no new C symbol. Until now these had
        // an interpreter + typechecker implementation but no codegen arm, so a
        // program that round-tripped an f64 through its bits ran under
        // `karac run` but failed `karac build` with "no handler for method
        // 'to_bits'" — a run/build divergence (surfaced by the LeetCode #50
        // Pow(x, n) benchmark's XOR-fold sink; ledger B-2026-07-03-1).
        //   `to_bits`     f64 → u64  : bitcast f64→i64
        //   `to_bits32`   f{32,64} → u32 : round to f32, bitcast→i32, zext→i64
        //   `bits_as_f64` int → f64  : width-normalize to i64, bitcast→f64
        //   `bits_as_f32` int → f32  : width-normalize to i32, bitcast→f32
        // Float-only for `to_bits*`, int-only for `bits_as_*`; other receivers
        // fall through to normal dispatch.
        if args.is_empty() && matches!(method, "to_bits" | "to_bits32") {
            let v = self.compile_expr(object)?;
            if let BasicValueEnum::FloatValue(fv) = v {
                let i64_t = self.context.i64_type();
                if method == "to_bits" {
                    let bits = self
                        .builder
                        .build_bit_cast(fv, i64_t, "to_bits")
                        .unwrap()
                        .into_int_value();
                    return Ok(bits.into());
                }
                // to_bits32: round the value to f32 first (identity if it already
                // is one), then take its 32-bit pattern, zero-extended into the
                // i64-backed integer representation.
                let f32_t = self.context.f32_type();
                let f32v = if fv.get_type() == f32_t {
                    fv
                } else {
                    self.builder.build_float_trunc(fv, f32_t, "to_f32").unwrap()
                };
                let bits32 = self
                    .builder
                    .build_bit_cast(f32v, self.context.i32_type(), "to_bits32")
                    .unwrap()
                    .into_int_value();
                let bits = self
                    .builder
                    .build_int_z_extend(bits32, i64_t, "to_bits32.zext")
                    .unwrap();
                return Ok(bits.into());
            }
        }
        if args.is_empty() && matches!(method, "bits_as_f64" | "bits_as_f32") {
            let v = self.compile_expr(object)?;
            if let BasicValueEnum::IntValue(iv) = v {
                // bits_as_f64 reads the low 64 bits, bits_as_f32 the low 32 —
                // width-normalize the receiver to exactly that many bits
                // (zero-extend if narrower, truncate if wider) before the cast.
                let (int_t, float_t, name) = if method == "bits_as_f64" {
                    (self.context.i64_type(), self.context.f64_type(), 64u32)
                } else {
                    (self.context.i32_type(), self.context.f32_type(), 32u32)
                };
                let w = iv.get_type().get_bit_width();
                let norm = if w == name {
                    iv
                } else if w < name {
                    self.builder
                        .build_int_z_extend(iv, int_t, "bits.zext")
                        .unwrap()
                } else {
                    self.builder
                        .build_int_truncate(iv, int_t, "bits.trunc")
                        .unwrap()
                };
                let f = self.builder.build_bit_cast(norm, float_t, method).unwrap();
                return Ok(f);
            }
        }

        // Built-in scalar transcendental + rounding math on float primitives
        // (typed in expr_method_call.rs; surface in `crate::float_math`): unary
        // `sin`/`cos`/`tan`/`asin`/`acos`/`atan`/`sinh`/`cosh`/`tanh`/`exp`/
        // `exp2`/`ln`/`log2`/`log10`/`floor`/`ceil`/`round`/`trunc` and binary
        // `pow`/`atan2`. Most lower to their overloaded LLVM intrinsic, which
        // becomes a libm call on most targets — and on wasm too, where the math
        // symbols live in wasi-libc's `libc.a` (already linked by the wasm-ld
        // path), so no archive/`--export` work is needed. The exceptions are the
        // functions whose LLVM intrinsic is LLVM-19+ (absent on the 18.1 pin) —
        // `tan`/`atan2` and the inverse-trig / hyperbolic set — which lower to a
        // direct width-correct libm call (`tan`/`tanf`, `asin`/`asinf`, …).
        // Float-only; a non-float receiver (e.g. a user type with its own
        // `round` method) falls through to normal dispatch.
        if let Some(kind) = crate::float_math::classify(method) {
            let v = self.compile_expr(object)?;
            if let BasicValueEnum::FloatValue(fv) = v {
                let fty = fv.get_type();
                let is_f32 = fty == self.context.f32_type();
                // `tan` / `atan2` have no LLVM-18 intrinsic — call libm directly,
                // picking the width-correct symbol (`f`-suffixed for f32).
                let libm_sym = match (method, is_f32) {
                    ("tan", false) => Some("tan"),
                    ("tan", true) => Some("tanf"),
                    ("atan2", false) => Some("atan2"),
                    ("atan2", true) => Some("atan2f"),
                    // Inverse-trig / hyperbolic: no LLVM-18 intrinsic, call libm
                    // directly (width-correct `f`-suffixed symbol for f32).
                    ("asin", false) => Some("asin"),
                    ("asin", true) => Some("asinf"),
                    ("acos", false) => Some("acos"),
                    ("acos", true) => Some("acosf"),
                    ("atan", false) => Some("atan"),
                    ("atan", true) => Some("atanf"),
                    ("sinh", false) => Some("sinh"),
                    ("sinh", true) => Some("sinhf"),
                    ("cosh", false) => Some("cosh"),
                    ("cosh", true) => Some("coshf"),
                    ("tanh", false) => Some("tanh"),
                    ("tanh", true) => Some("tanhf"),
                    ("asinh", false) => Some("asinh"),
                    ("asinh", true) => Some("asinhf"),
                    ("acosh", false) => Some("acosh"),
                    ("acosh", true) => Some("acoshf"),
                    ("atanh", false) => Some("atanh"),
                    ("atanh", true) => Some("atanhf"),
                    ("hypot", false) => Some("hypot"),
                    ("hypot", true) => Some("hypotf"),
                    // Rust's `exp_m1` / `ln_1p` are libm's `expm1` / `log1p`.
                    ("exp_m1", false) => Some("expm1"),
                    ("exp_m1", true) => Some("expm1f"),
                    ("ln_1p", false) => Some("log1p"),
                    ("ln_1p", true) => Some("log1pf"),
                    _ => None,
                };
                if let Some(sym) = libm_sym {
                    let mut call_args = vec![fv.into()];
                    let mut params = vec![fty.into()];
                    if matches!(kind, crate::float_math::FloatMathKind::Binary) {
                        let BasicValueEnum::FloatValue(yv) = self.compile_expr(&args[0].value)?
                        else {
                            panic!(
                                "{method} argument must be a float value (typechecker invariant)"
                            );
                        };
                        call_args.push(yv.into());
                        params.push(fty.into());
                    }
                    let fn_val = match self.module.get_function(sym) {
                        Some(f) => f,
                        None => {
                            let fn_ty = fty.fn_type(&params, false);
                            self.module.add_function(sym, fn_ty, None)
                        }
                    };
                    let r = self
                        .builder
                        .build_call(fn_val, &call_args, "flibm")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic();
                    return Ok(r);
                }
                let intrinsic_name = match method {
                    "sin" => "llvm.sin",
                    "cos" => "llvm.cos",
                    "exp" => "llvm.exp",
                    "ln" => "llvm.log",
                    "log2" => "llvm.log2",
                    "floor" => "llvm.floor",
                    "ceil" => "llvm.ceil",
                    "round" => "llvm.round",
                    "pow" => "llvm.pow",
                    "exp2" => "llvm.exp2",
                    "log10" => "llvm.log10",
                    "trunc" => "llvm.trunc",
                    "copysign" => "llvm.copysign",
                    _ => unreachable!("float_math codegen classify/match drift"),
                };
                let intrinsic = inkwell::intrinsics::Intrinsic::find(intrinsic_name)
                    .unwrap_or_else(|| panic!("{intrinsic_name} intrinsic must exist"));
                let decl = intrinsic
                    .get_declaration(&self.module, &[fty.into()])
                    .unwrap_or_else(|| panic!("{intrinsic_name} declaration for float type"));
                let r = match kind {
                    crate::float_math::FloatMathKind::Binary => {
                        let av = self.compile_expr(&args[0].value)?;
                        self.builder
                            .build_call(decl, &[fv.into(), av.into()], "fmath")
                    }
                    crate::float_math::FloatMathKind::Unary => {
                        self.builder.build_call(decl, &[fv.into()], "fmath")
                    }
                }
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic();
                return Ok(r);
            }
        }

        // Wrapping integer arithmetic (typed in expr_method_call.rs):
        // `wrapping_add` / `wrapping_sub` / `wrapping_mul`, the non-trapping
        // sibling of the checked `+`/`-`/`*` path. Lowers to a bare
        // `build_int_{add,sub,mul}` — silent two's-complement wraparound, no
        // `with.overflow` intrinsic and no trap branch (cf.
        // `emit_checked_int_arith` in expr_ops.rs). A straight-line loop body
        // with no per-element overflow-trap side-exit is precisely what lets
        // LLVM auto-vectorize integer slice kernels (the trap branch is the
        // proven vectorization blocker — roadmap.md § Codegen Optimization).
        // Width handling (B-2026-08-19-1). The receiver may be any integer
        // width now, and NEITHER operand can be assumed to already carry it:
        // codegen's narrow-int model normalizes to an i64 carrier
        // (`compile_narrow_int_binop`), while a narrow function PARAMETER is a
        // real LLVM `i32`. Mixing the two is how the first cut of this widening
        // produced `add i32 %x, i64 1` and failed module verification. So:
        // normalize both sides to i64, do the arithmetic there (matching the
        // interpreter, which is i64-backed), then reduce into the receiver's
        // declared width — the same shape `compile_narrow_int_binop` uses,
        // except this family WRAPS where that one traps.
        if matches!(method, "wrapping_add" | "wrapping_sub" | "wrapping_mul") && args.len() == 1 {
            let (bits, is_unsigned) = self.receiver_int_kind(object, call_span, method);
            let lv_raw = self.compile_expr(object)?;
            let rv_raw = self.compile_expr(&args[0].value)?;
            let lv = self.widen_int_to_i64(lv_raw, is_unsigned);
            let rv = self.widen_int_to_i64(rv_raw, is_unsigned);
            let wide = match method {
                "wrapping_add" => self.builder.build_int_add(lv, rv, "wadd"),
                "wrapping_sub" => self.builder.build_int_sub(lv, rv, "wsub"),
                "wrapping_mul" => self.builder.build_int_mul(lv, rv, "wmul"),
                _ => unreachable!("outer matches! restricts to the three methods"),
            }
            .unwrap();
            // 64-bit needs no reduction: the i64 carrier IS the width, and the
            // masks below would shift by 64 (poison in LLVM, UB in Rust).
            let reduced = if bits >= 64 {
                wide
            } else {
                let i64_t = self.context.i64_type();
                let mask = i64_t.const_int((1u64 << bits) - 1, false);
                let masked = self.builder.build_and(wide, mask, "wmask").unwrap();
                if is_unsigned {
                    masked
                } else {
                    // Sign-extend out of the width: shift the sign bit up to
                    // bit 63 and arithmetic-shift back, so the i64 carrier
                    // holds the same value the narrower type would.
                    let sh = i64_t.const_int((64 - bits) as u64, false);
                    let up = self.builder.build_left_shift(masked, sh, "wshl").unwrap();
                    self.builder
                        .build_right_shift(up, sh, true, "wsar")
                        .unwrap()
                }
            };
            return Ok(reduced.into());
        }

        // Euclidean division / remainder on `i64` (typed in expr_method_call.rs,
        // i64-only in this slice): `div_euclid` / `rem_euclid`. `emit_int_div_guards`
        // first traps the exact set the interpreter's `checked_*_euclid` reject
        // (`division by zero`, `i64::MIN / -1` → `integer overflow`); the
        // signed correction then matches Rust: the remainder is made
        // non-negative and the quotient adjusted toward negative infinity.
        if matches!(method, "div_euclid" | "rem_euclid") && args.len() == 1 {
            let lv = self.compile_expr(object)?.into_int_value();
            let rv = self.compile_expr(&args[0].value)?.into_int_value();
            self.emit_int_div_guards(lv, rv, false);
            let ty = lv.get_type();
            let zero = ty.const_zero();
            let one = ty.const_int(1, false);
            let rem = self
                .builder
                .build_int_signed_rem(lv, rv, "eucl.rem")
                .unwrap();
            let rem_neg = self
                .builder
                .build_int_compare(IntPredicate::SLT, rem, zero, "eucl.rneg")
                .unwrap();
            let r = if method == "rem_euclid" {
                // rem < 0 → `rem - rhs` when rhs < 0, else `rem + rhs`.
                let rhs_neg = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, rv, zero, "eucl.yneg")
                    .unwrap();
                let add = self.builder.build_int_add(rem, rv, "eucl.add").unwrap();
                let sub = self.builder.build_int_sub(rem, rv, "eucl.sub").unwrap();
                let corrected = self
                    .builder
                    .build_select(rhs_neg, sub, add, "eucl.corr")
                    .unwrap();
                self.builder
                    .build_select(rem_neg, corrected, rem.into(), "rem_euclid")
                    .unwrap()
            } else {
                // q = x / y; if rem < 0 then `q - 1` (rhs > 0) / `q + 1` (rhs < 0).
                let q = self.builder.build_int_signed_div(lv, rv, "eucl.q").unwrap();
                let rhs_pos = self
                    .builder
                    .build_int_compare(IntPredicate::SGT, rv, zero, "eucl.ypos")
                    .unwrap();
                let q_dec = self.builder.build_int_sub(q, one, "eucl.qdec").unwrap();
                let q_inc = self.builder.build_int_add(q, one, "eucl.qinc").unwrap();
                let adj = self
                    .builder
                    .build_select(rhs_pos, q_dec, q_inc, "eucl.adj")
                    .unwrap();
                self.builder
                    .build_select(rem_neg, adj, q.into(), "div_euclid")
                    .unwrap()
            };
            return Ok(r);
        }

        // Integer `.pow(exp)` (typed in expr_method_call.rs): `n.pow(k) -> Self`,
        // a repeated-multiply loop whose body reuses the `*` operator's
        // overflow-trapping multiply (`emit_checked_int_arith("mul", …)`), so an
        // out-of-range partial product traps `integer overflow` at the receiver
        // width exactly as `*` does. `acc` starts at 1; the `u32` exponent counts
        // the multiplications (`acc *= base`, `exp` times). Both operands stay at
        // the receiver's iN width; `exp == 0` yields `1`.
        if method == "pow" && args.len() == 1 {
            // Codegen widens narrow integers to i64 in value flow, so the receiver
            // width is recovered from the typechecker's callee record, not the
            // compiled value's type. The base is narrowed to that width so the
            // per-step trap fires at the declared width; the result is re-extended
            // to the i64-backed representation narrow integers flow in.
            let (bits, unsigned) = self.receiver_int_kind(object, call_span, "pow");
            let int_ty = self.int_type_for_bits(bits);
            let base_raw = self.compile_expr(object)?.into_int_value();
            let base = self.coerce_int_to(base_raw, int_ty, unsigned);
            let exp = self.compile_expr(&args[0].value)?.into_int_value();
            let exp_ty = exp.get_type();
            let fn_val = self.current_fn.unwrap();

            let acc_slot = self.create_entry_alloca(fn_val, "pow.acc", int_ty.into());
            self.builder
                .build_store(acc_slot, int_ty.const_int(1, false))
                .unwrap();
            let i_slot = self.create_entry_alloca(fn_val, "pow.i", exp_ty.into());
            self.builder
                .build_store(i_slot, exp_ty.const_zero())
                .unwrap();

            let cond_bb = self.context.append_basic_block(fn_val, "pow.cond");
            let body_bb = self.context.append_basic_block(fn_val, "pow.body");
            let exit_bb = self.context.append_basic_block(fn_val, "pow.exit");
            self.builder.build_unconditional_branch(cond_bb).unwrap();

            // cond: i < exp (unsigned)
            self.builder.position_at_end(cond_bb);
            let i_cur = self
                .builder
                .build_load(exp_ty, i_slot, "pow.i.cur")
                .unwrap()
                .into_int_value();
            let go = self
                .builder
                .build_int_compare(IntPredicate::ULT, i_cur, exp, "pow.lt")
                .unwrap();
            self.builder
                .build_conditional_branch(go, body_bb, exit_bb)
                .unwrap();

            // body: acc = checked_mul(acc, base); i += 1  (the trapping mul
            // appends its own ok/trap blocks and leaves the builder on the ok
            // continuation, where the loop's increment + back-branch are emitted).
            self.builder.position_at_end(body_bb);
            let acc_cur = self
                .builder
                .build_load(int_ty, acc_slot, "pow.acc.cur")
                .unwrap()
                .into_int_value();
            let prod = self.emit_checked_int_arith("mul", acc_cur, base, unsigned)?;
            self.builder.build_store(acc_slot, prod).unwrap();
            let i_now = self
                .builder
                .build_load(exp_ty, i_slot, "pow.i.now")
                .unwrap()
                .into_int_value();
            let i_next = self
                .builder
                .build_int_add(i_now, exp_ty.const_int(1, false), "pow.i.next")
                .unwrap();
            self.builder.build_store(i_slot, i_next).unwrap();
            self.builder.build_unconditional_branch(cond_bb).unwrap();

            self.builder.position_at_end(exit_bb);
            let acc_final = self
                .builder
                .build_load(int_ty, acc_slot, "pow.result")
                .unwrap()
                .into_int_value();
            let result =
                self.coerce_int_to(acc_final, self.int_carrier_type_for_bits(bits), unsigned);
            return Ok(result.into());
        }

        // `<c-like enum>.discriminant() -> D` — design.md § Enum Discriminant
        // Runtime Surface (B-2026-08-21-10).
        //
        // A select-chain over the tag rather than a bare tag read, because the
        // tag is the DECLARATION POSITION and the answer is the DECLARED value
        // — `#[repr(u8)] enum UsbClass { Audio = 0x01, Hid = 0x03 }` has tags
        // 0 and 1 and must answer 1 and 3. design.md is explicit that declared
        // discriminants are not layout commitments at v1, so the mapping has
        // to happen here rather than by laying the enum out at those values.
        // Same shape as `compile_unit_enum_display`, which folds a select
        // chain over the same tag to pick a variant NAME.
        //
        // The result is truncated to the repr width so `.discriminant()` on a
        // `#[repr(u8)]` enum really is a `u8`; the values themselves come from
        // the typechecker's folded table, never from a second fold here.
        if args.is_empty() && method == "discriminant" {
            if let Some(enum_name) = self.expr_user_enum_name(object) {
                if let Some((repr, values)) =
                    self.type_decls.enum_discriminants.get(&enum_name).cloned()
                {
                    let val = self.compile_expr(object)?;
                    let tag = match val {
                        BasicValueEnum::IntValue(iv) => iv,
                        BasicValueEnum::StructValue(sv) => self
                            .builder
                            .build_extract_value(sv, 0, "disc.tag")
                            .unwrap()
                            .into_int_value(),
                        other => {
                            return Err(format!(
                                "discriminant: enum '{enum_name}' value has unexpected                                  representation {other:?}"
                            ))
                        }
                    };
                    let i64_t = self.context.i64_type();
                    let mut acc: Option<inkwell::values::IntValue<'ctx>> = None;
                    for (vname, dval) in &values {
                        let tagval = *self
                            .type_decls
                            .enum_layouts
                            .get(&enum_name)
                            .and_then(|l| l.tags.get(vname))
                            .ok_or_else(|| {
                                format!("discriminant: missing tag for {enum_name}.{vname}")
                            })?;
                        let cand = i64_t.const_int(*dval as u64, true);
                        acc = Some(match acc {
                            // The first variant is the default: the tag is
                            // always one of the exhaustive 0..N range, so no
                            // select is needed for it.
                            None => cand,
                            Some(prev) => {
                                let is_v = self
                                    .builder
                                    .build_int_compare(
                                        IntPredicate::EQ,
                                        tag,
                                        i64_t.const_int(tagval, false),
                                        "disc.is",
                                    )
                                    .unwrap();
                                self.builder
                                    .build_select(is_v, cand, prev, "disc.sel")
                                    .unwrap()
                                    .into_int_value()
                            }
                        });
                    }
                    let raw = acc.ok_or_else(|| {
                        format!("discriminant: enum '{enum_name}' has no variants")
                    })?;
                    let (bits, unsigned) = match repr.as_str() {
                        "i8" => (8u32, false),
                        "i16" => (16, false),
                        "i32" => (32, false),
                        "i64" => (64, false),
                        "u8" => (8, true),
                        "u16" => (16, true),
                        "u64" => (64, true),
                        _ => (32, true),
                    };
                    let narrowed = self.coerce_int_to(raw, self.int_type_for_bits(bits), unsigned);
                    return Ok(narrowed.into());
                }
            }
        }

        // `to_ne_bytes()` (typed in method_numeric.rs) -> `Array[u8, N]`, the
        // receiver's NATIVE-order memory image (B-2026-08-21-10).
        //
        // Lowered as store-then-reload rather than a shift/mask chain: the
        // integer is narrowed to its declared width, stored into an alloca of
        // that type, and read back as `[N x i8]`. That IS the native byte
        // order by construction — no endianness constant is baked into the
        // compiler, so the same code is correct on a big-endian target, and it
        // matches the interpreter, which takes the same N bytes of the value's
        // `to_ne_bytes` image. The alloca is aligned for the integer type, so
        // the i8-array reload needs no lower alignment claim.
        if args.is_empty() && method == "to_ne_bytes" {
            let (bits, unsigned) = self.receiver_int_kind(object, call_span, method);
            let int_ty = self.int_type_for_bits(bits);
            let v_raw = self.compile_expr(object)?.into_int_value();
            let v = self.coerce_int_to(v_raw, int_ty, unsigned);
            let fn_val = self
                .current_fn
                .ok_or_else(|| "to_ne_bytes outside a function".to_string())?;
            let slot = self.create_entry_alloca(fn_val, "tneb.slot", int_ty.into());
            self.builder.build_store(slot, v).unwrap();
            let arr_ty = self.context.i8_type().array_type(bits / 8);
            return Ok(self.builder.build_load(arr_ty, slot, "tneb").unwrap());
        }

        // Bit intrinsics (typed in expr_method_call.rs): `count_ones` /
        // `leading_zeros` / `trailing_zeros` -> u32, lowered to the overloaded
        // `llvm.ctpop` / `llvm.ctlz` / `llvm.cttz` intrinsics. The receiver is
        // narrowed to its declared width first (codegen widens narrow ints to
        // i64, which would otherwise count over 64 bits); the intrinsic is then
        // width-correct. `ctlz` / `cttz` take an `is_zero_poison` i1 (`false` →
        // defined to return the bit width on a zero input, matching Rust and the
        // interpreter). The non-negative count is z-extended to the i64-backed
        // representation the `u32` result flows in.
        if args.is_empty()
            && matches!(
                method,
                "count_ones" | "count_zeros" | "leading_zeros" | "trailing_zeros"
            )
        {
            let (bits, unsigned) = self.receiver_int_kind(object, call_span, method);
            let int_ty = self.int_type_for_bits(bits);
            let v_raw = self.compile_expr(object)?.into_int_value();
            let v = self.coerce_int_to(v_raw, int_ty, unsigned);
            // `count_zeros` has no direct intrinsic — it is `bits - popcount`.
            let (base_name, is_clz_ctz) = match method {
                "count_ones" | "count_zeros" => ("llvm.ctpop", false),
                "leading_zeros" => ("llvm.ctlz", true),
                "trailing_zeros" => ("llvm.cttz", true),
                _ => unreachable!(),
            };
            let intrinsic = inkwell::intrinsics::Intrinsic::find(base_name)
                .ok_or_else(|| format!("{base_name} intrinsic must exist in LLVM"))?;
            let decl = intrinsic
                .get_declaration(&self.module, &[int_ty.into()])
                .ok_or_else(|| format!("{base_name} has no declaration for width {bits}"))?;
            let mut raw = if is_clz_ctz {
                let no_poison = self.context.bool_type().const_zero();
                self.builder
                    .build_call(decl, &[v.into(), no_poison.into()], "bitintr")
            } else {
                self.builder.build_call(decl, &[v.into()], "bitintr")
            }
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
            if method == "count_zeros" {
                let bits_c = int_ty.const_int(u64::from(bits), false);
                raw = self.builder.build_int_sub(bits_c, raw, "czero").unwrap();
            }
            let i64_t = self.context.i64_type();
            // The count is non-negative and ≤ 64, so a zero-extend is always
            // correct regardless of the receiver's signedness.
            let res = self.coerce_int_to(raw, i64_t, true);
            return Ok(res.into());
        }

        // Bit-permutation intrinsics `reverse_bits` / `swap_bytes` -> Self
        // (typed in expr_method_call.rs). Lowered on the receiver's declared iN
        // width to `llvm.bitreverse` / `llvm.bswap`; `swap_bytes` on an 8-bit
        // receiver is identity (`llvm.bswap` requires ≥ i16). The iN result is
        // re-extended to the i64-backed representation with the receiver's
        // signedness (sign-extend signed, zero-extend unsigned) so it matches
        // the interpreter's `eval_bit_permute` encoding.
        if args.is_empty() && matches!(method, "reverse_bits" | "swap_bytes") {
            let (bits, unsigned) = self.receiver_int_kind(object, call_span, method);
            let int_ty = self.int_type_for_bits(bits);
            let v_raw = self.compile_expr(object)?.into_int_value();
            let v = self.coerce_int_to(v_raw, int_ty, unsigned);
            let permuted = if method == "swap_bytes" && bits <= 8 {
                v
            } else {
                let base_name = if method == "reverse_bits" {
                    "llvm.bitreverse"
                } else {
                    "llvm.bswap"
                };
                let intrinsic = inkwell::intrinsics::Intrinsic::find(base_name)
                    .ok_or_else(|| format!("{base_name} intrinsic must exist in LLVM"))?;
                let decl = intrinsic
                    .get_declaration(&self.module, &[int_ty.into()])
                    .ok_or_else(|| format!("{base_name} has no declaration for width {bits}"))?;
                self.builder
                    .build_call(decl, &[v.into()], "bitperm")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value()
            };
            let res = self.coerce_int_to(permuted, self.int_carrier_type_for_bits(bits), unsigned);
            return Ok(res.into());
        }

        // Bit-rotation intrinsics `rotate_left(n)` / `rotate_right(n)` -> Self
        // (typed in expr_method_call.rs). A rotate is a funnel shift with both
        // inputs equal: `rotate_left` = `llvm.fshl(x, x, n)`, `rotate_right` =
        // `llvm.fshr(x, x, n)`. All three operands are the receiver's iN; the
        // shift `n` (a `u32` in Kāra) is truncated to iN — `fshl`/`fshr` take it
        // mod width, matching Rust's `rotate_*`. The result is re-extended to
        // the i64-backed value with the receiver's signedness.
        if args.len() == 1 && matches!(method, "rotate_left" | "rotate_right") {
            let (bits, unsigned) = self.receiver_int_kind(object, call_span, method);
            let int_ty = self.int_type_for_bits(bits);
            let v_raw = self.compile_expr(object)?.into_int_value();
            let v = self.coerce_int_to(v_raw, int_ty, unsigned);
            let amt_raw = self.compile_expr(&args[0].value)?.into_int_value();
            // The amount is unsigned (a bit count), so a zero-extend/truncate to
            // the receiver width is correct.
            let amt = self.coerce_int_to(amt_raw, int_ty, true);
            let base_name = if method == "rotate_left" {
                "llvm.fshl"
            } else {
                "llvm.fshr"
            };
            let intrinsic = inkwell::intrinsics::Intrinsic::find(base_name)
                .ok_or_else(|| format!("{base_name} intrinsic must exist in LLVM"))?;
            let decl = intrinsic
                .get_declaration(&self.module, &[int_ty.into()])
                .ok_or_else(|| format!("{base_name} has no declaration for width {bits}"))?;
            let rotated = self
                .builder
                .build_call(decl, &[v.into(), v.into(), amt.into()], "bitrot")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let res = self.coerce_int_to(rotated, self.int_carrier_type_for_bits(bits), unsigned);
            return Ok(res.into());
        }

        // `is_power_of_two` on unsigned integer scalars -> bool (i1) (typed in
        // expr_method_call.rs). Inline `(x != 0) & ((x & (x-1)) == 0)`, computed
        // on the receiver's declared iN width (zero-extended into the value, so a
        // narrow receiver's high bits are already clear). No intrinsic, no runtime
        // extern; matches the interpreter's single-bit test.
        if args.is_empty() && method == "is_power_of_two" {
            let (bits, unsigned) = self.receiver_int_kind(object, call_span, method);
            let int_ty = self.int_type_for_bits(bits);
            let v_raw = self.compile_expr(object)?.into_int_value();
            let v = self.coerce_int_to(v_raw, int_ty, unsigned);
            let zero = int_ty.const_zero();
            let one = int_ty.const_int(1, false);
            let nonzero = self
                .builder
                .build_int_compare(IntPredicate::NE, v, zero, "ipot.nz")
                .unwrap();
            let minus1 = self.builder.build_int_sub(v, one, "ipot.m1").unwrap();
            let anded = self.builder.build_and(v, minus1, "ipot.and").unwrap();
            let no_low = self
                .builder
                .build_int_compare(IntPredicate::EQ, anded, zero, "ipot.z")
                .unwrap();
            let r = self.builder.build_and(nonzero, no_low, "ipot").unwrap();
            return Ok(r.into());
        }

        // `next_power_of_two` on unsigned integer scalars -> Self (typed in
        // expr_method_call.rs). The smallest power of two ≥ self (0 and 1 → 1),
        // on the receiver's iN. Clamp m to ≥ 1 (so the `ctlz` shift is always
        // defined — 0 and 1 both map to 1), trap `integer overflow` when
        // `m > 2^(bits-1)` (the result would be the unrepresentable 2^bits),
        // else `1 << (bits - ctlz(m - 1))`. Zero-extended to the i64-backed
        // unsigned result; matches the interpreter's `next_power_of_two`.
        if args.is_empty() && method == "next_power_of_two" {
            let (bits, unsigned) = self.receiver_int_kind(object, call_span, method);
            let int_ty = self.int_type_for_bits(bits);
            let m_raw = self.compile_expr(object)?.into_int_value();
            let m0 = self.coerce_int_to(m_raw, int_ty, unsigned);
            let one = int_ty.const_int(1, false);
            // Clamp 0 → 1 so `m - 1` is never all-ones and the shift stays in
            // range; `next_power_of_two(0) == 1` anyway.
            let is_zero = self
                .builder
                .build_int_compare(IntPredicate::EQ, m0, int_ty.const_zero(), "npot.z")
                .unwrap();
            let m = self
                .builder
                .build_select(is_zero, one, m0, "npot.m")
                .unwrap()
                .into_int_value();
            // Overflow iff m > 2^(bits-1): the next power of two would be 2^bits.
            // The threshold's bit pattern (0x80…0 at bits==64) compares unsigned.
            let half = int_ty.const_int(1u64 << (bits - 1), false);
            let is_ovf = self
                .builder
                .build_int_compare(IntPredicate::UGT, m, half, "npot.ovf")
                .unwrap();
            let fn_val = self.current_fn.unwrap();
            let trap_bb = self.context.append_basic_block(fn_val, "npot.ovf.trap");
            let ok_bb = self.context.append_basic_block(fn_val, "npot.ovf.ok");
            self.builder
                .build_conditional_branch(is_ovf, trap_bb, ok_bb)
                .unwrap();
            self.builder.position_at_end(trap_bb);
            self.emit_panic("integer overflow");
            self.builder.build_unreachable().unwrap();
            self.builder.position_at_end(ok_bb);
            // t = m - 1 (in [0, 2^(bits-1)-1]); shift = bits - ctlz(t) ∈ [0, bits-1].
            let t = self.builder.build_int_sub(m, one, "npot.t").unwrap();
            let ctlz = inkwell::intrinsics::Intrinsic::find("llvm.ctlz")
                .ok_or("llvm.ctlz intrinsic must exist in LLVM")?;
            let decl = ctlz
                .get_declaration(&self.module, &[int_ty.into()])
                .ok_or_else(|| format!("llvm.ctlz has no declaration for width {bits}"))?;
            let no_poison = self.context.bool_type().const_zero();
            let lz = self
                .builder
                .build_call(decl, &[t.into(), no_poison.into()], "npot.lz")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let bits_c = int_ty.const_int(u64::from(bits), false);
            let shift = self
                .builder
                .build_int_sub(bits_c, lz, "npot.shift")
                .unwrap();
            let pw = self
                .builder
                .build_left_shift(one, shift, "npot.pw")
                .unwrap();
            // The result is a non-negative power of two in iN → zero-extend.
            let res = self.coerce_int_to(pw, self.int_carrier_type_for_bits(bits), true);
            return Ok(res.into());
        }

        // `abs_diff(self, other) -> unsigned sibling` (typed in
        // expr_method_call.rs). |a - b| at the receiver's iN, which never
        // overflows: pick the larger by the receiver's signedness, subtract (the
        // iN wrapping difference is the exact unsigned magnitude since hi ≥ lo),
        // then zero-extend to the i64-backed unsigned result — matching the
        // interpreter. No intrinsic, no runtime extern.
        if method == "abs_diff" && args.len() == 1 {
            let (bits, unsigned) = self.receiver_int_kind(object, call_span, method);
            let int_ty = self.int_type_for_bits(bits);
            let a_raw = self.compile_expr(object)?.into_int_value();
            let a = self.coerce_int_to(a_raw, int_ty, unsigned);
            let b_raw = self.compile_expr(&args[0].value)?.into_int_value();
            let b = self.coerce_int_to(b_raw, int_ty, unsigned);
            let ge_pred = if unsigned {
                IntPredicate::UGE
            } else {
                IntPredicate::SGE
            };
            let a_ge_b = self
                .builder
                .build_int_compare(ge_pred, a, b, "absd.ge")
                .unwrap();
            let hi = self
                .builder
                .build_select(a_ge_b, a, b, "absd.hi")
                .unwrap()
                .into_int_value();
            let lo = self
                .builder
                .build_select(a_ge_b, b, a, "absd.lo")
                .unwrap()
                .into_int_value();
            let diff = self.builder.build_int_sub(hi, lo, "absd").unwrap();
            // The magnitude is non-negative in iN bits → always zero-extend.
            let res = self.coerce_int_to(diff, self.int_carrier_type_for_bits(bits), true);
            return Ok(res.into());
        }

        // Overflow-aware integer arithmetic — `{checked,saturating,overflowing}_{add,sub,mul}`
        // (C2, B-2026-06-19-10). Lowered at the receiver's DECLARED width via the
        // `llvm.{s,u}{op}.with.overflow.iN` intrinsic (codegen widens narrow ints
        // to i64 in value flow, so both operands are first truncated back to iN).
        // The single `(wrapped, did_overflow)` pair feeds all three families,
        // matching the interpreter's width-correct semantics bit-for-bit:
        //   checked_*     -> `None` on overflow, else `Some(wrapped)` (Option[T])
        //   saturating_*  -> `wrapped` unless overflow, then the saturation bound
        //   overflowing_* -> `(wrapped, did_overflow)` tuple `(T, bool)`
        if args.len() == 1 {
            let fam_op = ["checked_", "saturating_", "overflowing_"]
                .into_iter()
                .find_map(|p| method.strip_prefix(p).map(|op| (p, op)))
                .filter(|(_, op)| matches!(*op, "add" | "sub" | "mul"));
            if let Some((fam, op)) = fam_op {
                let (bits, unsigned) = self.receiver_int_kind(object, call_span, method);
                let int_ty = self.int_type_for_bits(bits);

                let recv_raw = self.compile_expr(object)?.into_int_value();
                let recv = self.coerce_int_to(recv_raw, int_ty, unsigned);
                let arg_raw = self.compile_expr(&args[0].value)?.into_int_value();
                let arg = self.coerce_int_to(arg_raw, int_ty, unsigned);

                let (wrapped, ovf) = self.emit_overflow_intrinsic(op, recv, arg, unsigned)?;

                match fam {
                    // overflowing_* -> `(T, bool)`: the tuple field for T is the
                    // declared width iN (matching `llvm_type_for_type_expr` of the
                    // `(T, bool)` tuple), the flag is the i1 overflow bit.
                    "overflowing_" => {
                        let bool_t = self.context.bool_type();
                        let tup_ty = self
                            .context
                            .struct_type(&[int_ty.into(), bool_t.into()], false);
                        let mut agg = tup_ty.get_undef();
                        agg = self
                            .builder
                            .build_insert_value(agg, wrapped, 0, "ovf.tup.v")
                            .unwrap()
                            .into_struct_value();
                        agg = self
                            .builder
                            .build_insert_value(agg, ovf, 1, "ovf.tup.f")
                            .unwrap()
                            .into_struct_value();
                        return Ok(agg.into());
                    }
                    // checked_* -> `Option[T]`: None on overflow, else Some(wrapped).
                    // The Some payload word is the result coerced to the i64-backed
                    // Option payload slot (zext for unsigned, sext for signed).
                    "checked_" => {
                        let fn_val = self.current_fn.unwrap();
                        let some_bb = self.context.append_basic_block(fn_val, "chk.some");
                        let none_bb = self.context.append_basic_block(fn_val, "chk.none");
                        let merge_bb = self.context.append_basic_block(fn_val, "chk.merge");
                        self.builder
                            .build_conditional_branch(ovf, none_bb, some_bb)
                            .unwrap();

                        self.builder.position_at_end(some_bb);
                        // `build_option_some_via_phis` phis i64 WORDS, so a
                        // 128-bit result has to arrive already split — one
                        // i128 operand made every phi fail module verification
                        // ("PHI node operands are not the same type as the
                        // result", B-2026-08-19-19). `coerce_to_payload_words`
                        // is the same splitter the `Some(x)` construction path
                        // uses, so both producers of an `Option[i128]` emit the
                        // identical little-endian (lo, hi) word pair that the
                        // match-arm unpack rejoins.
                        let payload_words = if bits > 64 {
                            let wide = self.coerce_int_to(
                                wrapped,
                                self.int_carrier_type_for_bits(bits),
                                unsigned,
                            );
                            self.coerce_to_payload_words(wide.into(), bits.div_ceil(64) as usize)?
                        } else {
                            vec![self.coerce_int_to(
                                wrapped,
                                self.int_carrier_type_for_bits(bits),
                                unsigned,
                            )]
                        };
                        self.builder.build_unconditional_branch(merge_bb).unwrap();

                        self.builder.position_at_end(none_bb);
                        self.builder.build_unconditional_branch(merge_bb).unwrap();

                        self.builder.position_at_end(merge_bb);
                        let agg = self.build_option_some_via_phis(
                            &payload_words,
                            some_bb,
                            none_bb,
                            "chk.opt",
                        );
                        return Ok(agg);
                    }
                    // saturating_* -> `T`: `wrapped` unless overflow, then clamp to
                    // the saturation bound. Unsigned: sub underflows to 0, add/mul
                    // overflow to UMAX. Signed: the bound is SMAX/SMIN by the sign
                    // of the true result — for add/sub `a >= 0 ? SMAX : SMIN` (on
                    // overflow the operands force that sign), for mul
                    // `sign(a)==sign(b) ? SMAX : SMIN`. Matches Rust / the interp's
                    // i128 clamp without needing a wider type (no `llvm.*mul.sat`).
                    _ => {
                        let zero = int_ty.const_zero();
                        let bound = if unsigned {
                            if op == "sub" {
                                int_ty.const_zero()
                            } else {
                                int_ty.const_all_ones()
                            }
                        } else {
                            // Build the signed bounds by BIT PATTERN, not by
                            // computing them in `u128` and casting: `as u64`
                            // truncates, so at `bits == 128` SMAX became
                            // `u64::MAX` and SMIN became `0` — a saturating
                            // i128 add clamped to 18446744073709551615
                            // (B-2026-08-19-19). `all_ones >> 1` (logical) is
                            // SMAX at every width, and `SMAX + 1` wraps to SMIN;
                            // both fold to constants.
                            let one_c = int_ty.const_int(1, false);
                            let smax = self
                                .builder
                                .build_right_shift(
                                    int_ty.const_all_ones(),
                                    one_c,
                                    false,
                                    "sat.smax",
                                )
                                .unwrap();
                            let smin = self.builder.build_int_add(smax, one_c, "sat.smin").unwrap();
                            let pick_max = if op == "mul" {
                                let sa = self
                                    .builder
                                    .build_int_compare(IntPredicate::SLT, recv, zero, "sat.sa")
                                    .unwrap();
                                let sb = self
                                    .builder
                                    .build_int_compare(IntPredicate::SLT, arg, zero, "sat.sb")
                                    .unwrap();
                                self.builder
                                    .build_int_compare(IntPredicate::EQ, sa, sb, "sat.same")
                                    .unwrap()
                            } else {
                                self.builder
                                    .build_int_compare(IntPredicate::SGE, recv, zero, "sat.age")
                                    .unwrap()
                            };
                            self.builder
                                .build_select(pick_max, smax, smin, "sat.bound")
                                .unwrap()
                                .into_int_value()
                        };
                        let sat = self
                            .builder
                            .build_select(ovf, bound, wrapped, "sat.res")
                            .unwrap()
                            .into_int_value();
                        let res =
                            self.coerce_int_to(sat, self.int_carrier_type_for_bits(bits), unsigned);
                        return Ok(res.into());
                    }
                }
            }
        }

        // ASCII byte-classification predicates on integer scalars (the `u8`
        // bytes from `String.bytes()`): `is_ascii_digit` / `is_ascii_alphabetic`
        // / `is_ascii_hexdigit` → bool (i1). Phase-8 floor for the self-hosting
        // lexer's byte-indexed scan (phase-12-self-hosting.md). Lowered to inline
        // unsigned range checks — no runtime extern. Unsigned predicates so a
        // byte ≥ 0x80 never spuriously matches a signed range.
        if args.is_empty()
            && matches!(
                method,
                "is_ascii_digit" | "is_ascii_alphabetic" | "is_ascii_hexdigit"
            )
        {
            let v = self.compile_expr(object)?;
            if let BasicValueEnum::IntValue(iv) = v {
                let ty = iv.get_type();
                // in_range(lo, hi) = (iv >= lo) & (iv <= hi), unsigned.
                let in_range = |s: &Self, lo: u64, hi: u64, tag: &str| {
                    let ge = s
                        .builder
                        .build_int_compare(
                            IntPredicate::UGE,
                            iv,
                            ty.const_int(lo, false),
                            &format!("{tag}.ge"),
                        )
                        .unwrap();
                    let le = s
                        .builder
                        .build_int_compare(
                            IntPredicate::ULE,
                            iv,
                            ty.const_int(hi, false),
                            &format!("{tag}.le"),
                        )
                        .unwrap();
                    s.builder.build_and(ge, le, &format!("{tag}.in")).unwrap()
                };
                let digit = in_range(self, b'0' as u64, b'9' as u64, "ascii.d");
                let r = match method {
                    "is_ascii_digit" => digit,
                    "is_ascii_alphabetic" => {
                        let lower = in_range(self, b'a' as u64, b'z' as u64, "ascii.l");
                        let upper = in_range(self, b'A' as u64, b'Z' as u64, "ascii.u");
                        self.builder.build_or(lower, upper, "ascii.alpha").unwrap()
                    }
                    "is_ascii_hexdigit" => {
                        let lower = in_range(self, b'a' as u64, b'f' as u64, "ascii.hl");
                        let upper = in_range(self, b'A' as u64, b'F' as u64, "ascii.hu");
                        let af = self.builder.build_or(lower, upper, "ascii.hex.af").unwrap();
                        self.builder.build_or(digit, af, "ascii.hex").unwrap()
                    }
                    _ => unreachable!(),
                };
                return Ok(r.into());
            }
        }

        // `char.to_digit(radix) -> Option[u32]` (typed in expr_method_call.rs),
        // mirroring Rust's `char::to_digit` and the interpreter (method_call.rs):
        // an out-of-range radix (< 2 or > 36) traps (`panics`); otherwise the
        // codepoint's digit value in that radix wraps as `Some(v)` / `None` via
        // the shared `build_checked_to_int_option` Option constructor. Gated on
        // a `char` receiver so a user `to_digit` on another type is unaffected.
        // `char.is_digit(radix) -> bool` (B-2026-08-12-25) shares every line of
        // this arm except the final wrap: it is the `in_range` predicate the
        // Option constructor would have consumed, returned as a bare i1. The
        // `td.` value names below are shared with it.
        if matches!(method, "to_digit" | "is_digit") && args.len() == 1 && self.expr_is_char(object)
        {
            let i32_t = self.context.i32_type();
            // Codepoint as i32 (char lowers to i32; narrow receivers z-extend).
            let cp_raw = self.compile_expr(object)?.into_int_value();
            let cp = match cp_raw.get_type().get_bit_width() {
                32 => cp_raw,
                w if w < 32 => self
                    .builder
                    .build_int_z_extend(cp_raw, i32_t, "td.cp.z")
                    .unwrap(),
                _ => self
                    .builder
                    .build_int_truncate(cp_raw, i32_t, "td.cp.t")
                    .unwrap(),
            };
            // Radix as i32 (u32 source — compare unsigned).
            let radix_raw = self.compile_expr(&args[0].value)?.into_int_value();
            let radix = match radix_raw.get_type().get_bit_width() {
                32 => radix_raw,
                w if w < 32 => self
                    .builder
                    .build_int_z_extend(radix_raw, i32_t, "td.rx.z")
                    .unwrap(),
                _ => self
                    .builder
                    .build_int_truncate(radix_raw, i32_t, "td.rx.t")
                    .unwrap(),
            };

            // Trap on radix ∉ 2..=36, matching Rust's panic / the interpreter's
            // runtime error. `ULT 2` also catches 0/1; `UGT 36` the high end.
            let fn_val = self.current_fn.unwrap();
            let lo_bad = self
                .builder
                .build_int_compare(
                    IntPredicate::ULT,
                    radix,
                    i32_t.const_int(2, false),
                    "td.rx.lo",
                )
                .unwrap();
            let hi_bad = self
                .builder
                .build_int_compare(
                    IntPredicate::UGT,
                    radix,
                    i32_t.const_int(36, false),
                    "td.rx.hi",
                )
                .unwrap();
            let bad = self.builder.build_or(lo_bad, hi_bad, "td.rx.bad").unwrap();
            let trap_bb = self.context.append_basic_block(fn_val, "td.rx.trap");
            let ok_bb = self.context.append_basic_block(fn_val, "td.rx.ok");
            self.builder
                .build_conditional_branch(bad, trap_bb, ok_bb)
                .unwrap();
            self.builder.position_at_end(trap_bb);
            self.emit_panic(&format!("{method}: radix must be in 2..=36"));
            self.builder.build_unreachable().unwrap();
            self.builder.position_at_end(ok_bb);

            // Digit value by ASCII class (matching char::to_digit): '0'..='9' →
            // c-'0'; 'a'..='z' → c-'a'+10; 'A'..='Z' → c-'A'+10; else no digit.
            let in_class = |c: char| i32_t.const_int(c as u64, false);
            // Decimal '0'..='9'.
            let is_dec_lo = self
                .builder
                .build_int_compare(IntPredicate::UGE, cp, in_class('0'), "td.dec.ge")
                .unwrap();
            let is_dec_hi = self
                .builder
                .build_int_compare(IntPredicate::ULE, cp, in_class('9'), "td.dec.le")
                .unwrap();
            let is_dec = self
                .builder
                .build_and(is_dec_lo, is_dec_hi, "td.dec")
                .unwrap();
            let dec_val = self
                .builder
                .build_int_sub(cp, in_class('0'), "td.dec.v")
                .unwrap();
            // Lowercase 'a'..='z' → 10 + (c - 'a').
            let is_low_lo = self
                .builder
                .build_int_compare(IntPredicate::UGE, cp, in_class('a'), "td.low.ge")
                .unwrap();
            let is_low_hi = self
                .builder
                .build_int_compare(IntPredicate::ULE, cp, in_class('z'), "td.low.le")
                .unwrap();
            let is_low = self
                .builder
                .build_and(is_low_lo, is_low_hi, "td.low")
                .unwrap();
            let low_off = self
                .builder
                .build_int_sub(cp, in_class('a'), "td.low.off")
                .unwrap();
            let low_val = self
                .builder
                .build_int_add(low_off, i32_t.const_int(10, false), "td.low.v")
                .unwrap();
            // Uppercase 'A'..='Z' → 10 + (c - 'A').
            let is_up_lo = self
                .builder
                .build_int_compare(IntPredicate::UGE, cp, in_class('A'), "td.up.ge")
                .unwrap();
            let is_up_hi = self
                .builder
                .build_int_compare(IntPredicate::ULE, cp, in_class('Z'), "td.up.le")
                .unwrap();
            let is_up = self.builder.build_and(is_up_lo, is_up_hi, "td.up").unwrap();
            let up_off = self
                .builder
                .build_int_sub(cp, in_class('A'), "td.up.off")
                .unwrap();
            let up_val = self
                .builder
                .build_int_add(up_off, i32_t.const_int(10, false), "td.up.v")
                .unwrap();

            // Select the class value; default 0 when no class matches.
            let has_digit = self
                .builder
                .build_or(
                    is_dec,
                    self.builder.build_or(is_low, is_up, "td.low_up").unwrap(),
                    "td.any",
                )
                .unwrap();
            let v_up_or_zero = self
                .builder
                .build_select(is_up, up_val, i32_t.const_zero(), "td.v.up")
                .unwrap()
                .into_int_value();
            let v_low = self
                .builder
                .build_select(is_low, low_val, v_up_or_zero, "td.v.low")
                .unwrap()
                .into_int_value();
            let val = self
                .builder
                .build_select(is_dec, dec_val, v_low, "td.v")
                .unwrap()
                .into_int_value();

            // Valid iff a digit class matched AND value < radix (unsigned).
            let lt_radix = self
                .builder
                .build_int_compare(IntPredicate::ULT, val, radix, "td.lt")
                .unwrap();
            let in_range = self
                .builder
                .build_and(has_digit, lt_radix, "td.valid")
                .unwrap();
            if method == "is_digit" {
                return Ok(in_range.into());
            }
            return self.build_checked_to_int_option(in_range, val);
        }

        // Unicode `char` classification predicates (phase-12 #13): `is_alphabetic`
        // / `is_numeric` / `is_alphanumeric` / `is_whitespace` → bool (i1). The
        // typechecker admits these only on a `char` receiver (lowered to i32), so
        // a method-name match suffices. Unlike the inlined ASCII byte predicates
        // above, Unicode classification needs the runtime's Unicode tables, so
        // route through the `karac_runtime_char_is_*` externs (declared in
        // `Codegen::new`). The extern returns i8 (0/1) → compare `!= 0` for i1.
        if args.is_empty()
            && matches!(
                method,
                "is_alphabetic"
                    | "is_numeric"
                    | "is_alphanumeric"
                    | "is_whitespace"
                    | "is_uppercase"
                    | "is_lowercase"
            )
        {
            let v = self.compile_expr(object)?;
            if let BasicValueEnum::IntValue(iv) = v {
                let i32_t = self.context.i32_type();
                let cp = match iv.get_type().get_bit_width() {
                    32 => iv,
                    w if w < 32 => self
                        .builder
                        .build_int_z_extend(iv, i32_t, "char.cp.z")
                        .unwrap(),
                    _ => self
                        .builder
                        .build_int_truncate(iv, i32_t, "char.cp.t")
                        .unwrap(),
                };
                let fname = match method {
                    "is_alphabetic" => "karac_runtime_char_is_alphabetic",
                    "is_numeric" => "karac_runtime_char_is_numeric",
                    "is_alphanumeric" => "karac_runtime_char_is_alphanumeric",
                    "is_whitespace" => "karac_runtime_char_is_whitespace",
                    "is_uppercase" => "karac_runtime_char_is_uppercase",
                    "is_lowercase" => "karac_runtime_char_is_lowercase",
                    _ => unreachable!(),
                };
                let f = self
                    .module
                    .get_function(fname)
                    .expect("char predicate extern declared in Codegen::new");
                let ret = self
                    .builder
                    .build_call(f, &[cp.into()], "char.pred")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let b = self
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        ret,
                        self.context.i8_type().const_zero(),
                        "char.pred.b",
                    )
                    .unwrap();
                return Ok(b.into());
            }
        }

        // Unicode case folding on a `char` (B-2026-08-12-25): `to_lowercase` /
        // `to_uppercase` → char, through the `karac_runtime_char_to_*case`
        // externs (i32 codepoint in, i32 codepoint out). The runtime owns the
        // "full mapping when it is 1:1, else self" collapse so it is computed in
        // exactly one place for both backends — see the extern's doc comment.
        //
        // GATED ON A CHAR RECEIVER, unlike the predicate arm above, because
        // these two names are NOT char-only: `String.to_lowercase()` is the
        // allocating String→String transform, and it must keep reaching
        // `compile_vec_method`'s `karac_string_to_lowercase`.
        if args.is_empty()
            && matches!(method, "to_lowercase" | "to_uppercase")
            && self.expr_is_char(object)
        {
            let i32_t = self.context.i32_type();
            let cp_raw = self.compile_expr(object)?.into_int_value();
            let cp = match cp_raw.get_type().get_bit_width() {
                32 => cp_raw,
                w if w < 32 => self
                    .builder
                    .build_int_z_extend(cp_raw, i32_t, "char.cp.z")
                    .unwrap(),
                _ => self
                    .builder
                    .build_int_truncate(cp_raw, i32_t, "char.cp.t")
                    .unwrap(),
            };
            let fname = if method == "to_lowercase" {
                "karac_runtime_char_to_lowercase"
            } else {
                "karac_runtime_char_to_uppercase"
            };
            let f = self
                .module
                .get_function(fname)
                .expect("char case-fold extern declared in Codegen::new");
            let folded = self
                .builder
                .build_call(f, &[cp.into()], "char.fold")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic();
            return Ok(folded);
        }

        // ASCII `char` methods (typed in expr_method_call.rs, char receiver
        // lowered to i32): `is_ascii` → `cp <= 0x7F`; `to_ascii_uppercase` /
        // `to_ascii_lowercase` → char, mapping only the ASCII letter ranges.
        // Inlined codepoint arithmetic — pure ASCII, no Unicode tables and no
        // runtime extern (unlike the Unicode `is_*` predicates above).
        if args.is_empty()
            && matches!(
                method,
                "is_ascii" | "to_ascii_uppercase" | "to_ascii_lowercase"
            )
        {
            let v = self.compile_expr(object)?;
            if let BasicValueEnum::IntValue(iv) = v {
                let i32_t = self.context.i32_type();
                let cp = match iv.get_type().get_bit_width() {
                    32 => iv,
                    w if w < 32 => self
                        .builder
                        .build_int_z_extend(iv, i32_t, "char.cp.z")
                        .unwrap(),
                    _ => self
                        .builder
                        .build_int_truncate(iv, i32_t, "char.cp.t")
                        .unwrap(),
                };
                if method == "is_ascii" {
                    let r = self
                        .builder
                        .build_int_compare(
                            IntPredicate::ULE,
                            cp,
                            i32_t.const_int(0x7F, false),
                            "char.is_ascii",
                        )
                        .unwrap();
                    return Ok(r.into());
                }
                // Case fold: in_range(lo, hi) & then add `delta` (±32).
                let (lo, hi, delta): (u64, u64, i64) = if method == "to_ascii_uppercase" {
                    (b'a' as u64, b'z' as u64, -32)
                } else {
                    (b'A' as u64, b'Z' as u64, 32)
                };
                let ge = self
                    .builder
                    .build_int_compare(IntPredicate::UGE, cp, i32_t.const_int(lo, false), "case.ge")
                    .unwrap();
                let le = self
                    .builder
                    .build_int_compare(IntPredicate::ULE, cp, i32_t.const_int(hi, false), "case.le")
                    .unwrap();
                let in_range = self.builder.build_and(ge, le, "case.in").unwrap();
                let delta_c = i32_t.const_int(delta as u64, true);
                let shifted = self
                    .builder
                    .build_int_add(cp, delta_c, "case.shift")
                    .unwrap();
                let r = self
                    .builder
                    .build_select(in_range, shifted, cp, "case.fold")
                    .unwrap();
                return Ok(r);
            }
        }

        if method == "cmp" && args.len() == 1 {
            let lhs = self.compile_expr(object)?;
            let rhs = self.compile_expr(&args[0].value)?;
            if let (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) = (lhs, rhs) {
                let i64_t = self.context.i64_type();
                let lt = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, l, r, "cmp.lt")
                    .unwrap();
                let gt = self
                    .builder
                    .build_int_compare(IntPredicate::SGT, l, r, "cmp.gt")
                    .unwrap();
                let zero = i64_t.const_zero();
                let one = i64_t.const_int(1, false);
                let two = i64_t.const_int(2, false);
                let tag_gt = self
                    .builder
                    .build_select(gt, two, one, "cmp.tag.gt")
                    .unwrap()
                    .into_int_value();
                let tag = self
                    .builder
                    .build_select(lt, zero, tag_gt, "cmp.tag")
                    .unwrap()
                    .into_int_value();
                let ord_struct_ty = self
                    .type_decls
                    .enum_layouts
                    .get("Ordering")
                    .map(|l| l.llvm_type)
                    .unwrap_or_else(|| self.context.struct_type(&[i64_t.into()], false));
                let agg = ord_struct_ty.get_undef();
                let agg = self.builder.build_insert_value(agg, tag, 0, "ord").unwrap();
                return Ok(agg.into_struct_value().into());
            }
            // String.cmp(other) -> Ordering — byte-lexicographic, the method
            // form of the `<`/`>` operators. `karac_string_cmp` returns -1/0/+1
            // (the same order Vec[String].sort / binary_search use), and the
            // Ordering tags are Less=0 / Equal=1 / Greater=2, so tag = cmp + 1
            // maps them directly. Guard on the operand LAYOUT (the String
            // {ptr,len,cap} header) rather than `inferred_receiver_type`, which
            // only resolves NAMED receivers — a string LITERAL (`"a".cmp(b)`) or
            // an INDEX (`v[0].cmp(v[1])`) receiver typechecks + runs but has no
            // var-name to look up, so the earlier name-only guard left them
            // falling through to the "not yet supported" catch-all (a run/build
            // divergence). The typechecker admits `.cmp` only on int/char/bool/
            // String, and int/char/bool are `IntValue` (handled above), so any
            // `{ptr,len,cap}`-shaped struct pair reaching here IS a String;
            // user-struct `.cmp` is rejected at typecheck and never arrives.
            if let (BasicValueEnum::StructValue(l), BasicValueEnum::StructValue(r)) = (lhs, rhs) {
                if l.get_type() == self.vec_struct_type() {
                    let i64_t = self.context.i64_type();
                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                    let l_ptr = self
                        .builder
                        .build_extract_value(l, 0, "cmp.l.ptr")
                        .unwrap()
                        .into_pointer_value();
                    let l_len = self
                        .builder
                        .build_extract_value(l, 1, "cmp.l.len")
                        .unwrap()
                        .into_int_value();
                    let r_ptr = self
                        .builder
                        .build_extract_value(r, 0, "cmp.r.ptr")
                        .unwrap()
                        .into_pointer_value();
                    let r_len = self
                        .builder
                        .build_extract_value(r, 1, "cmp.r.len")
                        .unwrap()
                        .into_int_value();
                    let cmp_fn =
                        self.module
                            .get_function("karac_string_cmp")
                            .unwrap_or_else(|| {
                                let fn_ty = i64_t.fn_type(
                                    &[ptr_ty.into(), i64_t.into(), ptr_ty.into(), i64_t.into()],
                                    false,
                                );
                                self.module.add_function(
                                    "karac_string_cmp",
                                    fn_ty,
                                    Some(inkwell::module::Linkage::External),
                                )
                            });
                    let raw = self
                        .builder
                        .build_call(
                            cmp_fn,
                            &[l_ptr.into(), l_len.into(), r_ptr.into(), r_len.into()],
                            "cmp.scmp",
                        )
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_int_value();
                    let tag = self
                        .builder
                        .build_int_add(raw, i64_t.const_int(1, false), "cmp.tag")
                        .unwrap();
                    let ord_struct_ty = self
                        .type_decls
                        .enum_layouts
                        .get("Ordering")
                        .map(|l| l.llvm_type)
                        .unwrap_or_else(|| self.context.struct_type(&[i64_t.into()], false));
                    let agg = ord_struct_ty.get_undef();
                    let agg = self.builder.build_insert_value(agg, tag, 0, "ord").unwrap();
                    return Ok(agg.into_struct_value().into());
                }
                // A non-String struct pair reaching here is a user struct/enum
                // whose `#[derive(Ord)]` the typechecker admitted for `.cmp`
                // (`expr_method_call.rs`). Route through the same lexicographic
                // comparator the `<`/`>` operators use, converting its i64 sign
                // to an `Ordering` tag. roadmap Phase 8 § Eq/Ord.
                if let Some(type_name) = self.inferred_receiver_type(object) {
                    if let Some(v) = self.compile_user_cmp_to_ordering(&type_name, lhs, rhs)? {
                        return Ok(v);
                    }
                }
            }
        }

        // `.as_slice()` / `.as_slice_mut()` on Array, Vec, or Slice —
        // synthesize a `{ptr, i64}` slice header. The element type for the
        // resulting slice is inferred from the source variable, not from a
        // user-supplied argument. See design.md § Slices.
        // B-2026-08-10-4 — `split_at_mut` (design.md "`split_at_mut` —
        // disjoint mutable partition"). Sits beside `as_slice_mut` because it
        // needs exactly the same receiver triage (inline `Array` slot,
        // pass-through `Slice`, heap `Vec`) and then does one more step: split
        // the resulting `{ptr, len}` at `mid`.
        //
        // B-2026-08-14-9 answers the question the note here left open, and
        // `split_at` now shares this arm. The two DO differ in the aliasing
        // contract, and the answer is that the read-only one may alias freely:
        // its halves are immutable views, so nothing can observe the sharing,
        // and it is what the interpreter has always done (both halves are
        // `Value::Slice` windows over the receiver's own storage). Copying
        // instead would be a silent semantic change — a `Slice[String]` half
        // would have to deep-clone or alias-without-owning — as well as slower.
        // The mut form's disjointness argument is unaffected: it rests on
        // `[0, mid)` and `[mid, len)` not overlapping, which is a property of
        // the split, not of who else holds a view.
        //
        // Both halves are `mut Slice[T]` views over the receiver's own buffer
        // — no copy — which is the whole point: a write through either half
        // must land in the caller's collection. Disjointness is structural
        // (`[0, mid)` and `[mid, len)`), which is the spec's argument for why
        // both may be simultaneously live with no annotation.
        if matches!(method, "split_at_mut" | "split_at") && args.len() == 1 {
            if let ExprKind::Identifier(name) = &object.kind {
                if let Some(slot) = self.variables.get(name.as_str()).copied() {
                    let i64_t = self.context.i64_type();
                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                    let slice_ty = self.slice_struct_type();
                    // (base data pointer, length, element type) per receiver
                    // shape — the same three cases `as_slice` triages.
                    let parts: Option<(
                        inkwell::values::PointerValue<'ctx>,
                        inkwell::values::IntValue<'ctx>,
                        BasicTypeEnum<'ctx>,
                    )> = if let BasicTypeEnum::ArrayType(at) = slot.ty {
                        Some((
                            slot.ptr,
                            i64_t.const_int(at.len() as u64, false),
                            at.get_element_type(),
                        ))
                    } else if let Some(elem) =
                        self.var_types.slice_elem_types.get(name.as_str()).copied()
                    {
                        let hdr = self
                            .builder
                            .build_load(slice_ty, slot.ptr, "sam.s.hdr")
                            .unwrap()
                            .into_struct_value();
                        let data = self
                            .builder
                            .build_extract_value(hdr, 0, "sam.s.data")
                            .unwrap()
                            .into_pointer_value();
                        let len = self
                            .builder
                            .build_extract_value(hdr, 1, "sam.s.len")
                            .unwrap()
                            .into_int_value();
                        Some((data, len, elem))
                    } else if let Some(elem) =
                        self.var_types.vec_elem_types.get(name.as_str()).copied()
                    {
                        let vec_ty = self.vec_struct_type();
                        let data_pp = self
                            .builder
                            .build_struct_gep(vec_ty, slot.ptr, 0, "sam.v.data.pp")
                            .unwrap();
                        let data = self
                            .builder
                            .build_load(ptr_ty, data_pp, "sam.v.data")
                            .unwrap()
                            .into_pointer_value();
                        let len_p = self
                            .builder
                            .build_struct_gep(vec_ty, slot.ptr, 1, "sam.v.len.p")
                            .unwrap();
                        let len = self
                            .builder
                            .build_load(i64_t, len_p, "sam.v.len")
                            .unwrap()
                            .into_int_value();
                        Some((data, len, elem))
                    } else {
                        None
                    };
                    if let Some((data, len, elem_ty)) = parts {
                        let mid = self.compile_expr(&args[0].value)?;
                        let mid = self.coerce_to_i64(mid)?;
                        // Spec: "Panics if `mid > self.len()`". The negative
                        // case is folded into the same guard — an unsigned
                        // compare would read a negative `mid` as enormous and
                        // trap anyway, but testing it explicitly keeps the
                        // diagnostic honest about which bound was violated.
                        let fn_val = self.current_fn.unwrap();
                        let bad_hi = self
                            .builder
                            .build_int_compare(inkwell::IntPredicate::SGT, mid, len, "sam.hi")
                            .unwrap();
                        let bad_lo = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::SLT,
                                mid,
                                i64_t.const_zero(),
                                "sam.lo",
                            )
                            .unwrap();
                        let bad = self.builder.build_or(bad_hi, bad_lo, "sam.bad").unwrap();
                        let panic_bb = self.context.append_basic_block(fn_val, "sam.panic");
                        let ok_bb = self.context.append_basic_block(fn_val, "sam.ok");
                        self.builder
                            .build_conditional_branch(bad, panic_bb, ok_bb)
                            .unwrap();
                        self.builder.position_at_end(panic_bb);
                        self.emit_panic("split_at_mut index out of bounds");
                        self.builder.build_unreachable().unwrap();
                        self.builder.position_at_end(ok_bb);
                        // Second half starts `mid` ELEMENTS in — GEP on the
                        // element type so the stride is the element's, not a
                        // byte's. `inbounds` is justified by the guard above.
                        let rhs_ptr = unsafe {
                            self.builder
                                .build_in_bounds_gep(elem_ty, data, &[mid], "sam.rhs.ptr")
                                .unwrap()
                        };
                        let rhs_len = self.builder.build_int_sub(len, mid, "sam.rhs.len").unwrap();
                        let lhs = self.build_slice_header(slice_ty, data, mid);
                        let rhs = self.build_slice_header(slice_ty, rhs_ptr, rhs_len);
                        let tup_ty = self
                            .context
                            .struct_type(&[slice_ty.into(), slice_ty.into()], false);
                        let mut agg = tup_ty.get_undef();
                        agg = self
                            .builder
                            .build_insert_value(agg, lhs, 0, "sam.tup.0")
                            .unwrap()
                            .into_struct_value();
                        agg = self
                            .builder
                            .build_insert_value(agg, rhs, 1, "sam.tup.1")
                            .unwrap()
                            .into_struct_value();
                        return Ok(agg.into());
                    }
                }
            }
        }

        if (method == "as_slice" || method == "as_slice_mut") && args.is_empty() {
            if let ExprKind::Identifier(name) = &object.kind {
                if let Some(slot) = self.variables.get(name.as_str()).copied() {
                    let i64_t = self.context.i64_type();
                    let slice_ty = self.slice_struct_type();
                    // B-2026-08-21-4 — the receiver's PLACE, not its slot.
                    //
                    // For an owned binding the alloca IS the aggregate, so
                    // `slot.ptr` and the place coincide and this arm was right
                    // by accident. For a `ref` / `mut ref` PARAMETER the alloca
                    // holds a POINTER to the caller's aggregate, and reading
                    // the header straight out of `slot.ptr` interpreted that
                    // pointer as the Vec's `data` field and whatever sat beside
                    // it on the stack as `len` — a silent wrong answer
                    // (`v.as_slice().len()` returned a pointer-sized number, or
                    // 0). `get_data_ptr` is the established resolver for
                    // exactly this and also covers the RC-fallback box and a
                    // module-binding global; every other Vec method already
                    // goes through it, which is why only `as_slice` was wrong.
                    let place = self.get_data_ptr(name).unwrap_or(slot.ptr);
                    if let BasicTypeEnum::ArrayType(at) = slot.ty {
                        let len = i64_t.const_int(at.len() as u64, false);
                        return Ok(self.build_slice_header(slice_ty, place, len));
                    }
                    if self.var_types.slice_elem_types.contains_key(name.as_str()) {
                        return Ok(self
                            .builder
                            .build_load(slice_ty, place, "as_slice.passthrough")
                            .unwrap());
                    }
                    if self.var_types.vec_elem_types.contains_key(name.as_str()) {
                        let ptr_ty = self.context.ptr_type(AddressSpace::default());
                        let vec_ty = self.vec_struct_type();
                        let data_pp = self
                            .builder
                            .build_struct_gep(vec_ty, place, 0, "as_slice.v.data.pp")
                            .unwrap();
                        let data = self
                            .builder
                            .build_load(ptr_ty, data_pp, "as_slice.v.data")
                            .unwrap()
                            .into_pointer_value();
                        let len_p = self
                            .builder
                            .build_struct_gep(vec_ty, place, 1, "as_slice.v.len.p")
                            .unwrap();
                        let len = self
                            .builder
                            .build_load(i64_t, len_p, "as_slice.v.len")
                            .unwrap()
                            .into_int_value();
                        return Ok(self.build_slice_header(slice_ty, data, len));
                    }
                }
            }
        }

        // Module-binding receivers dispatch through the same Vec / Map / Set
        // codegen paths as local Vec / Map / Set variables — the slice-10
        // `reseed_module_binding_side_tables` registers `vec_elem_types` /
        // `map_key_types` / `set_elem_types` for each module binding, and
        // `get_data_ptr` falls back to the binding's global pointer when
        // the name isn't a local. The typechecker's
        // `path_call_method_dispatch` rewrite + the lowering pass already
        // converted the `Call(Path([X, method]))` shape to `MethodCall(X,
        // method)` for value-binding receivers, so the receiver-shape
        // routing here is uniform with the local-variable case.
        if let ExprKind::Identifier(name) = &object.kind {
            if !self.variables.contains_key(name.as_str())
                && self
                    .mod_bindings
                    .module_bindings
                    .contains_key(name.as_str())
            {
                if self.var_types.vec_elem_types.contains_key(name.as_str()) {
                    let data_ptr = self.get_data_ptr(name).unwrap();
                    return self.compile_vec_method(name, data_ptr, method, args);
                }
                if self.mapset.map_key_types.contains_key(name.as_str()) {
                    let name = name.clone();
                    return self.compile_map_method(&name, method, args);
                }
                if self.mapset.set_elem_types.contains_key(name.as_str()) {
                    let name = name.clone();
                    return self.compile_set_method(&name, method, args);
                }
            }
        }

        if let ExprKind::Identifier(name) = &object.kind {
            if let Some(slot) = self.variables.get(name.as_str()).copied() {
                // Array methods (owned — slot.ty is ArrayType)
                if let BasicTypeEnum::ArrayType(at) = slot.ty {
                    if method == "len" {
                        return Ok(self
                            .context
                            .i64_type()
                            .const_int(at.len() as u64, false)
                            .into());
                    }
                    // `as_ptr()` / `as_mut_ptr()` — the element-0 address of
                    // the owned array's storage, handed out as the raw
                    // pointer `*const T` / `*mut T` (raw pointers lower to a
                    // genuine LLVM `ptr`; the typechecker types these in
                    // `infer_method_call`'s Array arm). Mirrors `CStr.as_ptr`,
                    // except the producer is a GEP to element 0 rather than a
                    // struct field — `slot.ptr` points at the `[N x T]`
                    // alloca, and `[0, 0]` is its first element.
                    if method == "as_ptr" || method == "as_mut_ptr" {
                        let zero = self.context.i32_type().const_zero();
                        let elem0 = unsafe {
                            self.builder
                                .build_in_bounds_gep(at, slot.ptr, &[zero, zero], "arr.as_ptr")
                                .map_err(|e| format!("Array.{method} gep: {e}"))?
                        };
                        return Ok(elem0.into());
                    }
                    // Read-only surface `get`/`first`/`last`/`contains`/
                    // `is_empty` over a SCALAR-element fixed array (the
                    // interpreter runs these via array-as-Vec; codegen matches
                    // here — B-2026-07-17-19). Non-scalar elements are gated out
                    // by the typechecker (they stay rejected at check).
                    let elem_ty = at.get_element_type();
                    if matches!(
                        elem_ty,
                        BasicTypeEnum::IntType(_) | BasicTypeEnum::FloatType(_)
                    ) && matches!(
                        method,
                        "get" | "first" | "last" | "contains" | "is_empty" | "is_sorted"
                    ) {
                        let zero = self.context.i32_type().const_zero();
                        let elem0 = unsafe {
                            self.builder
                                .build_in_bounds_gep(at, slot.ptr, &[zero, zero], "arr.elem0")
                                .map_err(|e| format!("Array.{method} gep: {e}"))?
                        };
                        let elem_te = self
                            .var_types
                            .array_elem_type_exprs
                            .get(name.as_str())
                            .cloned();
                        return self.compile_fixed_array_read(
                            elem0,
                            elem_ty,
                            at.len() as u64,
                            method,
                            args,
                            elem_te,
                        );
                    }
                }
                // Ref Array methods — ref_params has the inner type
                if let Some(&BasicTypeEnum::ArrayType(at)) =
                    self.borrow_vars.ref_params.get(name.as_str())
                {
                    if method == "len" {
                        return Ok(self
                            .context
                            .i64_type()
                            .const_int(at.len() as u64, false)
                            .into());
                    }
                    // `as_ptr()` / `as_mut_ptr()` on a `ref Array` — the ref
                    // param already carries the data pointer (element-0), so
                    // hand it out directly. Same `*const T` / `*mut T` result
                    // as the owned arm above.
                    if method == "as_ptr" || method == "as_mut_ptr" {
                        let data = self.get_data_ptr(name).ok_or_else(|| {
                            format!("Array.{method}: no data pointer for ref array '{name}'")
                        })?;
                        return Ok(data.into());
                    }
                    // Read-only surface over a `ref Array` — the ref param's data
                    // pointer is already element-0 (a `T*`), so pass it straight
                    // through (B-2026-07-17-19).
                    let elem_ty = at.get_element_type();
                    if matches!(
                        elem_ty,
                        BasicTypeEnum::IntType(_) | BasicTypeEnum::FloatType(_)
                    ) && matches!(
                        method,
                        "get" | "first" | "last" | "contains" | "is_empty" | "is_sorted"
                    ) {
                        let data = self.get_data_ptr(name).ok_or_else(|| {
                            format!("Array.{method}: no data pointer for ref array '{name}'")
                        })?;
                        let elem_te = self
                            .var_types
                            .array_elem_type_exprs
                            .get(name.as_str())
                            .cloned();
                        return self.compile_fixed_array_read(
                            data,
                            elem_ty,
                            at.len() as u64,
                            method,
                            args,
                            elem_te,
                        );
                    }
                }
                // SoA layout methods
                if let Some(soa) = self.active_soa_layout(name.as_str()) {
                    return self.compile_soa_method(name, &soa, slot, method, args);
                }
                // Tensor instance methods — shape()/rank() read the
                // `[rank][dims][data]` header (`src/codegen/tensor.rs`).
                // The reshape/permute/slice/squeeze family is handled by
                // `try_compile_tensor_transform` at the top of this fn
                // (covers identifier + chained receivers); only `iter_axis`
                // remains a follow-on codegen slice and errors loudly here
                // rather than falling through to the silent-0 default.
                if let Some(info) = self.accel.tensor_var_infos.get(name.as_str()) {
                    match method {
                        "shape" | "rank" => {
                            let t_ptr = self.tensor_ptr_for_var(name)?;
                            return self.compile_tensor_shape_method(t_ptr, method);
                        }
                        "iter_axis" => {
                            let (elem, rank) = (info.elem, info.dims.len());
                            let t_ptr = self.tensor_ptr_for_var(name)?;
                            return self
                                .compile_tensor_iter_axis(t_ptr, elem, rank, args, call_span);
                        }
                        // Phase-11 Arrow IPC twin: the tensor block's header
                        // carries rank + dims, so only the element
                        // description crosses to the runtime
                        // (`src/codegen/arrow.rs`).
                        "to_arrow_ipc" => {
                            let (elem, unsigned) = (info.elem, info.elem_unsigned);
                            let t_ptr = self.tensor_ptr_for_var(name)?;
                            let (elem_size, kind) = self.tensor_arrow_elem_desc(elem, unsigned)?;
                            return self.compile_arrow_tensor_to_ipc(t_ptr, elem_size, kind);
                        }
                        _ => {}
                    }
                }
                // Vec/String methods (owned or ref)
                if self.var_types.vec_elem_types.contains_key(name.as_str()) {
                    let data_ptr = self.get_data_ptr(name).unwrap();
                    match self.compile_vec_method(name, data_ptr, method, args) {
                        Ok(v) => return Ok(v),
                        Err(e) => {
                            // S6c blanket-Vec: `impl Trait for Vec[i64]` emits a
                            // `Vec.<method>` fn. When the builtin dispatcher has
                            // no arm for `method` but such a user impl fn exists,
                            // fall through to the generic user-impl dispatch
                            // (`inferred_receiver_type` → `Vec.method`, below) —
                            // otherwise loud-fail with the builtin's error.
                            // B-2026-08-12-32 — `String.{method}` joins the same
                            // test. A `String` variable is registered in
                            // `vec_elem_types` (it shares Vec's
                            // `{ptr,len,cap}` shape), so an `impl Trait for
                            // String` method lands in this dispatcher, gets no
                            // arm, and — before this — loud-failed the build
                            // with "Vec/String method 'describe' is not yet
                            // supported in codegen" even though the
                            // typechecker had resolved it.
                            //
                            // That combination is the shape worth naming: the
                            // typecheck-side half of this row without this line
                            // is check-green and interp-green with BOTH
                            // compiled backends dead, which is a strictly worse
                            // failure than the `no method` it replaced.
                            let has_user_impl = ["Vec", "VecDeque", "String"]
                                .iter()
                                .any(|t| self.user_impl_method_exists(call_span, t, method));

                            // B-2026-08-13-7 — a `Slice[T]` variable is
                            // registered in `vec_elem_types` TOO (it is built
                            // from one), so this dispatcher runs first and gets
                            // the method before the slice block below ever sees
                            // it. Without this clause an `impl Trait for
                            // Slice[T]` call loud-failed here with the Vec
                            // message, which is why the row's symptom named
                            // "Vec/String" for a slice receiver. The
                            // `slice_elem_types` conjunct keeps a genuine `Vec`
                            // receiver out: a program with a `Slice`-only impl
                            // must still reject `v.describe()` rather than
                            // dispatch a Vec to it.
                            let slice_has_user_impl =
                                self.var_types.slice_elem_types.contains_key(name.as_str())
                                    && self.user_impl_method_exists(call_span, "Slice", method);

                            if !has_user_impl && !slice_has_user_impl {
                                return Err(e);
                            }
                            // fall through to user-impl dispatch (via the slice
                            // block below when it is a slice receiver)
                        }
                    }
                }
                // Slice[T] / mut Slice[T] read-only methods. For an OWNED
                // slice the stack alloca holds the 2-field `{ptr, i64}` struct
                // directly (see `slice_struct_type`); for a `ref Slice[T]` /
                // `mut ref Slice[T]` parameter the alloca holds a pointer TO
                // that struct instead. `get_data_ptr` normalizes both to a
                // pointer at the `{ptr, i64}` header (owned → the alloca as-is,
                // ref → one load through it), so every method below GEPs off
                // `slice_ptr`, not the raw `slot.ptr`. Using `slot.ptr` for a
                // ref param GEP'd into the pointer-to-header itself and read
                // the caller's stack words as if they were slice fields —
                // `get_unchecked` then indexed the header struct instead of the
                // buffer and printed the data-pointer / len as "elements"
                // (B-2026-07-02-28). The `xs[i]` index path already routes
                // through `get_data_ptr` (`compile_slice_index`); this mirrors
                // it for the method family.
                if self.var_types.slice_elem_types.contains_key(name.as_str()) {
                    let i64_t = self.context.i64_type();
                    let slice_ty = self.slice_struct_type();
                    let slice_ptr = self.get_data_ptr(name).ok_or_else(|| {
                        format!("Slice.{method}: no data pointer for slice '{name}'")
                    })?;
                    match method {
                        "len" => {
                            let len_ptr = self
                                .builder
                                .build_struct_gep(slice_ty, slice_ptr, 1, "slice.len.ptr")
                                .unwrap();
                            let len = self
                                .builder
                                .build_load(i64_t, len_ptr, "slice.len")
                                .unwrap();
                            return Ok(len);
                        }
                        "is_empty" => {
                            let len_ptr = self
                                .builder
                                .build_struct_gep(slice_ty, slice_ptr, 1, "slice.len.ptr")
                                .unwrap();
                            let len = self
                                .builder
                                .build_load(i64_t, len_ptr, "slice.len")
                                .unwrap()
                                .into_int_value();
                            let zero = i64_t.const_zero();
                            let is_empty = self
                                .builder
                                .build_int_compare(IntPredicate::EQ, len, zero, "slice.is_empty")
                                .unwrap();
                            return Ok(is_empty.into());
                        }
                        // `Slice[T].get_unchecked(i) -> T` — direct-index read
                        // with NO bounds check (mirror of `Vec.get_unchecked`,
                        // `vec_method.rs`). GEP field 0 → load data ptr → GEP
                        // elem at idx → load, skipping `emit_split_bounds_check`.
                        // UB on out-of-range; the unsafe-block requirement is
                        // enforced upstream by `unsafe_lint`. Reaching here
                        // means that check already passed.
                        "get_unchecked" => {
                            if args.is_empty() {
                                return Err(
                                    "Slice.get_unchecked requires an index argument".to_string()
                                );
                            }
                            let ptr_ty = self.context.ptr_type(AddressSpace::default());
                            let elem_ty =
                                *self.var_types.slice_elem_types.get(name.as_str()).unwrap();
                            let idx_val = self.compile_expr(&args[0].value)?.into_int_value();
                            let data_pp = self
                                .builder
                                .build_struct_gep(slice_ty, slice_ptr, 0, "s.uchk.data.pp")
                                .unwrap();
                            let data = self
                                .builder
                                .build_load(ptr_ty, data_pp, "s.uchk.data")
                                .unwrap()
                                .into_pointer_value();
                            let elem_ptr = unsafe {
                                self.builder
                                    .build_gep(elem_ty, data, &[idx_val], "s.uchk.elem.ptr")
                                    .unwrap()
                            };
                            let val = self
                                .builder
                                .build_load(elem_ty, elem_ptr, "s.uchk.elem")
                                .unwrap();
                            return Ok(val);
                        }
                        // `Slice[T].binary_search(x) -> Option[i64]`. Same
                        // algorithm as the Vec path; the only difference is the
                        // 2-field `{ptr, len}` slice header (no `cap`). Shares
                        // `compile_binary_search`, so the duplicate-key index
                        // matches the interpreter exactly.
                        "binary_search" => {
                            if args.len() != 1 {
                                return Err("Slice.binary_search requires 1 argument".to_string());
                            }
                            let elem_name = self.vec_elem_type_name(name).ok_or_else(|| {
                                "Slice.binary_search: could not resolve the element type \
                                 in codegen"
                                    .to_string()
                            })?;
                            let elem_ty =
                                *self.var_types.slice_elem_types.get(name.as_str()).unwrap();
                            let ptr_ty = self.context.ptr_type(AddressSpace::default());
                            let data = {
                                let p = self
                                    .builder
                                    .build_struct_gep(slice_ty, slice_ptr, 0, "bs.s.data.p")
                                    .unwrap();
                                self.builder
                                    .build_load(ptr_ty, p, "bs.s.data")
                                    .unwrap()
                                    .into_pointer_value()
                            };
                            let len = {
                                let p = self
                                    .builder
                                    .build_struct_gep(slice_ty, slice_ptr, 1, "bs.s.len.p")
                                    .unwrap();
                                self.builder
                                    .build_load(i64_t, p, "bs.s.len")
                                    .unwrap()
                                    .into_int_value()
                            };
                            return self
                                .compile_binary_search(data, len, elem_ty, &elem_name, &args[0]);
                        }
                        // B-2026-08-14-9 — `chunks(n)` / `windows(n)`, the
                        // view-producers. Both return `Vec[Slice[T]]`, so the
                        // result is a Vec whose ELEMENT is a 2-field slice
                        // header: allocate `count` headers, fill each with an
                        // `{ptr, len}` view into the receiver's own buffer, and
                        // hand back the `{data, len, cap}` aggregate.
                        //
                        // Nothing is copied and nothing is owned. Every header
                        // borrows the receiver, which is what makes the result
                        // cheap and also why the outer Vec needs no element
                        // drop — a slice header owns no allocation. The
                        // existing `chunks`/`windows` codegen is unrelated: it
                        // fuses `named_vec.iter().chunks(n)` into a collect
                        // pipeline that materializes real sub-Vecs, and is
                        // gated to a literal `n` over a named Vec.
                        //
                        // The two differ only in the count and the stride:
                        // chunks tile (`ceil(len / n)` of them, the last one
                        // short), windows overlap (`len - n + 1` of them, all
                        // exactly `n`). A non-positive `n` or a window wider
                        // than the slice yields an empty Vec, matching the
                        // interpreter rather than trapping.
                        "chunks" | "windows" => {
                            if args.len() != 1 {
                                return Err(format!("Slice.{method} requires 1 argument"));
                            }
                            let overlapping = method == "windows";
                            let elem_ty =
                                *self.var_types.slice_elem_types.get(name.as_str()).unwrap();
                            let (data, len) = self.slice_data_and_len(slice_ty, slice_ptr, "cw");
                            let n = self.compile_expr(&args[0].value)?;
                            let n = self.coerce_to_i64(n)?;
                            let fn_val = self.current_fn.unwrap();
                            let zero = i64_t.const_zero();
                            let one = i64_t.const_int(1, false);
                            // `n <= 0` would divide by zero (chunks) or produce
                            // a nonsense count (windows); clamp the count to 0
                            // and let the empty-Vec path below handle it.
                            let n_ok = self
                                .builder
                                .build_int_compare(inkwell::IntPredicate::SGT, n, zero, "cw.n.ok")
                                .unwrap();
                            let n_safe = self
                                .builder
                                .build_select(n_ok, n, one, "cw.n.safe")
                                .unwrap()
                                .into_int_value();
                            let raw_count = if overlapping {
                                // len - n + 1
                                let d = self.builder.build_int_sub(len, n_safe, "cw.w.d").unwrap();
                                self.builder.build_int_add(d, one, "cw.w.c").unwrap()
                            } else {
                                // (len + n - 1) / n
                                let a = self.builder.build_int_add(len, n_safe, "cw.c.a").unwrap();
                                let b = self.builder.build_int_sub(a, one, "cw.c.b").unwrap();
                                self.builder
                                    .build_int_signed_div(b, n_safe, "cw.c.c")
                                    .unwrap()
                            };
                            let pos = self
                                .builder
                                .build_int_compare(
                                    inkwell::IntPredicate::SGT,
                                    raw_count,
                                    zero,
                                    "cw.cnt.pos",
                                )
                                .unwrap();
                            let keep = self.builder.build_and(pos, n_ok, "cw.cnt.keep").unwrap();
                            let count = self
                                .builder
                                .build_select(keep, raw_count, zero, "cw.cnt")
                                .unwrap()
                                .into_int_value();
                            // One allocation of `count` slice headers, and NONE
                            // when `count` is 0 — see `alloc_buffer_or_null`.
                            // The original comment here claimed the zero-size
                            // block "the empty Vec then owns and frees like any
                            // other"; that was wrong, and `chunks`/`windows` on
                            // an empty receiver leaked it (B-2026-08-14-20).
                            let hdr_size = slice_ty.size_of().unwrap();
                            let alloc_bytes = self.checked_alloc_bytes(count, hdr_size, "cw")?;
                            let buf = self.alloc_buffer_or_null(count, alloc_bytes, "cw")?;
                            let idx = self.create_entry_alloca(fn_val, "cw.i", i64_t.into());
                            let _ = self.builder.build_store(idx, zero);
                            let head = self.context.append_basic_block(fn_val, "cw.head");
                            let body = self.context.append_basic_block(fn_val, "cw.body");
                            let done = self.context.append_basic_block(fn_val, "cw.done");
                            self.builder.build_unconditional_branch(head).unwrap();
                            self.builder.position_at_end(head);
                            let i = self
                                .builder
                                .build_load(i64_t, idx, "cw.cur")
                                .unwrap()
                                .into_int_value();
                            let go = self
                                .builder
                                .build_int_compare(inkwell::IntPredicate::SLT, i, count, "cw.go")
                                .unwrap();
                            self.builder
                                .build_conditional_branch(go, body, done)
                                .unwrap();
                            self.builder.position_at_end(body);
                            let (off, sub_len) = if overlapping {
                                (i, n_safe)
                            } else {
                                let off = self.builder.build_int_mul(i, n_safe, "cw.off").unwrap();
                                // The final chunk is short: min(n, len - off).
                                let rest = self.builder.build_int_sub(len, off, "cw.rest").unwrap();
                                let short = self
                                    .builder
                                    .build_int_compare(
                                        inkwell::IntPredicate::SLT,
                                        rest,
                                        n_safe,
                                        "cw.short",
                                    )
                                    .unwrap();
                                let l = self
                                    .builder
                                    .build_select(short, rest, n_safe, "cw.sublen")
                                    .unwrap()
                                    .into_int_value();
                                (off, l)
                            };
                            let sub_ptr = unsafe {
                                self.builder
                                    .build_gep(elem_ty, data, &[off], "cw.sub.p")
                                    .unwrap()
                            };
                            let mut hdr = slice_ty.get_undef();
                            hdr = self
                                .builder
                                .build_insert_value(hdr, sub_ptr, 0, "cw.hdr.d")
                                .unwrap()
                                .into_struct_value();
                            hdr = self
                                .builder
                                .build_insert_value(hdr, sub_len, 1, "cw.hdr.l")
                                .unwrap()
                                .into_struct_value();
                            let slot = unsafe {
                                self.builder
                                    .build_gep(slice_ty, buf, &[i], "cw.slot")
                                    .unwrap()
                            };
                            let _ = self.builder.build_store(slot, hdr);
                            let next = self.builder.build_int_add(i, one, "cw.next").unwrap();
                            let _ = self.builder.build_store(idx, next);
                            self.builder.build_unconditional_branch(head).unwrap();
                            self.builder.position_at_end(done);
                            let vec_ty = self.vec_struct_type();
                            let mut agg = vec_ty.get_undef();
                            agg = self
                                .builder
                                .build_insert_value(agg, buf, 0, "cw.v.d")
                                .unwrap()
                                .into_struct_value();
                            agg = self
                                .builder
                                .build_insert_value(agg, count, 1, "cw.v.l")
                                .unwrap()
                                .into_struct_value();
                            agg = self
                                .builder
                                .build_insert_value(agg, count, 2, "cw.v.c")
                                .unwrap()
                                .into_struct_value();
                            return Ok(agg.into());
                        }
                        // B-2026-08-14-20 — `to_vec()`, the only `Slice`
                        // method that produces an OWNED container. Everything
                        // else on this surface either reads the view or cuts
                        // another view out of it, so this is the one arm whose
                        // result the caller has to free — hence a fresh
                        // `{data, len, cap=len}` with `cap == len`, the shape
                        // every Vec cleanup path already understands.
                        //
                        // A trivially-copyable element is one `memcpy` of the
                        // whole range. A heap element must be DEEP-cloned per
                        // slot, or the returned Vec and the view's source would
                        // both own the same inner buffers and both free them:
                        // `let v = b.to_vec()` over a `Slice[String]` is two
                        // owners of every String. Same memory-ABI clone helper
                        // (`clone(src_ptr, dst_ptr)`) the `fill` arm below uses.
                        "to_vec" => {
                            if !args.is_empty() {
                                return Err("Slice.to_vec takes no arguments".to_string());
                            }
                            let elem_ty =
                                *self.var_types.slice_elem_types.get(name.as_str()).unwrap();
                            let elem_te = self
                                .var_types
                                .var_elem_type_exprs
                                .get(name.as_str())
                                .cloned();
                            let needs_clone = match elem_te.as_ref() {
                                Some(te) => !super::vec_method::is_trivially_copyable_te(te),
                                // Same rule as `fill`: with no element
                                // `TypeExpr` there is no way to know whether a
                                // slot owns anything, and guessing "it does
                                // not" would alias every heap element into a
                                // double free. Fail loudly instead.
                                None => {
                                    return Err(
                                        "Slice.to_vec: could not resolve the element type in \
                                         codegen"
                                            .to_string(),
                                    )
                                }
                            };
                            let (data, len) = self.slice_data_and_len(slice_ty, slice_ptr, "tv");
                            let elem_size = {
                                use inkwell::types::BasicType;
                                elem_ty.size_of().unwrap()
                            };
                            let alloc_bytes = self.checked_alloc_bytes(len, elem_size, "tv")?;
                            let buf = self.alloc_buffer_or_null(len, alloc_bytes, "tv")?;
                            if needs_clone {
                                let te = elem_te.unwrap();
                                let clone_fn = self.emit_clone_fn_for_type_expr(&te);
                                let fn_val = self.current_fn.unwrap();
                                let zero = i64_t.const_zero();
                                let one = i64_t.const_int(1, false);
                                let idx = self.create_entry_alloca(fn_val, "tv.i", i64_t.into());
                                let _ = self.builder.build_store(idx, zero);
                                let head = self.context.append_basic_block(fn_val, "tv.head");
                                let body = self.context.append_basic_block(fn_val, "tv.body");
                                let done = self.context.append_basic_block(fn_val, "tv.done");
                                self.builder.build_unconditional_branch(head).unwrap();
                                self.builder.position_at_end(head);
                                let i = self
                                    .builder
                                    .build_load(i64_t, idx, "tv.cur")
                                    .unwrap()
                                    .into_int_value();
                                let go = self
                                    .builder
                                    .build_int_compare(inkwell::IntPredicate::SLT, i, len, "tv.go")
                                    .unwrap();
                                self.builder
                                    .build_conditional_branch(go, body, done)
                                    .unwrap();
                                self.builder.position_at_end(body);
                                let src = unsafe {
                                    self.builder
                                        .build_gep(elem_ty, data, &[i], "tv.src")
                                        .unwrap()
                                };
                                let dst = unsafe {
                                    self.builder
                                        .build_gep(elem_ty, buf, &[i], "tv.dst")
                                        .unwrap()
                                };
                                let _ = self.builder.build_call(
                                    clone_fn,
                                    &[src.into(), dst.into()],
                                    "",
                                );
                                let next = self.builder.build_int_add(i, one, "tv.next").unwrap();
                                let _ = self.builder.build_store(idx, next);
                                self.builder.build_unconditional_branch(head).unwrap();
                                self.builder.position_at_end(done);
                            } else {
                                self.builder
                                    .build_memcpy(buf, 1, data, 1, alloc_bytes)
                                    .map_err(|e| format!("Slice.to_vec memcpy: {e}"))?;
                            }
                            let vec_ty = self.vec_struct_type();
                            let mut agg = vec_ty.get_undef();
                            agg = self
                                .builder
                                .build_insert_value(agg, buf, 0, "tv.v.d")
                                .unwrap()
                                .into_struct_value();
                            agg = self
                                .builder
                                .build_insert_value(agg, len, 1, "tv.v.l")
                                .unwrap()
                                .into_struct_value();
                            agg = self
                                .builder
                                .build_insert_value(agg, len, 2, "tv.v.c")
                                .unwrap()
                                .into_struct_value();
                            return Ok(agg.into());
                        }
                        // B-2026-08-14-9 — `swap` and `fill`, implemented
                        // directly because `Vec` has no arm for either, so
                        // there is nothing to route to.
                        //
                        // `swap` is element-type agnostic: it exchanges two
                        // whole values in place, so ownership never changes
                        // hands and a plain load/load/store/store is correct
                        // for a `String` or a struct element as much as for an
                        // `i64`. An out-of-range index is a NO-OP rather than a
                        // panic, matching the interpreter's `if i < len && j <
                        // len` exactly; whether both surfaces should instead
                        // trap is that method's question, not this row's.
                        "swap" => {
                            if args.len() != 2 {
                                return Err("Slice.swap requires 2 arguments".to_string());
                            }
                            let elem_ty =
                                *self.var_types.slice_elem_types.get(name.as_str()).unwrap();
                            let ptr_ty = self.context.ptr_type(AddressSpace::default());
                            let (data, len) = self.slice_data_and_len(slice_ty, slice_ptr, "swp");
                            let i = self.compile_expr(&args[0].value)?;
                            let i = self.coerce_to_i64(i)?;
                            let j = self.compile_expr(&args[1].value)?;
                            let j = self.coerce_to_i64(j)?;
                            let fn_val = self.current_fn.unwrap();
                            let zero = i64_t.const_zero();
                            let ok = {
                                use inkwell::IntPredicate::{SGE, SLT};
                                let a = self
                                    .builder
                                    .build_int_compare(SLT, i, len, "swp.i.lt")
                                    .unwrap();
                                let b = self
                                    .builder
                                    .build_int_compare(SLT, j, len, "swp.j.lt")
                                    .unwrap();
                                let c = self
                                    .builder
                                    .build_int_compare(SGE, i, zero, "swp.i.ge")
                                    .unwrap();
                                let d = self
                                    .builder
                                    .build_int_compare(SGE, j, zero, "swp.j.ge")
                                    .unwrap();
                                let ab = self.builder.build_and(a, b, "swp.ab").unwrap();
                                let cd = self.builder.build_and(c, d, "swp.cd").unwrap();
                                self.builder.build_and(ab, cd, "swp.ok").unwrap()
                            };
                            let do_bb = self.context.append_basic_block(fn_val, "swp.do");
                            let end_bb = self.context.append_basic_block(fn_val, "swp.end");
                            self.builder
                                .build_conditional_branch(ok, do_bb, end_bb)
                                .unwrap();
                            self.builder.position_at_end(do_bb);
                            let pi = unsafe {
                                self.builder
                                    .build_gep(elem_ty, data, &[i], "swp.pi")
                                    .unwrap()
                            };
                            let pj = unsafe {
                                self.builder
                                    .build_gep(elem_ty, data, &[j], "swp.pj")
                                    .unwrap()
                            };
                            let vi = self.builder.build_load(elem_ty, pi, "swp.vi").unwrap();
                            let vj = self.builder.build_load(elem_ty, pj, "swp.vj").unwrap();
                            let _ = self.builder.build_store(pi, vj);
                            let _ = self.builder.build_store(pj, vi);
                            self.builder.build_unconditional_branch(end_bb).unwrap();
                            self.builder.position_at_end(end_bb);
                            let _ = ptr_ty;
                            return Ok(self.context.struct_type(&[], false).get_undef().into());
                        }
                        // `fill(v)` overwrites every slot in `[0, len)`.
                        //
                        // The element type decides how much work a slot is. A
                        // trivially-copyable element is one store per slot. A
                        // heap-backed one has to DROP what is already there and
                        // put a distinct buffer in its place, or the fill either
                        // leaks every overwritten value or aliases one buffer
                        // into every slot and double-frees at scope end. The
                        // per-type `clone` / `drop` helpers are memory-ABI
                        // (`clone(src_ptr, dst_ptr)`, `drop(ptr)`), the same
                        // pair the repeat-literal path uses.
                        //
                        // The ARGUMENT is consumed, exactly as `Vec.filled`
                        // consumes its value: it is moved into slot 0 and
                        // cloned into the rest, so nothing double-owns it. An
                        // empty slice has no slot to move into, so the argument
                        // is dropped there instead — otherwise `s.fill(x)` on
                        // an empty slice would leak `x`.
                        "fill" => {
                            if args.len() != 1 {
                                return Err("Slice.fill requires 1 argument".to_string());
                            }
                            let elem_ty =
                                *self.var_types.slice_elem_types.get(name.as_str()).unwrap();
                            let elem_te = self
                                .var_types
                                .var_elem_type_exprs
                                .get(name.as_str())
                                .cloned();
                            let needs_clone = match elem_te.as_ref() {
                                Some(te) => !super::vec_method::is_trivially_copyable_te(te),
                                // No element `TypeExpr` means no way to know
                                // whether a slot owns anything. Guessing "it
                                // does not" would leak or double-free a heap
                                // element silently, so fail loudly instead.
                                None => {
                                    return Err(
                                        "Slice.fill: could not resolve the element type in \
                                         codegen"
                                            .to_string(),
                                    )
                                }
                            };
                            let (data, len) = self.slice_data_and_len(slice_ty, slice_ptr, "fil");
                            let val = self.compile_expr(&args[0].value)?;
                            let val =
                                self.coerce_scalar_to_type_src(val, elem_ty, Some(&args[0].value));
                            let fn_val = self.current_fn.unwrap();
                            let zero = i64_t.const_zero();
                            let one = i64_t.const_int(1, false);
                            let fill_loop = |cg: &mut Self,
                                             from: inkwell::values::IntValue<'ctx>,
                                             tag: &str,
                                             body_fn: &dyn Fn(
                                &mut Self,
                                inkwell::values::PointerValue<'ctx>,
                            )| {
                                let idx = cg.create_entry_alloca(
                                    fn_val,
                                    &format!("{tag}.i"),
                                    i64_t.into(),
                                );
                                let _ = cg.builder.build_store(idx, from);
                                let head = cg
                                    .context
                                    .append_basic_block(fn_val, &format!("{tag}.head"));
                                let body = cg
                                    .context
                                    .append_basic_block(fn_val, &format!("{tag}.body"));
                                let done = cg
                                    .context
                                    .append_basic_block(fn_val, &format!("{tag}.done"));
                                cg.builder.build_unconditional_branch(head).unwrap();
                                cg.builder.position_at_end(head);
                                let cur = cg
                                    .builder
                                    .build_load(i64_t, idx, &format!("{tag}.cur"))
                                    .unwrap()
                                    .into_int_value();
                                let go = cg
                                    .builder
                                    .build_int_compare(
                                        inkwell::IntPredicate::SLT,
                                        cur,
                                        len,
                                        &format!("{tag}.go"),
                                    )
                                    .unwrap();
                                cg.builder.build_conditional_branch(go, body, done).unwrap();
                                cg.builder.position_at_end(body);
                                let slot = unsafe {
                                    cg.builder
                                        .build_gep(elem_ty, data, &[cur], &format!("{tag}.slot"))
                                        .unwrap()
                                };
                                body_fn(cg, slot);
                                let next = cg
                                    .builder
                                    .build_int_add(cur, one, &format!("{tag}.next"))
                                    .unwrap();
                                let _ = cg.builder.build_store(idx, next);
                                cg.builder.build_unconditional_branch(head).unwrap();
                                cg.builder.position_at_end(done);
                            };
                            if !needs_clone {
                                fill_loop(self, zero, "fil", &|cg, slot| {
                                    let _ = cg.builder.build_store(slot, val);
                                });
                                return Ok(self.context.struct_type(&[], false).get_undef().into());
                            }
                            let te = elem_te.unwrap();
                            let drop_fn = self.emit_drop_fn_for_type_expr(&te);
                            let clone_fn = self.emit_clone_fn_for_type_expr(&te);
                            // 1. drop every value the slice currently holds.
                            fill_loop(self, zero, "fil.d", &|cg, slot| {
                                let _ = cg.builder.build_call(drop_fn, &[slot.into()], "");
                            });
                            // 2. move the argument into slot 0 (when there is
                            //    one) and deep-clone it into slots 1..len.
                            let src = self.create_entry_alloca(fn_val, "fil.src", elem_ty);
                            let _ = self.builder.build_store(src, val);
                            let nonempty = self
                                .builder
                                .build_int_compare(
                                    inkwell::IntPredicate::SGT,
                                    len,
                                    zero,
                                    "fil.nonempty",
                                )
                                .unwrap();
                            let move_bb = self.context.append_basic_block(fn_val, "fil.move");
                            let empty_bb = self.context.append_basic_block(fn_val, "fil.empty");
                            let join_bb = self.context.append_basic_block(fn_val, "fil.join");
                            self.builder
                                .build_conditional_branch(nonempty, move_bb, empty_bb)
                                .unwrap();
                            self.builder.position_at_end(move_bb);
                            let _ = self.builder.build_store(data, val);
                            self.builder.build_unconditional_branch(join_bb).unwrap();
                            self.builder.position_at_end(empty_bb);
                            // Nothing consumed the argument — free it here so
                            // `empty.fill(x)` does not leak `x`.
                            let _ = self.builder.build_call(drop_fn, &[src.into()], "");
                            self.builder.build_unconditional_branch(join_bb).unwrap();
                            self.builder.position_at_end(join_bb);
                            fill_loop(self, one, "fil.c", &|cg, slot| {
                                let _ =
                                    cg.builder
                                        .build_call(clone_fn, &[src.into(), slot.into()], "");
                            });
                            return Ok(self.context.struct_type(&[], false).get_undef().into());
                        }
                        // B-2026-08-14-8 — READ-ONLY accessors shared with
                        // `Vec`, routed rather than reimplemented.
                        //
                        // A slice header is `{ptr, len}` and a Vec header is
                        // `{ptr, len, cap}` with the first two fields at the
                        // same indices, so a borrowed VIEW of the slice — the
                        // same `cap == 0` convention `zero_cap_if_ref_heap_borrow`
                        // already uses for a borrowed heap value — is a valid
                        // Vec header for any method that only reads `ptr`/`len`.
                        // `compile_vec_method` then supplies the whole
                        // implementation, including the Option-payload word
                        // splitting `first`/`last`/`get` need for a multi-word
                        // element, which is why routing beats four hand-written
                        // arms that would each have to repeat it.
                        //
                        // B-2026-08-14-9 extends the same route to the
                        // IN-PLACE mutators. -8 held them back on the grounds
                        // that `cap == 0` is a lie to anything that could grow
                        // or reallocate — true, and the reason `push` / `insert`
                        // / `extend` must never come this way. But `reverse`,
                        // `sort`, `sort_by` and `sort_by_key` cannot change the
                        // length: each is a permutation of `[0, len)` that reads
                        // `ptr` and `len` and writes through `ptr`. Checked arm
                        // by arm in `compile_vec_method` — none of the four
                        // GEPs field 2 — so the borrowed view is as valid for
                        // them as for the reads, and the write lands in the
                        // caller's buffer because the view aliases it rather
                        // than copying.
                        //
                        // The comparator concern was real but is answered by
                        // the same publish-and-restore this arm already does
                        // for the element type: `sort` picks unsigned ordering
                        // via `vec_elem_type_name` and `sort_by_key` recovers
                        // its key shape via `var_elem_type_exprs`, both keyed by
                        // VARIABLE NAME, and a slice binding registers its
                        // element `TypeExpr` under exactly that name already.
                        //
                        // `fill` and `swap` are NOT here because `Vec` has no
                        // arm to route to; they are implemented directly over
                        // the 2-field header below. `chunks` / `windows` /
                        // `split_at` return views and have no Vec analogue at
                        // all.
                        "contains" | "first" | "last" | "get" | "is_sorted" | "reverse"
                        | "sort" | "sort_by" | "sort_by_key" => {
                            let elem_ty =
                                *self.var_types.slice_elem_types.get(name.as_str()).unwrap();
                            let ptr_ty = self.context.ptr_type(AddressSpace::default());
                            let data = {
                                let p = self
                                    .builder
                                    .build_struct_gep(slice_ty, slice_ptr, 0, "s.view.data.p")
                                    .unwrap();
                                self.builder.build_load(ptr_ty, p, "s.view.data").unwrap()
                            };
                            let len = {
                                let p = self
                                    .builder
                                    .build_struct_gep(slice_ty, slice_ptr, 1, "s.view.len.p")
                                    .unwrap();
                                self.builder.build_load(i64_t, p, "s.view.len").unwrap()
                            };
                            let fn_val = self.current_fn.unwrap();
                            let vec_ty = self.vec_struct_type();
                            let view = self.create_entry_alloca(fn_val, "s.view", vec_ty.into());
                            let dp = self
                                .builder
                                .build_struct_gep(vec_ty, view, 0, "s.view.d")
                                .unwrap();
                            let lp = self
                                .builder
                                .build_struct_gep(vec_ty, view, 1, "s.view.l")
                                .unwrap();
                            let cp = self
                                .builder
                                .build_struct_gep(vec_ty, view, 2, "s.view.c")
                                .unwrap();
                            let _ = self.builder.build_store(dp, data);
                            let _ = self.builder.build_store(lp, len);
                            let _ = self.builder.build_store(cp, i64_t.const_zero());
                            // `compile_vec_method` resolves the element type by
                            // VARIABLE NAME through `vec_elem_types`, which a
                            // slice binding is absent from — it would silently
                            // default to i64 and misread the stride for any
                            // wider element. Publish the slice's own element
                            // type under that name for the call and restore
                            // after, so no slice binding is left masquerading as
                            // a Vec for anything downstream.
                            let saved = self.var_types.vec_elem_types.insert(name.clone(), elem_ty);
                            let out = self.compile_vec_method(name, view, method, args);
                            match saved {
                                Some(prev) => {
                                    self.var_types.vec_elem_types.insert(name.clone(), prev);
                                }
                                None => {
                                    self.var_types.vec_elem_types.remove(name.as_str());
                                }
                            }
                            return out;
                        }
                        // B-2026-08-13-7 — the `Slice` peer of the blanket-`Vec`
                        // fallthrough above. When the builtin dispatcher has no
                        // arm for `method` but `impl Trait for Slice[..]`
                        // emitted a `Slice.<method>` fn, fall out of this match
                        // (and out of the enclosing `if`) so the generic
                        // user-impl dispatch below gets it, instead of
                        // loud-failing here. Without this line the typecheck
                        // half of this row is check-green and interp-green with
                        // BOTH compiled backends dead — strictly worse than the
                        // `no method` error it replaced.
                        _ if self.user_impl_method_exists(call_span, "Slice", method) => {}
                        _ => {
                            return Err(format!(
                                "codegen: no handler for slice method '{}' on '{}'",
                                method, name
                            ));
                        }
                    }
                }
                // Map methods
                if self.mapset.map_key_types.contains_key(name.as_str()) {
                    let name = name.clone();
                    // B-2026-08-12-34 — the `Map` peer of the blanket-`Vec`
                    // fallthrough above: when the builtin dispatcher has no arm
                    // for `method` but `impl Trait for Map[..]` emitted a
                    // `Map.<method>` fn, fall through to the generic user-impl
                    // dispatch instead of loud-failing with
                    // "codegen: Map.<m> not yet implemented".
                    match self.compile_map_method(&name, method, args) {
                        Ok(v) => return Ok(v),
                        Err(e) => {
                            if !self.user_impl_method_exists(call_span, "Map", method) {
                                return Err(e);
                            }
                            // fall through to user-impl dispatch
                        }
                    }
                }
                // Set methods
                if self.mapset.set_elem_types.contains_key(name.as_str()) {
                    let name = name.clone();
                    // B-2026-08-12-34 — the `Set` peer of the `Map` fallthrough
                    // above; see it for the reasoning.
                    match self.compile_set_method(&name, method, args) {
                        Ok(v) => return Ok(v),
                        Err(e) => {
                            if !self.user_impl_method_exists(call_span, "Set", method) {
                                return Err(e);
                            }
                            // fall through to user-impl dispatch
                        }
                    }
                }
                // HTTP handler ABI trampoline (2026-05-09): `Request.path()`
                // and `Request.method()`. Request is an opaque-ptr value
                // (F2) wrapping the runtime's `*const KaracHttpRequest`.
                // Both methods round-trip through runtime externs that
                // return a borrowed `*const c_char`; we copy the bytes into
                // a fresh Kāra String per call so the resulting value
                // outlives the request struct (which the runtime drops
                // after the handler returns).
                if matches!(self.var_types.var_type_names.get(name.as_str()), Some(n) if n == "Request")
                    && (method == "path" || method == "method")
                {
                    let name = name.clone();
                    return self.compile_request_string_method(&name, method);
                }
                if matches!(self.var_types.var_type_names.get(name.as_str()), Some(n) if n == "Request")
                    && method == "body"
                {
                    let name = name.clone();
                    return self.compile_request_body(&name);
                }
                // `Request.header(name)` — case-insensitive lookup
                // through `karac_runtime_http_request_header`; returns
                // `Option[String]` with `Some(value)` on hit, `None` on
                // miss. Args[0] is the header name (`String`); the
                // payload's data ptr + len round-trip through the FFI.
                if matches!(self.var_types.var_type_names.get(name.as_str()), Some(n) if n == "Request")
                    && method == "header"
                    && args.len() == 1
                {
                    let name = name.clone();
                    return self.compile_request_header(&name, &args[0].value);
                }
                // `Request.headers()` / `Request.query()` — full-map
                // iteration returning `Vec[(String, String)]`. Both walk
                // the runtime's count + indexed key/val accessors, copying
                // each borrowed cstring into a fresh owned String (phase-8
                // line 13). `query()` parameters are percent-decoded
                // runtime-side; `headers()` keys are hyper-normalized
                // lowercase.
                if matches!(self.var_types.var_type_names.get(name.as_str()), Some(n) if n == "Request")
                    && (method == "headers" || method == "query")
                    && args.is_empty()
                {
                    let name = name.clone();
                    let kind = if method == "headers" {
                        super::http::RequestPairsKind::Headers
                    } else {
                        super::http::RequestPairsKind::Query
                    };
                    return self.compile_request_pairs(&name, kind);
                }
                // Phase-8 line 17 — `Client.get(url)` / `Client.post(url,
                // body)` codegen dispatch. Receiver `c` is `ref self`,
                // an empty `Client { }` struct; the runtime extern does
                // the real synchronous-HTTP work via `ureq`. Returns
                // `Result[Response, HttpError]` packed into the seeded
                // 5-word Result enum (`tag, w0=status, w1..w3=body /
                // err.message`).
                if matches!(self.var_types.var_type_names.get(name.as_str()), Some(n) if n == "Client")
                    && (method == "get" || method == "post")
                {
                    return self.compile_client_http_method(method, args);
                }
                // `reg.read()` / `reg.write(v)` on a `VolatileCell[T]` binding —
                // the transparent MMIO wrapper (`volatile_cell.kara`). The
                // binding's alloca IS the inner `T` (transparent, like Atomic),
                // so the access lowers to a volatile load / store directly
                // against that slot. Intercepted here because codegen does not
                // compile the baked type's `.kara` method bodies (which call the
                // `volatile_read` / `ptr.const` surface); the interpreter DOES
                // execute them and rejects — matching the codegen-only posture
                // of the raw volatile intrinsics.
                if matches!(self.var_types.var_type_names.get(name.as_str()), Some(n) if n == "VolatileCell")
                    && matches!(method, "read" | "write")
                {
                    let name = name.clone();
                    return self.compile_volatile_cell_method(&name, method, args);
                }
                // Phase-8 line 24 — `Client.request(method, url)`
                // chained-builder entrypoint. Returns a `RequestBuilder
                // { handle: i64 }` wrapping a runtime-side
                // `HTTP_BUILDERS` entry; subsequent `.header(...) /
                // .body(...) / .timeout(...) / .send()` chain through
                // the handle-based runtime externs.
                if matches!(self.var_types.var_type_names.get(name.as_str()), Some(n) if n == "Client")
                    && method == "request"
                {
                    return self.compile_client_request_builder(args);
                }
                // Phase-8 line 24 — `RequestBuilder` chained methods
                // (`.header / .body / .timeout / .send`). Configuration
                // methods route through `compile_request_builder_setter`
                // (handle stays the same, runtime entry mutates); `.send()`
                // routes through `compile_request_builder_send` (consumes
                // the handle and packs the result).
                if matches!(self.var_types.var_type_names.get(name.as_str()), Some(n) if n == "RequestBuilder")
                {
                    if method == "header" || method == "body" || method == "timeout" {
                        let name = name.clone();
                        return self.compile_request_builder_setter(&name, method, args);
                    }
                    if method == "send" && args.is_empty() {
                        let name = name.clone();
                        return self.compile_request_builder_send(&name);
                    }
                }
                // Phase-8 line 17 slice 3 — `Response.status() / .body()`
                // and `HttpError.message()`. Stdlib stubs are
                // `#[compiler_builtin]` so the bodies are never compiled;
                // these arms emit direct field extractions on the
                // receiver's struct value. `status` is i64 — passthrough.
                // `body` / `message` are owned-String returns and route
                // through `karac_string_clone` so the caller's String
                // doesn't alias the receiver's field (a subsequent
                // `Drop` of either would double-free otherwise).
                // `body` / `text` clone the entity as a `String`; `bytes`
                // clones the same buffer as `Vec[u8]` (phase-8 line 32) —
                // the buffers are layout-identical (`{ptr, len, cap}`), so
                // all three route through `compile_response_accessor`; the
                // binding's surface type (String vs Vec[u8]) comes from the
                // typechecker, not the cloned aggregate.
                if matches!(self.var_types.var_type_names.get(name.as_str()), Some(n) if n == "Response")
                    && matches!(method, "status" | "body" | "bytes")
                    && args.is_empty()
                {
                    let name = name.clone();
                    return self.compile_response_accessor(&name, method);
                }
                // Phase-8 line 39 — `Response.header(name)` →
                // `Option[String]`. Distinct from the no-arg accessors
                // above: it takes the header name and routes through
                // `compile_response_header`, which reads the hidden
                // `headers` handle off the Response and calls the runtime
                // `HTTP_RESPONSE_HEADERS` side-table lookup
                // (case-insensitive, RFC 7230 §3.2).
                if matches!(self.var_types.var_type_names.get(name.as_str()), Some(n) if n == "Response")
                    && method == "header"
                    && args.len() == 1
                {
                    let name = name.clone();
                    return self.compile_response_header(&name, &args[0].value);
                }
                // Phase-8 line 39 follow-up — `Response.headers()` →
                // `Vec[(String, String)]` (full-map iteration over the
                // captured response headers, mirror of `Request.headers()`).
                // Routes through `compile_response_pairs`, which reads the
                // hidden headers handle and drives the runtime count +
                // key_at/val_at iteration accessors.
                if matches!(self.var_types.var_type_names.get(name.as_str()), Some(n) if n == "Response")
                    && method == "headers"
                    && args.is_empty()
                {
                    let name = name.clone();
                    return self.compile_response_pairs(&name);
                }
                if matches!(self.var_types.var_type_names.get(name.as_str()), Some(n) if n == "HttpError")
                    && method == "message"
                    && args.is_empty()
                {
                    let name = name.clone();
                    return self.compile_http_error_message(&name);
                }
                // `std.json` codegen-side wiring (phase-8 line 435):
                // `j.stringify()` on a Kāra-side `Json` enum value.
                // Loads the receiver's four enum words, dispatches
                // through the synthesized `__karac_json_kara_to_ffi`
                // walker, calls `karac_runtime_json_stringify`, and
                // copies the result into a fresh Kāra String.
                if method == "stringify"
                    && args.is_empty()
                    && matches!(self.var_types.var_type_names.get(name.as_str()), Some(n) if n == "Json")
                {
                    let recv_val = self.compile_expr(object)?;
                    return self.compile_json_stringify(recv_val);
                }
            }
        }

        // `std.json` codegen-side wiring (phase-8 line 435) —
        // non-identifier-receiver path: `Json.Object([...]).stringify()`,
        // `Json.Array([...]).stringify()`, etc. The receiver is an
        // expression that evaluates to a Json enum value; we compile it
        // to its struct value and feed it through the same lowering
        // path as the identifier case.
        if method == "stringify" && args.is_empty() && self.expr_is_json_value(object) {
            let recv_val = self.compile_expr(object)?;
            return self.compile_json_stringify(recv_val);
        }

        // `Atomic[T].load(ord)` / `Atomic[T].store(value, ord)` —
        // compiler-builtin dispatch for the transparent Atomic wrapper.
        // Two receiver shapes supported:
        //   1. Identifier `a` where `var_type_names["a"] == "Atomic"`
        //      (populated by the let-stmt Atomic-RHS recognizer in
        //      `compile_stmt`).
        //   2. FieldAccess `c.count` where struct `Counter`'s `count`
        //      field has declared type `Atomic[T]` (recorded in
        //      `struct_field_type_names`). This is the shape the
        //      `karac migrate --atomic` consumer-rewrite emits
        //      (L215c-cons), so the migration tool's output compiles
        //      under codegen without further hand-conversion.
        // Both shapes route through `compile_atomic_method`, which
        // resolves the receiver's storage pointer + element LLVM type,
        // pattern-matches the trailing `MemoryOrdering.X` qualified-
        // variant arg into an `inkwell::AtomicOrdering`, and emits
        // `load atomic` / `store atomic`.
        if matches!(
            method,
            "load"
                | "store"
                | "fetch_add"
                | "fetch_sub"
                | "swap"
                | "fetch_and"
                | "fetch_or"
                | "fetch_xor"
                | "compare_exchange"
        ) && self.is_atomic_receiver(object)
        {
            return self.compile_atomic_method(object, method, args);
        }

        // Phase 6 "Channel AOT codegen lowering": `Sender.send/clone` and
        // `Receiver.recv/try_recv` on a channel-end receiver. `Sender`/
        // `Receiver` are empty stdlib structs (no impl bodies), so this must
        // intercept BEFORE the user-impl dispatch below — otherwise the
        // qualified `Sender.send` lookup misses and the call falls through to
        // a "no such method" error. The gate is the presence of a
        // typechecker-recorded `channel_elem_types` entry at this call span:
        // only `infer_channel_method` populates that table, so an entry is an
        // unambiguous, scope-stable "this is a channel op" signal (the
        // `var_type_names` receiver-type lookup is unreliable here — the
        // statement-hoisting pre-pass binds channel ends then resets
        // `var_type_names` before this method-call pass runs).
        if self
            .conc
            .channel_elem_types
            .contains_key(&(call_span.offset, call_span.length))
        {
            return self.compile_channel_method(object, method, args, call_span);
        }

        // `Secret.expose() -> ref T` (std.secret): a `#[compiler_builtin]` field
        // borrow. `inner` is field 0, so its address IS the receiver struct's
        // base pointer (offset 0) — return that pointer as the `ref T`. The
        // caller-side `let x = s.expose()` binds it as a deref-on-use ref-local
        // (stmts.rs), and `user_ref_method_names` + `ref_return_inner_types`
        // (auto-populated for every impl method whose return type is `ref`) wire
        // the borrow ABI with no extra work. `expose_mut` is a follow-on slice —
        // it falls through to a clean "no such method" error here (matching the
        // interpreter) until its write-back path lands.
        if method == "expose"
            && args.is_empty()
            && matches!(
                self.inferred_receiver_type(object).as_deref(),
                Some("Secret")
            )
        {
            let name = match &object.kind {
                ExprKind::Identifier(n) => n.clone(),
                ExprKind::SelfValue => "self".to_string(),
                _ => {
                    return Err(
                        "`Secret.expose` requires an identifier or `self` receiver".to_string()
                    )
                }
            };
            let recv_ptr = self.get_data_ptr(&name).ok_or_else(|| {
                format!("`Secret.expose`: no storage pointer for receiver `{name}`")
            })?;
            return Ok(recv_ptr.into());
        }

        // `Secret.ct_eq(other) -> bool` (std.secret): constant-time equality
        // via the reviewed `karac_secret_ct_eq` runtime helper (OR-accumulate +
        // `black_box` barrier — deliberately NOT the short-circuiting
        // `karac_string_cmp`, whose first-differing-byte exit is the timing
        // leak `ct_eq` exists to close). `inner` is field 0, so the Secret
        // struct pointer IS the inner String's `{ptr,len,cap}` header (offset
        // 0). v1 supports `Secret[String]`; any other inner type fails closed
        // to a clear error here (the interpreter mirrors this with a runtime
        // error), so both backends reject the same programs.
        if method == "ct_eq"
            && args.len() == 1
            && matches!(
                self.inferred_receiver_type(object).as_deref(),
                Some("Secret")
            )
            && self.contract_state.secret_type_is_stdlib
        {
            // Resolve the receiver's inner `T` (shared with the arg, since the
            // signature is `ct_eq(ref self, other: ref Secret[T])`). The parser
            // sets `MethodCall.span == receiver.span`, so the receiver's own
            // `Secret[T]` type is shadowed at that span by the call's `bool`
            // result — the argument's span does not collide, so consult it
            // first, then fall back to the receiver span.
            let arg_span = &args[0].value.span;
            let inner_te = self
                .contract_state
                .secret_inner_types
                .get(&(arg_span.offset, arg_span.length))
                .or_else(|| {
                    self.contract_state
                        .secret_inner_types
                        .get(&(object.span.offset, object.span.length))
                });
            let inner_is_string = inner_te
                .map(|te| self.is_string_type_expr(te))
                .unwrap_or(false);
            if !inner_is_string {
                return Err(
                    "`Secret.ct_eq` is only supported for `Secret[String]` in v1 \
                     (Vec[u8] / [u8; N] are planned)"
                        .to_string(),
                );
            }
            let name_of = |e: &Expr| -> Option<String> {
                match &e.kind {
                    ExprKind::Identifier(n) => Some(n.clone()),
                    ExprKind::SelfValue => Some("self".to_string()),
                    _ => None,
                }
            };
            let recv_name = name_of(object).ok_or_else(|| {
                "`Secret.ct_eq` requires an identifier or `self` receiver".to_string()
            })?;
            let arg_name = name_of(&args[0].value).ok_or_else(|| {
                "`Secret.ct_eq` requires an identifier argument (compare two named secrets); \
                 an inline expression argument is not yet supported"
                    .to_string()
            })?;
            let recv_ptr = self.get_data_ptr(&recv_name).ok_or_else(|| {
                format!("`Secret.ct_eq`: no storage pointer for receiver `{recv_name}`")
            })?;
            let arg_ptr = self.get_data_ptr(&arg_name).ok_or_else(|| {
                format!("`Secret.ct_eq`: no storage pointer for argument `{arg_name}`")
            })?;
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let i64_t = self.context.i64_type();
            let str_ty = self.vec_struct_type();
            // Load `{ptr, len}` (fields 0, 1) from each inner String header.
            let ap_p = self
                .builder
                .build_struct_gep(str_ty, recv_ptr, 0, "cte.a.ptr.p")
                .unwrap();
            let a_ptr = self
                .builder
                .build_load(ptr_ty, ap_p, "cte.a.ptr")
                .unwrap()
                .into_pointer_value();
            let al_p = self
                .builder
                .build_struct_gep(str_ty, recv_ptr, 1, "cte.a.len.p")
                .unwrap();
            let a_len = self
                .builder
                .build_load(i64_t, al_p, "cte.a.len")
                .unwrap()
                .into_int_value();
            let bp_p = self
                .builder
                .build_struct_gep(str_ty, arg_ptr, 0, "cte.b.ptr.p")
                .unwrap();
            let b_ptr = self
                .builder
                .build_load(ptr_ty, bp_p, "cte.b.ptr")
                .unwrap()
                .into_pointer_value();
            let bl_p = self
                .builder
                .build_struct_gep(str_ty, arg_ptr, 1, "cte.b.len.p")
                .unwrap();
            let b_len = self
                .builder
                .build_load(i64_t, bl_p, "cte.b.len")
                .unwrap()
                .into_int_value();
            let ct_fn = self
                .module
                .get_function("karac_secret_ct_eq")
                .unwrap_or_else(|| {
                    let ft = i64_t.fn_type(
                        &[ptr_ty.into(), i64_t.into(), ptr_ty.into(), i64_t.into()],
                        false,
                    );
                    self.module.add_function(
                        "karac_secret_ct_eq",
                        ft,
                        Some(inkwell::module::Linkage::External),
                    )
                });
            let raw = self
                .builder
                .build_call(
                    ct_fn,
                    &[a_ptr.into(), a_len.into(), b_ptr.into(), b_len.into()],
                    "cte.call",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            // Helper returns 1 / 0; map to an `i1` bool.
            let as_bool = self
                .builder
                .build_int_compare(IntPredicate::NE, raw, i64_t.const_zero(), "cte.bool")
                .unwrap();
            return Ok(as_bool.into());
        }

        // `OnceLock`/`OnceCell` `set`/`get`/`is_set`/`get_or_init` on a local
        // binding. Gated on the receiver identifier's membership in
        // `once_var_types` (populated by `register_var_from_type_expr` from the
        // `OnceLock[T]`/`OnceCell[T]` annotation) — the baked stdlib structs
        // have no user impl, so this must intercept before the user-impl lookup
        // below. B-8 OnceLock codegen.
        if let ExprKind::Identifier(recv_name) = &object.kind {
            if self
                .var_types
                .once_var_types
                .contains_key(recv_name.as_str())
                && matches!(method, "set" | "get" | "is_set" | "get_or_init")
            {
                return self.compile_once_method(recv_name, method, args);
            }
        }

        // `Interner` `intern`/`resolve`/`len` on a local binding. Gated on the
        // receiver identifier's membership in `interner_vars` (populated at
        // the `let i = Interner.new()` bind site / `Interner` annotation) —
        // the baked stdlib struct has no user impl, so this must intercept
        // before the user-impl lookup below. Phase-8 Interner codegen.
        if let ExprKind::Identifier(recv_name) = &object.kind {
            if self.var_types.interner_vars.contains(recv_name.as_str())
                && matches!(method, "intern" | "resolve" | "len")
            {
                return self.compile_interner_method(recv_name, method, args);
            }
        }

        // `Arena[T]` `push`/`get`/`len`/`high_water_mark`/`rewind_to` on a
        // local binding. Gated on the receiver identifier's membership in
        // `arena_vars` (populated at the annotated `let a: Arena[T] =
        // Arena.new()` bind site) — same interception posture as the
        // Interner arm above. Phase-8 Arena codegen.
        if let ExprKind::Identifier(recv_name) = &object.kind {
            if self.var_types.arena_vars.contains_key(recv_name.as_str())
                && matches!(
                    method,
                    "push" | "get" | "len" | "high_water_mark" | "rewind_to"
                )
            {
                return self.compile_arena_method(recv_name, method, args);
            }
        }

        // User impl-block method on a struct receiver: route `obj.method(args)`
        // through the `Type.method` function emitted by the impl-block pass.
        // Requires knowing the object's declared type; the typechecker stashes
        // it via `var_type_names` for struct-kind locals.
        if let Some(receiver_type) = self.inferred_receiver_type(object) {
            // B-2026-08-13-8 — `receiver_type` is a head name, which stops being
            // an identity once two impls target two instantiations of one type.
            // `impl_dispatch_segment_at` swaps in the qualified segment the
            // typechecker resolved for THIS call site; a no-op (returns the
            // head) for every program with no colliding impl group.
            let receiver_type = self.impl_dispatch_segment_at(call_span, method, &receiver_type);
            let qualified = format!("{}.{}", receiver_type, method);
            // B-2026-08-06-20 — never dispatch a GENERIC impl method through the
            // unmangled `Type.method` symbol, even when one exists in the module.
            //
            // That symbol is the fallback slot the monomorphizer emits when it
            // cannot resolve an instantiation (`mangle_mono_name` appends a `$`
            // token only for a param it has a subst for, so an empty subst
            // mangles back to the base name). It therefore carries WHICHEVER
            // instantiation was emitted first, and this arm — which runs before
            // the generic path below — handed every later receiver to it:
            //
            //     let n: i64 = env.args().len();
            //     Box { v: n + 41 }.take()                 // i64, unresolvable
            //     let b = Box { v: "s".repeat(n) };
            //     b.take()                                 // String, resolvable
            //
            // fixed `@Box.take` at `{ i64 }` and then called it with
            // `{ {ptr,i64,i64} }` — "Call parameter type does not match function
            // signature", while either receiver form ALONE built and `karac run`
            // was correct throughout. Falling through sends the second call to
            // the monomorphizer, which mangles it per instantiation.
            //
            // A non-generic method is unaffected: `generic_fns` carries a
            // `Type.method` key only for generic ones, so the lookup below is
            // the same one it always was.
            if let Some(fn_val) = self
                .module
                .get_function(&qualified)
                .filter(|_| !self.mono_state.generic_fns.contains_key(&qualified))
            {
                // B-2026-08-01-7: an OWNED-`self` method CONSUMES the
                // receiver — a named value-enum binding's payload-bodies
                // walk must disarm, exactly like `let c = b;` (the arm
                // channel inside the method is the payload's sole owner
                // now). Without this, `b.into_id()` fired the body twice on
                // both backends: once from the arm channel, once from b's
                // still-armed walk at scope exit. Enum receivers only — a
                // struct receiver's single walk fire is the established
                // in-parity convention (probe b159_ownstruct).
                if let ExprKind::Identifier(recv_name) = &object.kind {
                    if matches!(
                        self.impl_method_self_and_borrow_return(&receiver_type, method),
                        Some((crate::ast::SelfParam::Owned, _))
                    ) && self
                        .var_types
                        .var_type_names
                        .get(recv_name.as_str())
                        .is_some_and(|tn| self.type_decls.enum_layouts.contains_key(tn.as_str()))
                    {
                        let recv_name = recv_name.clone();
                        self.suppress_container_elem_bodies_for_var(&recv_name);
                    }
                }
                // Inspect the resolved fn's first param to decide the receiver
                // calling convention: pointer-typed (ref self / mut ref self)
                // means pass the address of the receiver's storage; struct-
                // typed (owned self) means pass the value. Mismatch silently
                // miscompiles, which is exactly what shipped before this slice.
                let first_param_is_ptr = fn_val
                    .get_type()
                    .get_param_types()
                    .first()
                    .map(|t| matches!(t, BasicMetadataTypeEnum::PointerType(_)))
                    .unwrap_or(false);
                // OWNED self on a SHARED receiver is ALSO ptr-typed at the
                // LLVM level (shared types lower to the heap pointer), but
                // it expects the heap pointer BY VALUE — one indirection
                // less than the ref-self convention (whose body loads the
                // param to reach the heap ptr; see `compile_function`'s
                // `inner_type_of_ref` registration). The LLVM param type
                // can't discriminate the two, so consult the source-level
                // ref flag recorded by `declare_function`. Before this,
                // `node.step()` with `fn step(self)` passed the STACK SLOT
                // address: the callee's entry rc_inc then incremented a
                // stack word as if it were a refcount header and every
                // field GEP was one indirection off — the owned-`self`
                // receiver-move segfault (bugs.md entry, 2026-06-05).
                let first_param_is_ref = self
                    .fn_sig
                    .fn_param_ref
                    .get(&qualified)
                    .and_then(|flags| flags.first().copied())
                    .unwrap_or(false);
                // Receiver storage name for the ptr-self ABI. Both `obj`
                // (Identifier) and `self` (SelfValue, registered under the
                // synthesized "self" param) resolve to a data pointer; any
                // other shape has no stable storage to address.
                let recv_storage_name: Option<&str> = match &object.kind {
                    ExprKind::Identifier(var_name) => Some(var_name.as_str()),
                    ExprKind::SelfValue => Some("self"),
                    _ => None,
                };
                // B-2026-08-18-11 — an OWNED receiver that lowers to a bare
                // `ptr` is passed BY VALUE, not by address. The rule used to
                // name shared types specifically, but shared-ness was never
                // what made it true: what makes it true is that the callee
                // declared the param OWNED (`!first_param_is_ref`) while its
                // LLVM type came out `ptr` — the value IS the pointer, so the
                // address of the slot holding it is one indirection too many.
                //
                // A `Map[K, V]` / `Set[T]` handle is exactly that shape, and
                // it was taking the address path: `for x in self` over an
                // owned `Set[i64]` self handed `karac_map_iter_new` the
                // receiver's STACK SLOT instead of the handle, so it iterated
                // nothing and the method returned 0 while `--interp` returned
                // 12 — a silent wrong answer, the same "one indirection off"
                // failure the shared-receiver case was added for. `Vec` and
                // `Slice` were never affected: they lower to `{ptr, len, cap}`
                // / `{ptr, len}` STRUCTS, so `first_param_is_ptr` is false and
                // they already took the value path.
                //
                // The one owned-and-`ptr` shape that genuinely wants an
                // address is an AArch64 INDIRECT struct param (> 16 B
                // `#[repr(C)]`, which arrives as a pointer to the caller's
                // copy), so it is carved out by name+index. That map is empty
                // on x86-64, which is also why this axis cannot be measured
                // there — the carve-out is what keeps arm64 on the path it
                // has today.
                let receiver_is_indirect_struct = self
                    .target_abi
                    .indirect_struct_params
                    .get(&qualified)
                    .is_some_and(|v| v.iter().any(|(idx, _)| *idx == 0));
                let receiver_arg: BasicMetadataValueEnum<'ctx> =
                    if first_param_is_ptr && !first_param_is_ref && !receiver_is_indirect_struct {
                        // Owned pointer-shaped `self`: the pointer by value. For a
                        // shared receiver the callee's entry emits its own
                        // receive-inc ("caller keeps its reference"), so there is
                        // no caller-side count change here either. `compile_expr`
                        // on an Identifier loads the slot, which holds exactly the
                        // heap ptr / container handle.
                        self.compile_expr(object)?.into()
                    } else if first_param_is_ptr {
                        if let Some(ptr) = recv_storage_name.and_then(|n| self.get_data_ptr(n)) {
                            ptr.into()
                        } else {
                            // Non-identifier / non-self receiver into a ref-self
                            // method: unsupported in v1 (would require materializing
                            // a temporary alloca). Fall through to compile_expr;
                            // mismatched ABI may surface at link time.
                            self.compile_expr(object)?.into()
                        }
                    } else {
                        self.compile_expr(object)?.into()
                    };
                // Positional-arg ref/slice lowering — mirrors the free-fn
                // path in `compile_call` (call_dispatch.rs). Before this, the
                // method path compiled every non-receiver arg by *value* and
                // pushed it, so a `ref`/`mut ref` struct param (declared `ptr`)
                // received a `{ ... }` struct value and module verification
                // rejected the call (B-2026-06-12-8). The receiver occupies
                // param slot 0 (`self`), so source arg `i` maps to declared
                // param slot `i + 1` in `fn_param_ref` / `fn_param_slice_elem`
                // (both keyed by the qualified `Type.method` name and built
                // from `func.params`, whose element 0 is the receiver).
                let ref_flags = self
                    .fn_sig
                    .fn_param_ref
                    .get(&qualified)
                    .cloned()
                    .unwrap_or_default();
                let slice_elems = self
                    .fn_sig
                    .fn_param_slice_elem
                    .get(&qualified)
                    .cloned()
                    .unwrap_or_default();
                // B-2026-08-21-38 (codegen half) — the mutate-through subset,
                // which needs a pointer to the caller's PLACE rather than to a
                // copy. Same key and same self-at-0 indexing as `ref_flags`.
                let mut_ref_flags = self
                    .fn_sig
                    .fn_param_mut_ref
                    .get(&qualified)
                    .cloned()
                    .unwrap_or_default();
                let mut compiled_args: Vec<BasicMetadataValueEnum<'ctx>> = vec![receiver_arg];
                for (i, a) in args.iter().enumerate() {
                    let pidx = i + 1;
                    let is_ref = ref_flags.get(pidx).copied().unwrap_or(false);
                    if !is_ref {
                        // B-2026-07-28-4: by-value struct arg whose param
                        // declined the entry copy — move it, don't leave both
                        // sides owning it.
                        self.move_declined_copy_struct_arg(&a.value);
                        // B-2026-08-09-15 — the method spelling of the free-fn
                        // arm in `compile_call`: when the method returns a
                        // payload bound out of this arg, the caller's walk is a
                        // second body for one value. Measured before this:
                        // `t.take(b)` printed `drop 7 e7` alongside the returned
                        // value's own body while `--interp` printed one.
                        // `pidx` (not `i`) is the DECLARED slot, the same index
                        // `fn_param_ref` is keyed by, with the receiver at 0.
                        // The AST does not agree — `Function::params` excludes
                        // `self` — and the predicate shifts back by one itself
                        // rather than making every caller know that.
                        if self.callee_returns_enum_arg_payload(&qualified, pidx) {
                            if let ExprKind::Identifier(var_name) = &a.value.kind {
                                let var_name = var_name.clone();
                                self.suppress_container_elem_bodies_for_var(&var_name);
                            }
                        }
                    }
                    if is_ref {
                        // B-2026-08-21-23 — a `ref Slice[T]` / `mut ref Slice[T]`
                        // slot fed an `Array[T, N]`. The callee receives a
                        // POINTER to a `{ptr,len}` header; an Array binding's
                        // storage is its raw ELEMENTS, with no header anywhere.
                        // Without this, the `get_data_ptr` fast path immediately
                        // below hands over `&array[0]` and the callee reads
                        // `{ptr,len}` out of the first two elements — measured on
                        // `let b: Array[u8, 3] = [10u8, 20u8, 30u8]; h.by_ref(b)`
                        // as `-129820080518201344` under JIT and a SEGFAULT under
                        // AOT once the body indexes, while `--interp` answered 3.
                        // A struct-FIELD array missed that fast path and instead
                        // reached the slice coercion further down, which pushes a
                        // header VALUE into a `ptr` slot: module verification
                        // failure. One carve-out covers both, because both want
                        // the same thing — synthesize the header, pass its
                        // address.
                        //
                        // This is B-2026-06-19-1's fix, which landed on the
                        // free-function path (`call_dispatch.rs`) and the generic
                        // path (`mono.rs`) and never on this one. Placed FIRST,
                        // above the identifier fast path, for the same reason it
                        // is first there: the fast path is what produces the
                        // wrong pointer.
                        //
                        // Array sources ONLY, mirroring that gate: a `Vec`
                        // binding's storage starts with `{ptr,len}` (a header
                        // superset) and a `Slice` / `ref Slice` binding's
                        // `get_data_ptr` already yields a header pointer, so both
                        // forward correctly below — intercepting them would
                        // re-coerce a ref-slice binding and corrupt the forward.
                        if let Some(Some(elem_ty)) = slice_elems.get(pidx).cloned() {
                            if self.arg_is_array_source(&a.value) {
                                if let Some(slice_val) = self.coerce_to_slice(&a.value, elem_ty)? {
                                    let ptr = self.materialize_rvalue_for_ref_arg(slice_val, i);
                                    compiled_args.push(ptr.into());
                                    continue;
                                }
                            }
                        }
                        // Identifier place — pass its data pointer.
                        if let ExprKind::Identifier(var_name) = &a.value.kind {
                            if let Some(ptr) = self.get_data_ptr(var_name) {
                                compiled_args.push(ptr.into());
                                continue;
                            }
                        }
                        // `vec[idx]` borrow — pass the element pointer in place
                        // (no shallow-copy + drop double-free).
                        if let Some(elem_ptr) = self.ref_arg_index_borrow_ptr(&a.value)? {
                            compiled_args.push(elem_ptr.into());
                            continue;
                        }
                        // B-2026-08-21-38 (codegen half) — a `mut ref T` slot
                        // fed a FIELD or TUPLE-INDEX place. Neither took the
                        // identifier fast path above, so the argument fell
                        // through to the rvalue path below and the callee
                        // mutated a COPY: measured on
                        // `struct Box { v: i64 }` … `h.bump(mut b.v)` as `4`
                        // under JIT and AOT where the byte-identical FREE
                        // function `free_bump(mut b.v)` answered `5` on every
                        // surface. This is B-2026-08-05-41's fix, which landed
                        // on the free-function path (`call_dispatch.rs`) and
                        // the generic path (`mono.rs`) and never on this one.
                        //
                        // Gated on the PARAMETER MODE, not the payload type,
                        // exactly as the free-fn arm is: a mutate-through
                        // borrow of a place always needs the place, while a
                        // read-only `ref` param is left alone because a copy is
                        // a correct borrow for a reader and the arms below do
                        // type-specific work on that path.
                        if mut_ref_flags.get(pidx).copied().unwrap_or(false) {
                            if let Some(place_ptr) = self.mut_ref_place_arg_ptr(&a.value) {
                                compiled_args.push(place_ptr.into());
                                continue;
                            }
                        }
                        // Borrow-returning call in ref-arg position — forward
                        // the raw `-> ref T` borrow pointer (bypass the
                        // direct-use intercept that would load the pointee).
                        if self.is_borrow_returning_call_expr(&a.value) {
                            let prev = self.compiling_ref_return_let_rhs;
                            self.compiling_ref_return_let_rhs = true;
                            let ptr = self.compile_expr(&a.value);
                            self.compiling_ref_return_let_rhs = prev;
                            compiled_args.push(ptr?.into());
                            continue;
                        }
                    }
                    // `Slice[T]` / `mut Slice[T]` param: synthesize the
                    // `{ ptr, i64 }` header from an Array/Vec/slice arg.
                    if let Some(Some(elem_ty)) = slice_elems.get(pidx).cloned() {
                        if let Some(slice_val) = self.coerce_to_slice(&a.value, elem_ty)? {
                            compiled_args.push(slice_val.into());
                            continue;
                        }
                    }
                    if is_ref {
                        // Rvalue ref path: a non-place arg (literal, call
                        // return, arithmetic) bound to a `ref T` param.
                        // Materialize into a stack temp so the callee receives
                        // the `ptr` ABI its signature declares; queue the
                        // temp's cleanup (the callee only borrows). Mirrors the
                        // free-fn rvalue-ref arm in `compile_call`.
                        let val = self.compile_expr(&a.value)?;
                        let cur_fn = self
                            .builder
                            .get_insert_block()
                            .and_then(|bb| bb.get_parent())
                            .expect("compile_method_call inside a function context");
                        let temp = self.create_entry_alloca(
                            cur_fn,
                            &format!("ref_rvalue_marg{i}"),
                            val.get_type(),
                        );
                        self.builder.build_store(temp, val).unwrap();
                        self.queue_ref_rvalue_arg_cleanup(temp, val, &a.value);
                        compiled_args.push(temp.into());
                        continue;
                    }
                    let val = self.compile_expr(&a.value)?;
                    // `Option[shared T]` arg-share discipline — mirrors
                    // the free-fn call path in `compile_call`: a tracked
                    // Identifier binding gets a tag+null-guarded inner
                    // inc so the callee receives an independent +1 (its
                    // param `RcDecOption` decs at exit; the caller's
                    // binding keeps its own +1 for its scope-exit dec);
                    // a FieldAccess arg reading an `Option[shared T]`
                    // field gets the loaded inner inc'd. Without these,
                    // reusing a binding after passing it — `m.total(c);
                    // m.total(c)` — read freed memory (2026-06-05 probe,
                    // pre-existing on the conventional ABI).
                    self.share_option_shared_ref_for_arg(&a.value);
                    self.share_option_shared_field_ref_for_arg(&a.value, val);
                    // B-2026-06-12-10: register the caller-side drop for an inline
                    // owned-aggregate arg (enum-variant constructor / tuple /
                    // struct literal) — the lexer's `self.make_spanned(Token.V(…))`
                    // reaches here, not the free-fn `compile_call` path. Shared
                    // helper keeps both arg loops in lockstep.
                    // No passthrough guard on this path (a method that returns
                    // its own by-value param is not a shape this arm models), so
                    // the arg never flows into the result here.
                    self.track_inline_owned_aggregate_arg(val, &a.value, false);
                    // Fresh-heap by-value arg materialization — the method-call
                    // sibling of the #20 arm in `compile_call` (call_dispatch.rs).
                    // A `String`/`Vec` produced by a Call/MethodCall (or a block /
                    // inline-temp-Vec heap index) and passed DIRECTLY by value to a
                    // method — `lx.ident_matches("Fn".to_string())` — has no
                    // consuming binding, and an owned `String`/`Vec` by-value param
                    // is NOT freed by the callee (it lands in `owned_vecstr_params`
                    // for retaining-consume deep-copy, never a callee-side
                    // `track_vec_var`), so the temp orphaned and leaked one buffer
                    // per call (B-2026-06-20: the self-host string-eq method leak).
                    // `materialize_owned_temp` self-guards on the Vec/String LLVM
                    // shape (+ the `owned_temp_drops` hint for Map), so non-heap
                    // args are a no-op; `rhs_stages_fstr_acc` excludes a struct/enum
                    // `.to_string()` (its f-string acc already owns a caller-scope
                    // cleanup). The free-fn arm's full rationale applies verbatim.
                    let is_block_arg = matches!(
                        &a.value.kind,
                        ExprKind::Block(_)
                            | ExprKind::Seq(_)
                            | ExprKind::Unsafe(_)
                            | ExprKind::LabeledBlock { .. }
                    );
                    // B-2026-07-02-6 follow-on: collection-literal args share
                    // #20's orphaned-fresh-heap shape (see the free-fn arm).
                    let is_collection_literal_arg = matches!(
                        &a.value.kind,
                        ExprKind::ArrayLiteral(_)
                            | ExprKind::PrefixCollectionLiteral { .. }
                            | ExprKind::RepeatLiteral { .. }
                    );
                    let is_fresh_heap_call_arg = (self.expr_yields_fresh_owned_temp(&a.value)
                        || self.expr_is_inline_temp_vec_heap_index(&a.value)
                        || is_collection_literal_arg)
                        && self.llvm_ty_is_vec_struct(val.get_type())
                        && !self.rhs_stages_fstr_acc(&a.value);
                    if is_block_arg || is_fresh_heap_call_arg {
                        self.materialize_owned_temp(
                            val,
                            (a.value.span.offset, a.value.span.length),
                        );
                    }
                    // A fresh bare-`shared` (RC-box) call / variant-ctor result
                    // passed by value: the callee inc/decs net-zero, so the caller
                    // still owns the temp's +1 and must release it — the bare-shared
                    // sibling of the arm above (`fresh_arg_bare_shared_heap_type`
                    // self-excludes a `g(make())` passthrough chain).
                    if val.is_pointer_value() {
                        if let Some(heap_type) = self.fresh_arg_bare_shared_heap_type(&a.value) {
                            self.track_rc_var(
                                "__owned_arg_tmp",
                                val.into_pointer_value(),
                                heap_type,
                            );
                        }
                    }
                    // B-2026-07-11-37 — the method-call sibling of the by-value
                    // Option/Result/boxed MOVE suppression the free-fn path already
                    // applies (`compile_call`, call_dispatch.rs:1535). An inline-heap
                    // `Option[String]` (or `Result` / boxed-enum) binding moved by
                    // value into a `mut ref self` method that OWNS + frees it never
                    // had its caller slot nulled here — so the callee's arm-drop AND
                    // the caller's scope-exit `FreeInlineOptionPayload` freed the same
                    // payload (double-free; interpreter correct — a run/build
                    // divergence with no diagnostic). Zero the source slot so the
                    // caller's tag/cap guard skips. Gated OUT of a return-passthrough
                    // (`fn take(mut ref self, o) -> Option { o }` — the callee hands
                    // `o` back and the caller's RESULT binding owns it, so the source
                    // must stay live); `find_function_ast` resolves the `Type.method`
                    // key against the impl blocks, and the method's `self` occupies
                    // param 0 so the source arg maps to declared param `pidx = i + 1`.
                    // By-ref args never reach here (every `is_ref` arm `continue`s
                    // above), so no borrow gate is needed; the helper self-guards on
                    // the inline/boxed payload sets, leaving shared `Option[shared T]`
                    // (rc inc/dec balanced) and untracked args untouched.
                    let arg_flows_into_return = self
                        .program_snapshot
                        .as_deref()
                        .and_then(|p| super::declarations::find_function_ast(p, &qualified))
                        .is_some_and(|f| crate::ast::fn_returns_param(f, pidx));
                    // B-2026-08-12-1 — same carve-out as the free-fn path: an
                    // ENTRY-COPIED `Option`/`Result` param owns its own buffer,
                    // so the caller keeps its original and must not zero it.
                    // Gating only the free-fn site would leave a method's caller
                    // zeroing a slot the callee no longer takes over, orphaning
                    // the payload — which is how this shape's fixture failed
                    // while every free-fn fixture passed.
                    let entry_copied = self.callee_optres_param_entry_copied(&qualified, pidx);
                    if !arg_flows_into_return && entry_copied.is_none() {
                        self.suppress_inline_option_result_binding_move(&a.value);
                    }
                    if let Some(param_te) = entry_copied {
                        // Both halves, for the reason the free-fn site gives:
                        // the payload buffer and the boxed field envelope are
                        // separate allocations with separate freshness rules.
                        // B-2026-08-12-15.
                        let own_payload = self.optres_arg_is_unowned_temp(&a.value);
                        let own_envelope = self.optres_arg_mints_field_envelope(&a.value);
                        if own_payload || own_envelope {
                            self.track_optres_arg_temp(val, &param_te, own_payload, own_envelope);
                        }
                    }
                    // Signedness-carrying scalar coercion at the METHOD arg
                    // boundary, the twin of the free-fn site in
                    // `call_dispatch.rs`. The boundary sweep below sees only
                    // LLVM values, where `u8` and `i8` are both `i8` and an
                    // int bound for a `double` has no source to ask, so both
                    // conversions have to happen here while `a.value` is in
                    // hand. `compiled_args.len()` is the slot this value is
                    // about to take, so the two never drift.
                    //
                    // B-2026-08-13-18 wired this. Two pre-existing miscompiles
                    // ended at this one missing call, both `karac check`-clean:
                    // `m.set(b)` against `fn set(mut ref self, x: i64)` with
                    // `b: u8` 200 sign-extended to -56 (B-2026-08-13-15 fixed
                    // the free-fn spelling of exactly this and did not reach
                    // the method one), and the same call against `x: f64`
                    // failed module verification outright until the shared
                    // helper learned int→float — after which it would have
                    // printed -56.0 instead, which is why this landed with it
                    // rather than after it.
                    let val =
                        self.coerce_call_arg_scalar(fn_val, compiled_args.len(), val, &a.value);
                    compiled_args.push(val.into());
                }
                // Niche-ABI pack/unpack at the `obj.method(...)` boundary
                // — the receiver occupies position 0 (`self`, never an
                // Option, never a niche position) so source args line up
                // with declared params 1..N.
                self.pack_niche_abi_args(&qualified, &mut compiled_args);
                // Scalar width coercion at the method-arg boundary —
                // mirrors the free-fn site in `call_dispatch.rs`
                // (`p.scale(2)` against `fn scale(self, k: i8)` would
                // otherwise emit a width-mismatched call). See
                // `coerce_scalar_to_type`.
                self.coerce_args_to_fn_params(fn_val, &mut compiled_args);
                let call_site = self
                    .builder
                    .build_call(fn_val, &compiled_args, "usermethod")
                    .unwrap();
                let basic_val = call_site.try_as_basic_value();
                return if basic_val.is_instruction() {
                    // Void-return placeholder: callee returns unit, so fill the
                    // expression slot with const-0 i64. NOT a dispatch fall-through.
                    Ok(self.context.i64_type().const_int(0, false).into())
                } else {
                    Ok(self.unpack_niche_abi_ret(&qualified, basic_val.unwrap_basic()))
                };
            }
        }

        // Non-identifier receiver of Vec / String type — e.g.
        // `list_primes_under(n).len()`. Compile the receiver to a `{ptr,
        // len, cap}` struct value, then service the read-only Vec methods
        // (`len`, `is_empty`) via direct field extraction. Methods that
        // would mutate the receiver (`push`, `sort`, etc.) don't make
        // semantic sense on a temporary — the mutation would be lost when
        // the temp goes out of scope at the end of the statement — so
        // those keep falling through to the dispatch-fail Err below.
        //
        // For element-type-aware Vec methods (`contains`, `get`, `iter`),
        // a follow-up slice can materialize the value to a temporary
        // alloca + synthesize a name + register elem_ty from the typed
        // AST. Today's narrow scope: just `len` and `is_empty`, which
        // are element-type-agnostic.
        // Read-only `len` / `is_empty` on a borrow-LOCAL receiver — a
        // `let n = name_of(u);` / chained borrow result (B-2026-06-07-5).
        // Such a binding is registered in `ref_params` (the let-RHS path
        // stores it as a `ptr` and derefs on use), so `compile_expr(n)`
        // yields the same `{ptr,len,cap}` struct a temp receiver does, and
        // the field-extraction below services it. A ref *parameter* receiver
        // (`s: ref String`) is dispatched by an earlier String arm and never
        // reaches here, so this only rescues the let-bound borrows that
        // otherwise fell through to the dispatch-fail error below. Owned
        // String/Vec locals are likewise handled earlier (via the
        // string/var-type paths); the `== vec_ty` struct guard makes a
        // non-`{ptr,len,cap}` borrow (`ref i64`) fall through safely.
        let borrow_local_recv = matches!(&object.kind, ExprKind::Identifier(n) if self.borrow_vars.ref_params.contains_key(n));

        // `<iter-chain>.count()` — the element-count terminal on a fused
        // iterator chain (B-2026-07-11-19). Placed BEFORE the `len`/`count`
        // materialized-collection intercept below, which would otherwise try to
        // `compile_expr` the chain receiver as a Vec value and fail on the
        // `map`/`filter` adaptor ("no handler for method 'filter'"). Gated on an
        // iterator-chain receiver (MethodCall/Range) and fails closed for any
        // shape the shared peel can't lower (`s.chars().count()` — a
        // materialized `Vec[char]` — falls through to the intercept unchanged).
        if method == "count"
            && args.is_empty()
            && matches!(
                &object.kind,
                ExprKind::MethodCall { .. } | ExprKind::Range { .. }
            )
        {
            if let Some(v) = self.try_compile_iter_chain_count(object, call_span)? {
                return Ok(v);
            }
        }

        // Defer to user-method dispatch when the receiver's type declares its
        // own `len`/`is_empty`/`count` method (`dispatch_key` names an emitted
        // `<Type>.<method>` fn). Otherwise this collection/iterator intercept
        // speculatively `compile_expr(object)`s the receiver — allocating a
        // fresh temp for a `make().count()`-style call — then, finding the
        // value isn't a Vec/String/slice struct, falls through WITHOUT freeing
        // it while the real user-method dispatch re-evaluates `object`, leaking
        // the discarded box (B-2026-07-11-14 — surfaced when `count` joined
        // this arm and collided with a user `fn count(self)`; the latent leak
        // applied to a user `len`/`is_empty` too). Iterator/collection chains
        // (`s.chars().count()`, `make_vec().len()`) declare no such user fn, so
        // they still take the intercept.
        let user_method_for_len_family = dispatch_key.as_deref().is_some_and(|k| {
            self.module.get_function(k).is_some() || self.mono_state.generic_fns.contains_key(k)
        });
        // B-2026-08-18-26 — a FRESH-TEMP `Map`/`Set` receiver is the same
        // hazard the paragraph above describes, one receiver-kind over, and the
        // guard there does not cover it: a builtin `Map`/`Set` declares no user
        // `len`, so `user_method_for_len_family` is false and the intercept
        // speculatively compiled the receiver. A handle is a plain `ptr`, not
        // the Vec/String struct this arm can lower, so it fell through — and
        // `try_compile_freshtemp_mapset_read_method` further down evaluated
        // `object` AGAIN. `mk_set(3).len()` therefore ran `mk_set` twice under
        // `karac build` against once under `--interp`, and only the second
        // handle was drop-tracked, stranding the first (216 bytes for a
        // 3-element `Set`, 604 for a 2-entry `Map[String, i64]`).
        //
        // Deciding it from the typechecker's recorded receiver TYPE keeps the
        // skip ahead of any emission, which is the whole point — returning
        // `Ok(None)` after `compile_expr` cannot un-emit the producer.
        let freshtemp_mapset_recv = self
            .mapset
            .temp_recv_mapset_types
            .contains_key(&(call_span.offset, call_span.length));
        if !user_method_for_len_family
            && !freshtemp_mapset_recv
            && (!matches!(&object.kind, ExprKind::Identifier(_)) || borrow_local_recv)
            && matches!(method, "len" | "is_empty" | "count")
        {
            let recv_val = self.compile_expr(object)?;
            if let BasicValueEnum::StructValue(sv) = recv_val {
                let vec_ty = self.vec_struct_type();
                if sv.get_type() == vec_ty {
                    // General owned-temp tracking, slice 3 (method-chain
                    // receiver temps): when the receiver is a *fresh-owned*
                    // Vec/String temporary (`make_vec().len()`), `len` /
                    // `is_empty` borrow it read-only — so the caller owns the
                    // temp and must drop it. Without this its heap buffer
                    // leaks (the field-extract below reads `len` and discards
                    // the struct, orphaning `data`). Route the receiver value
                    // through the owned-temp chokepoint so a `FreeVecBuffer`
                    // (with the element type from `owned_temp_drops`, closing
                    // nested-heap leaks) drains at scope exit. Gated to
                    // Call/MethodCall: a *place*-expression receiver
                    // (`obj.items.len()`, `arr[0].len()`) reloads a buffer an
                    // existing binding owns, which a second free would
                    // double-free; `expr_yields_fresh_owned_temp` excludes
                    // those (and the `cap > 0` guard in `FreeVecBuffer` keeps
                    // a non-owning / borrowed value safe regardless).
                    // For a heap-bearing element type the chokepoint's
                    // `owned_temp_drops` hint is span-clobbered (the parser
                    // gives a MethodCall its receiver's span, so the chain's
                    // scalar result evicts the receiver's `Vec[T]` from
                    // `expr_types`) and the track degrades to an
                    // outer-buffer-only free — every element String/row/agg
                    // leaks (B-2026-07-31-43: `Env.args().len()`). The
                    // typechecker records the element type in the dedicated
                    // `temp_recv_len_elem_types` table exactly for this
                    // receiver; prefer it, falling back to the chokepoint
                    // when absent (scalar elements — outer free is complete).
                    // …but NOT for an `unwrap`/`expect` over a borrow
                    // accessor (`mk_rows().first().unwrap().len()`,
                    // B-2026-08-01-1): the unwrapped value ALIASES element
                    // storage a get-family materialization already
                    // drop-tracks per-element (the same
                    // `temp_recv_elem_types` record that let `first()`
                    // compile is what tracked the outer temp), so a second
                    // free of the row buffer here aborts. The typechecker
                    // agrees the value is a borrow (`ref Vec[T]` — it
                    // rejects passing it as owned), so there is nothing for
                    // this scope to free.
                    // B-2026-08-05-7 (surface-concat leg): a string concat the
                    // `String.add` desugar skipped stays an `ExprKind::Binary`,
                    // which `expr_yields_fresh_owned_temp` declines — so
                    // `("p:".to_string() + s).len()` freed nothing and leaked
                    // the concat RESULT once per evaluation. The ARGUMENT side
                    // already admits this shape (B-2026-07-21-12); this is the
                    // receiver side of the same gate. Both halves of the
                    // predicate matter — `Add` is also scalar addition, and the
                    // vec-struct value check is what keeps `a + b` over `i64`
                    // out of it.
                    if (self.expr_yields_fresh_owned_temp(object)
                        || self.expr_is_fresh_owned_string_concat(object, recv_val.get_type()))
                        && !self.expr_is_unwrap_of_borrow_accessor(object)
                    {
                        let recv_span_key = (object.span.offset, object.span.length);
                        // Two tables, two keys, and they are NOT interchangeable
                        // (B-2026-08-18-24). `temp_recv_len_elem_types` is
                        // inserted by the typechecker under the CALL's span;
                        // `owned_temp_drops` is recorded against the receiver
                        // EXPRESSION. While `MethodCall` copied its object's
                        // span one key served both, which is precisely why the
                        // difference went unnoticed.
                        let call_span_key = (call_span.offset, call_span.length);
                        if !self.try_track_len_family_recv_temp(recv_val, call_span_key) {
                            self.materialize_owned_temp(recv_val, recv_span_key);
                        }
                    }
                    let i64_t = self.context.i64_type();
                    let len_val = self
                        .builder
                        .build_extract_value(sv, 1, "tmp.vec.len")
                        .unwrap()
                        .into_int_value();
                    return Ok(match method {
                        // `count` is the char-iterator length: `s.chars()`
                        // compiles to a materialized `Vec[char]` here, so its
                        // element count IS `len` (B-2026-07-11-9 gap 1).
                        "len" | "count" => len_val.into(),
                        "is_empty" => self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::EQ,
                                len_val,
                                i64_t.const_zero(),
                                "tmp.vec.is_empty",
                            )
                            .unwrap()
                            .into(),
                        _ => unreachable!(),
                    });
                }
                // Slice-header receiver — `s.bytes().len()`, `slice.len()`
                // where the receiver is a method-chain result. `bytes()` (and
                // the other zero-copy views) return the `{ptr, i64}` slice
                // header, not the `{ptr,len,cap}` Vec struct, so the `vec_ty`
                // branch above misses them and the chain fell through to the
                // dispatch-fail error (B surfaced by kata-katas #722 bench
                // harness's `out[k].bytes().len()`). A slice is a borrowed
                // view that owns no buffer, so there is NO owned-temp drop
                // here — just extract `len` (field 1, same index as the Vec).
                if sv.get_type() == self.slice_struct_type() {
                    let i64_t = self.context.i64_type();
                    let len_val = self
                        .builder
                        .build_extract_value(sv, 1, "tmp.slice.len")
                        .unwrap()
                        .into_int_value();
                    return Ok(match method {
                        "len" | "count" => len_val.into(),
                        "is_empty" => self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::EQ,
                                len_val,
                                i64_t.const_zero(),
                                "tmp.slice.is_empty",
                            )
                            .unwrap()
                            .into(),
                        _ => unreachable!(),
                    });
                }
            }
        }

        // Phase-8 line 24 — `RequestBuilder` non-identifier receiver
        // dispatch. The chained-builder shape
        // `c.request("GET", url).header(...).timeout(...).send()` has
        // each call's receiver as the prior call's return value (a
        // MethodCall expr, not an Identifier). Detect the receiver's
        // LLVM struct type at the seeded `RequestBuilder` shape, stash
        // it in a synthesized alloca, register the synth name in
        // `var_type_names`, then re-dispatch through the identifier
        // path so the existing setter / send arms fire.
        if !matches!(&object.kind, ExprKind::Identifier(_))
            && matches!(method, "header" | "body" | "timeout" | "send")
        {
            let rb_ty = self.type_decls.struct_types.get("RequestBuilder").copied();
            if let Some(rb_ty) = rb_ty {
                let recv_val = self.compile_expr(object)?;
                if let BasicValueEnum::StructValue(sv) = recv_val {
                    if sv.get_type() == rb_ty {
                        let fn_val = self.current_fn.ok_or_else(|| {
                            "RequestBuilder chained method call outside fn".to_string()
                        })?;
                        let synth = format!("__rb_tmp_{}", self.indexed_elem_counter);
                        self.indexed_elem_counter += 1;
                        let slot_ptr = self.create_entry_alloca(fn_val, &synth, rb_ty.into());
                        self.builder.build_store(slot_ptr, sv).unwrap();
                        self.variables.insert(
                            synth.clone(),
                            super::VarSlot {
                                ptr: slot_ptr,
                                ty: rb_ty.into(),
                            },
                        );
                        self.var_types
                            .var_type_names
                            .insert(synth.clone(), "RequestBuilder".to_string());
                        let synth_expr = Expr {
                            kind: ExprKind::Identifier(synth.clone()),
                            span: object.span,
                        };
                        let result = self.compile_method_call(
                            &synth_expr,
                            method,
                            args,
                            call_span,
                            call_span,
                        );
                        self.variables.remove(&synth);
                        self.var_types.var_type_names.remove(&synth);
                        return result;
                    }
                }
            }
        }

        // `std.tracing` builder-chain non-identifier receiver dispatch.
        // `LogEvent.info(msg).with_field(k, v).in_span(id)` and
        // `Span.root(n, id).child(c, id).with_field(k, v)` chain owned-self
        // builders, so each call's receiver is the prior call's return
        // value (a `Call` / `MethodCall` expr, not an Identifier). Same
        // shape as the `RequestBuilder` block above: compile the receiver,
        // match its LLVM struct type against the seeded `Span` / `LogEvent`
        // layouts (`with_field` lives on both, so the type — not the method
        // name — disambiguates), stash it in a synthesized alloca, and
        // re-dispatch through the identifier path so the compiled
        // `Type.method` body fires. Gated on the tracing builder method
        // names so an unrelated non-identifier `.with_field(...)` on a user
        // type whose value isn't a tracing struct falls through untouched.
        if !matches!(&object.kind, ExprKind::Identifier(_))
            && matches!(method, "with_field" | "child" | "in_span")
        {
            let recv_val = self.compile_expr(object)?;
            if let BasicValueEnum::StructValue(sv) = recv_val {
                let sv_ty = sv.get_type();
                let matched = ["LogEvent", "Span"]
                    .into_iter()
                    .find(|name| self.type_decls.struct_types.get(*name) == Some(&sv_ty));
                if let Some(type_name) = matched {
                    let fn_val = self
                        .current_fn
                        .ok_or_else(|| "tracing builder chain outside fn".to_string())?;
                    let synth = format!("__trace_tmp_{}", self.indexed_elem_counter);
                    self.indexed_elem_counter += 1;
                    let slot_ptr = self.create_entry_alloca(fn_val, &synth, sv_ty.into());
                    self.builder.build_store(slot_ptr, sv).unwrap();
                    self.variables.insert(
                        synth.clone(),
                        super::VarSlot {
                            ptr: slot_ptr,
                            ty: sv_ty.into(),
                        },
                    );
                    self.var_types
                        .var_type_names
                        .insert(synth.clone(), type_name.to_string());
                    let synth_expr = Expr {
                        kind: ExprKind::Identifier(synth.clone()),
                        span: object.span,
                    };
                    let result =
                        self.compile_method_call(&synth_expr, method, args, call_span, call_span);
                    self.variables.remove(&synth);
                    self.var_types.var_type_names.remove(&synth);
                    return result;
                }
            }
        }

        // ── Ambient built-in resource methods (BuiltinDefault) ─────
        // Last resort before the dispatch-fail error: lower the ambient
        // resource methods (`env.set`, `clock.now`, ...) the interpreter
        // services via `dispatch_builtin_resource_method_with_values`
        // (`src/interpreter/resource_method.rs`). The receiver is a bare
        // lowercase alias (`env`, `clock`) — see the interpreter's alias
        // table in `src/interpreter/method_call.rs` — that is NOT a bound
        // local; a user variable named `env` shadows the ambient resource,
        // so guard on `self.variables`. User `with_provider` overrides of
        // overridable resources are dispatched earlier via
        // `try_compile_provider_dispatch` (`call_dispatch.rs`), so reaching
        // here means no provider claimed the call.
        if let ExprKind::Identifier(recv) = &object.kind {
            if !self.variables.contains_key(recv) {
                if let Some(resource) = ambient_resource_for_alias(recv) {
                    return self.compile_ambient_resource_method(resource, method, args);
                }
            }
        }

        // Float→int / int→float conversion methods (phase-8 § "Saturating
        // float→int", slice 4 — the codegen for the slice-2 surface). Reaching
        // the fall-through means no impl/user method claimed the call, so a
        // conversion-named method here is the primitive form (a user-defined
        // `to_f32`/`to_f64` on a struct dispatches via the impl-block path above
        // and never reaches here). Semantics match `crate::numeric_conv` (the
        // slice-2 interpreter oracle): `saturating_to_iN` ≡ the `f as iN`
        // saturating cast, `wrapping_to_iN` = modular truncation,
        // `checked_to_iN` → `Option[iN]`, `trunc_to_iN` traps on out-of-range.
        if args.is_empty() {
            if let Some((family, _target, bits, signed)) =
                crate::numeric_conv::parse_float_to_int(method)
            {
                let recv = self.compile_expr(object)?;
                if let BasicValueEnum::FloatValue(fv) = recv {
                    let int_ty = self.int_type_for_bits(bits);
                    return self.emit_float_to_int_conv(fv, family, int_ty, !signed);
                }
            }
            // `i.to_f32()` / `i.to_f64()` — int→float widening (`sitofp`/
            // `uitofp` per the source-integer signedness).
            if method == "to_f32" || method == "to_f64" {
                let src_unsigned = self.expr_is_unsigned_int(object);
                let recv = self.compile_expr(object)?;
                if let BasicValueEnum::IntValue(iv) = recv {
                    let ft = if method == "to_f32" {
                        self.context.f32_type()
                    } else {
                        self.context.f64_type()
                    };
                    let r = if src_unsigned {
                        self.builder.build_unsigned_int_to_float(iv, ft, "to_float")
                    } else {
                        self.builder.build_signed_int_to_float(iv, ft, "to_float")
                    }
                    .unwrap();
                    return Ok(r.into());
                }
            }
        }

        // `<string>.chars().collect()` → materialize a `Vec[char]`. Codegen has
        // no general iterator/`collect` lowering (the chars-iterator value is
        // unsupported, and `collect` on a non-identifier receiver — here the
        // `.chars()` call — falls through to the dispatch-fail error). But the
        // equivalent `for c in <string>.chars() { v.push(c) }` IS fully
        // supported, so lower this idiom to exactly that block and compile it.
        // Surfaced by kata:38 (B-2026-06-18-1). The `.chars()` call is `object`
        // here (the receiver of `collect`); it is reused verbatim as the loop
        // iterable, so no string-receiver shape needs re-synthesizing.
        if method == "collect" && args.is_empty() {
            if let ExprKind::MethodCall {
                method: inner_method,
                args: inner_args,
                ..
            } = &object.kind
            {
                if inner_method == "chars" && inner_args.is_empty() {
                    return self.compile_chars_collect_to_vec(object, call_span);
                }
            }
        }

        // `<map>.values().collect()` / `.keys().collect()` / `.entries().collect()`
        // → the map iterator `values`/`keys`/`entries` already materializes a
        // fresh owned `Vec` eagerly (`compile_map_keys_values_entries`), so
        // `collect()` on it is identity: evaluate the receiver and hand back its
        // Vec (mirrors the identifier-receiver `collect` intercept above, which
        // returns a clone of a materialized-iterator Vec). Without this the
        // non-identifier `collect` receiver — the `.values()` MethodCall — falls
        // through to the dispatch-fail error (B-2026-07-08-17). Surfaced by
        // leetcode/group_anagrams (`groups.values().collect()`).
        if method == "collect" && args.is_empty() {
            if let ExprKind::MethodCall {
                method: inner_method,
                args: inner_args,
                ..
            } = &object.kind
            {
                if matches!(inner_method.as_str(), "values" | "keys" | "entries")
                    && inner_args.is_empty()
                {
                    return self.compile_expr(object);
                }
            }
        }

        // `<iter>.map(f)/.filter(p)....collect()` → materialize a `Vec[U]`
        // (B-2026-07-03-25). Codegen has no lazy iterator value, but a `map` /
        // `filter` adaptor chain terminating in `collect` is equivalent to a
        // `for` loop that pushes each surviving/transformed element onto a fresh
        // `Vec`, and every construct that desugar needs — `for x in <src>`,
        // closure-body inlining via `let <param> = <elem>`, `if <pred> { ... }`,
        // `push`, block move-out — is already fully supported. `object` here is
        // the `collect` receiver: the outermost `map`/`filter` MethodCall.
        // Returns `Ok(None)` (falls through to the diagnostic) for any chain the
        // desugar can't faithfully lower — a non-`map`/`filter` adaptor
        // (`enumerate`, `zip`, …), a non-single-`Binding`-param closure, or a
        // missing output element type — so unsupported shapes fail loudly rather
        // than miscompile.
        // `<recv>.flatten().collect()` (B-2026-07-19-12 slice 3) — a single
        // accumulating loop over the flatten chain (routes through the flatten
        // for-loop desugar). Intercepted before the general adaptor collect
        // engine, which has its own base-peel that doesn't recognize a flatten
        // base. Gated on a bare `flatten()` receiver on the proven whitelist;
        // fails closed to the general engine / loud bail for richer shapes
        // (e.g. `flatten().map(g).collect()`, still `--interp`).
        if method == "collect" && args.is_empty() && Self::for_loop_iterates_flatten(object) {
            if let Some(v) = self.try_compile_flatten_collect(object, call_span)? {
                return Ok(v);
            }
        }

        if method == "collect" && args.is_empty() {
            if let Some(v) = self.try_compile_iter_adaptor_collect_to_vec(object, call_span)? {
                return Ok(v);
            }
        }

        // `<filter_map-chain>.collect()` — the general collect engine above has no
        // `filter_map` in its separate `IterAdaptor` peel, so route it through the
        // (working) fused-chain FOR-LOOP lowering via a `Vec.push` accumulator
        // (B-2026-07-19-14). Gated to a chain that carries a `FilterMap` step, so
        // it never shadows the engine's map/filter path.
        if method == "collect" && args.is_empty() {
            if let Some(v) = self.try_compile_filter_map_collect(object, call_span)? {
                return Ok(v);
            }
        }

        // `<iter-chain>.fold(init, |acc, x| body)` — the sequential `fold`
        // terminal on a fused iterator chain (B-2026-07-11-17). Gated on an
        // iterator-chain receiver (a MethodCall — `Column`/`Tensor.fold` on a
        // variable receiver is intercepted earlier via `try_compile_column_method`
        // and never reaches here). Fails closed to the loud dispatch error below
        // for any chain shape it can't faithfully lower.
        if method == "fold"
            && args.len() == 2
            && matches!(
                &object.kind,
                ExprKind::MethodCall { .. } | ExprKind::Range { .. }
            )
        {
            if let ExprKind::Closure { params, body, .. } = &args[1].value.kind {
                if params.len() == 2 {
                    // B-2026-08-17-24: the ELEMENT param may destructure
                    // (`fold(0, |acc, P { x, y }| acc + x + y)`), which
                    // `closure_param_name` fails closed on. `fold` can wrap its
                    // closure body, so it takes the widened helper and prepends
                    // the pattern's `let`s — the same desugaring the map/filter
                    // peel uses. The ACCUMULATOR param keeps the narrow helper:
                    // it is not an element and never carries a pattern.
                    if let (Some(acc_p), Some((x_p, pat_stmts))) = (
                        Self::closure_param_name(&params[0].pattern, "__fwa"),
                        Self::destructuring_closure_param(&params[1].pattern, "__fwx"),
                    ) {
                        let wrapped;
                        let body: &Expr = if pat_stmts.is_empty() {
                            body
                        } else {
                            wrapped = Expr {
                                kind: ExprKind::Block(crate::ast::Block {
                                    stmts: pat_stmts,
                                    final_expr: Some(Box::new((**body).clone())),
                                    span: body.span,
                                }),
                                span: body.span,
                            };
                            &wrapped
                        };
                        self.register_iter_body_retarget(body);
                        {
                            if let Some(v) = self.try_compile_iter_chain_fold(
                                object,
                                &args[0].value,
                                &acc_p,
                                &x_p,
                                body,
                                call_span,
                            )? {
                                return Ok(v);
                            }
                        }
                    }
                }
            }
        }

        // `<iter-chain>.any(|x| pred)` / `.all(|x| pred)` — short-circuit boolean
        // terminals on a fused iterator chain (B-2026-07-11-19). Same
        // iterator-chain gate as `fold`.
        if (method == "any" || method == "all")
            && args.len() == 1
            && matches!(
                &object.kind,
                ExprKind::MethodCall { .. } | ExprKind::Range { .. }
            )
        {
            if let ExprKind::Closure { params, body, .. } = &args[0].value.kind {
                if params.len() == 1 {
                    if let Some(param) = Self::closure_param_name(&params[0].pattern, "__aap") {
                        self.register_iter_body_retarget(body);
                        {
                            if let Some(v) = self.try_compile_iter_chain_any_all(
                                object,
                                method == "any",
                                &param,
                                body,
                                call_span,
                            )? {
                                return Ok(v);
                            }
                        }
                    }
                }
            }
        }

        // `<iter-chain>.position(|x| pred) -> Option[i64]` — short-circuit index
        // terminal. Same iterator-chain gate as `any`/`all`.
        if method == "position"
            && args.len() == 1
            && matches!(
                &object.kind,
                ExprKind::MethodCall { .. } | ExprKind::Range { .. }
            )
        {
            if let ExprKind::Closure { params, body, .. } = &args[0].value.kind {
                if params.len() == 1 {
                    if let Some(param) = Self::closure_param_name(&params[0].pattern, "__pop") {
                        if let Some(v) =
                            self.try_compile_iter_chain_position(object, &param, body, call_span)?
                        {
                            return Ok(v);
                        }
                    }
                }
            }
        }

        // `<iter-chain>.find(|x| pred) -> Option[T]` — short-circuit element
        // terminal. Same iterator-chain gate; scalar payloads only (a heap
        // element `Some(elem)` would alias the borrowed source buffer — the
        // reduce/max heap deferral applies), else `Ok(None)` -> loud `--interp`.
        if method == "find"
            && args.len() == 1
            && matches!(
                &object.kind,
                ExprKind::MethodCall { .. } | ExprKind::Range { .. }
            )
        {
            if let ExprKind::Closure { params, body, .. } = &args[0].value.kind {
                if params.len() == 1 {
                    if let Some(param) = Self::closure_param_name(&params[0].pattern, "__fip") {
                        if let Some(v) =
                            self.try_compile_iter_chain_find(object, &param, body, call_span)?
                        {
                            return Ok(v);
                        }
                    }
                }
            }
            // Declined (a HEAP element — `Some(elem)` would alias the borrowed
            // source buffer — or an unpeelable chain shape). Bail LOUD with a
            // `--interp` pointer rather than the generic "no handler" dispatch
            // error; the interpreter runs every element type correctly.
            return Err(
                "`Iterator.find()` is lowered under `karac build` only for a SCALAR \
                 element over a fused map/filter chain; a heap element (String / Vec / \
                 struct) or an unsupported chain shape is deferred — run it under the \
                 interpreter (`karac run --interp`, or `KARAC_RUN_JIT=0`)."
                    .to_string(),
            );
        }

        // `<iter-chain>.next() -> Option[T]` — the single-pull FIRST-YIELD read
        // on a chain receiver (`s.chars().next()`, `v.iter().filter(p).next()`;
        // B-2026-07-21-2). A fresh chain expression is its own iterator, so its
        // `next()` is exactly `find(|_| true)` — reuse that proven terminal with
        // a synthesized const-true predicate (same peel, same `Option[T]`
        // accumulator annotation from `iter_terminal_elem_types`, same scalar
        // gate). Stateful multi-pull on a MATERIALIZED binding is intercepted
        // loud at the substitution guard above and never reaches here.
        if method == "next"
            && args.is_empty()
            && matches!(
                &object.kind,
                ExprKind::MethodCall { .. } | ExprKind::Range { .. }
            )
        {
            let true_pred = Expr {
                kind: ExprKind::Bool(true),
                span: *call_span,
            };
            if let Some(v) =
                self.try_compile_iter_chain_find(object, "__nxp", &true_pred, call_span)?
            {
                return Ok(v);
            }
            return Err(
                "`Iterator.next()` on an iterator chain is lowered under `karac build` \
                 only for a SCALAR element (a heap element — String / Vec / struct — or \
                 an unsupported chain shape is deferred); re-run with `--interp` (or \
                 `KARAC_RUN_JIT=0`)."
                    .to_string(),
            );
        }

        // `<iter-chain>.find_map(|x| <Option-expr>) -> Option[U]` — short-circuit
        // map+find terminal: apply the closure to each adapted element and return
        // the first `Some(u)` payload. Same iterator-chain gate as `find`;
        // trivially-copyable payload `U` only (a heap `U` — String / Vec — defers
        // loud to `--interp`, matching `find`'s scalar gate), else `Ok(None)`.
        if method == "find_map"
            && args.len() == 1
            && matches!(
                &object.kind,
                ExprKind::MethodCall { .. } | ExprKind::Range { .. }
            )
        {
            if let ExprKind::Closure { params, body, .. } = &args[0].value.kind {
                if params.len() == 1 {
                    if let Some(param) = Self::closure_param_name(&params[0].pattern, "__fmp") {
                        if let Some(v) =
                            self.try_compile_iter_chain_find_map(object, &param, body, call_span)?
                        {
                            return Ok(v);
                        }
                    }
                }
            }
            return Err(
                "`Iterator.find_map()` is lowered under `karac build` only for a SCALAR \
                 payload over a fused map/filter chain; a heap payload (String / Vec / \
                 struct) or an unsupported chain shape is deferred — run it under the \
                 interpreter (`karac run --interp`, or `KARAC_RUN_JIT=0`)."
                    .to_string(),
            );
        }

        // `<iter-chain>.partition(|x| pred) -> (Vec[T], Vec[T])` — eager terminal
        // splitting the adapted elements into (matches, non-matches). Same
        // iterator-chain gate as `find`; trivially-copyable element `T` only (a
        // heap `T` — String / Vec — would need per-element clones into the target
        // Vecs, deferred loud to `--interp`), else `Ok(None)`.
        if method == "partition"
            && args.len() == 1
            && matches!(
                &object.kind,
                ExprKind::MethodCall { .. } | ExprKind::Range { .. }
            )
        {
            if let ExprKind::Closure { params, body, .. } = &args[0].value.kind {
                if params.len() == 1 {
                    if let Some(param) = Self::closure_param_name(&params[0].pattern, "__ptp") {
                        if let Some(v) =
                            self.try_compile_iter_chain_partition(object, &param, body, call_span)?
                        {
                            return Ok(v);
                        }
                    }
                }
            }
            return Err(
                "`Iterator.partition()` is lowered under `karac build` for a scalar or heap \
                 element over a fused map/filter chain; this chain shape is not yet \
                 lowered — run it under the interpreter (`karac run --interp`, or \
                 `KARAC_RUN_JIT=0`)."
                    .to_string(),
            );
        }

        // `<iter-chain>.last() -> Option[T]` (no args) / `.nth(n) -> Option[T]`
        // (one int arg) — element-returning terminals, scalar payloads only (heap
        // defers loud, like `find`).
        if ((method == "last" && args.is_empty()) || (method == "nth" && args.len() == 1))
            && matches!(
                &object.kind,
                ExprKind::MethodCall { .. } | ExprKind::Range { .. }
            )
        {
            {
                let nth_arg = if method == "nth" {
                    Some(args[0].value.clone())
                } else {
                    None
                };
                if let Some(v) =
                    self.try_compile_iter_chain_last_nth(object, nth_arg.as_ref(), call_span)?
                {
                    return Ok(v);
                }
                // B-2026-08-21-25 — `last` is also a method on the fixed-array
                // read surface, and the gate above admits ANY `MethodCall`
                // receiver on the assumption that it is an iterator chain. So
                // `n.to_ne_bytes().last()` was claimed here and died on this
                // error, never reaching the fixed-array arm at the dispatch
                // tail — the one method of that surface the tail could not see.
                // Retried here rather than by narrowing the gate above: the
                // chain path has already declined, so this runs only where the
                // next statement is an error return, and a receiver it cannot
                // type falls straight back to that same error.
                if method == "last" {
                    if let Some(v) = self.try_compile_nonident_fixed_array_method(
                        object,
                        method,
                        args,
                        call_span,
                        args_close_span,
                    )? {
                        return Ok(v);
                    }
                }
                return Err(format!(
                    "`Iterator.{}()` is lowered under `karac build` only for a SCALAR \
                     element over a fused map/filter chain; a heap element or an \
                     unsupported chain shape is deferred — run it under the interpreter \
                     (`karac run --interp`, or `KARAC_RUN_JIT=0`).",
                    method
                ));
            }
        }

        // `<iter-chain>.sum()` — the numeric-accumulation terminal on a fused
        // iterator chain (B-2026-07-11-19). Same iterator-chain receiver gate as
        // `fold`. Desugars to a `fold(<typed-zero>, |acc, x| acc + x)`, seeding
        // the accumulator with a `(0 as <elem>)` cast so the width matches for
        // every numeric element type. Fails closed to the loud dispatch error
        // below when the element type wasn't recorded or the chain shape isn't
        // one the shared peel understands.
        if (method == "sum" || method == "product")
            && args.is_empty()
            && matches!(
                &object.kind,
                ExprKind::MethodCall { .. } | ExprKind::Range { .. }
            )
        {
            if let Some(v) =
                self.try_compile_iter_chain_sum_product(object, method == "product", call_span)?
            {
                return Ok(v);
            }
        }

        // `<iter-chain>.max()` / `.min()` — comparison terminals
        // (B-2026-07-16-14). Desugars to the existing `reduce` lowering with a
        // synthesized comparison closure — `reduce(|__mma, __mmx| if __mmx >
        // __mma { __mmx } else { __mma })` (Lt for `min`) — so the
        // Option[T]-returning accumulator machinery (seeded None, synthetic
        // Some/None match per element) is reused verbatim. The typechecker
        // recorded the element type against THIS call's span in
        // `iter_terminal_elem_types`, the same key `reduce`'s lowering reads
        // (`MethodCall.span == receiver.span`, and the desugared receiver
        // keeps its span). Scalar elements only — a String element falls to
        // the loud interp-hint bail, mirroring `reduce`'s heap deferral.
        if (method == "max" || method == "min")
            && args.is_empty()
            && matches!(
                &object.kind,
                ExprKind::MethodCall { .. } | ExprKind::Range { .. }
            )
        {
            // Engage for scalar (int / uint / char / bool / float) elements —
            // the reduce lowering now roundtrips float and narrow-int payloads
            // correctly (B-2026-07-17-11 registered the synthesized acc
            // binding's surface type). Non-scalar elements (Strings, which
            // belong to the sorted-collection iter paths below this arm, and
            // any unrecorded shape) FALL THROUGH to the existing dispatch — an
            // early Err here would preempt those later arms
            // (e2e_sorted_set_string_iter_min_max_codegen).
            let elem_head = self
                .span_tables
                .iter_terminal_elem_types
                .get(&(call_span.offset, call_span.length))
                .and_then(|te| match &te.kind {
                    crate::ast::TypeKind::Path(p) => {
                        p.segments.last().map(|s| s.as_str().to_string())
                    }
                    _ => None,
                });
            let elem_is_scalar = matches!(
                elem_head.as_deref(),
                Some(
                    "i8" | "i16"
                        | "i32"
                        | "i64"
                        | "i128"
                        | "u8"
                        | "u16"
                        | "u32"
                        | "u64"
                        | "u128"
                        | "usize"
                        | "isize"
                        | "char"
                        | "bool"
                        | "f32"
                        | "f64"
                        // B-2026-08-11-15: the total-order float wrappers.
                        // Single-field `{ float }` structs — register-sized,
                        // no heap, no RC — so the reduce lowering's
                        // trivially-copyable requirement holds for them, and
                        // their `>`/`<` emit the wrapper total order.
                        | "F32"
                        | "F64"
                        | "F16"
                        | "Bf16"
                )
            );
            if !elem_is_scalar {
                // FALL THROUGH, never Err: a non-scalar element (String —
                // owned by the sorted-collection iter paths below this arm; an
                // early Err regressed e2e_sorted_set_string_iter_min_max_codegen)
                // or an unrecorded element type lands on the existing dispatch,
                // which handles it or emits the generic loud error.
            } else {
                let sp = *call_span;
                let ident = |n: &str| Expr {
                    kind: ExprKind::Identifier(n.to_string()),
                    span: sp,
                };
                let blk = |e: Expr| Block {
                    stmts: Vec::new(),
                    final_expr: Some(Box::new(e)),
                    span: sp,
                };
                let cmp = Expr {
                    kind: ExprKind::Binary {
                        op: if method == "max" {
                            BinOp::Gt
                        } else {
                            BinOp::Lt
                        },
                        left: Box::new(ident("__mmx")),
                        right: Box::new(ident("__mma")),
                    },
                    span: sp,
                };
                let body = Expr {
                    kind: ExprKind::If {
                        condition: Box::new(cmp),
                        then_block: blk(ident("__mmx")),
                        else_branch: Some(Box::new(Expr {
                            kind: ExprKind::Block(blk(ident("__mma"))),
                            span: sp,
                        })),
                    },
                    span: sp,
                };
                if let Some(v) =
                    self.try_compile_iter_chain_reduce(object, "__mma", "__mmx", &body, call_span)?
                {
                    return Ok(v);
                }
                // Unpeelable chain: fall through to the existing dispatch.
            }
        }

        // `<iter-chain>.for_each(|x| body)` — the side-effecting terminal on a
        // fused iterator chain (B-2026-07-11-19). Same iterator-chain gate as
        // `fold`. Desugars to a `for` loop over the peeled base with the closure
        // body as the loop body — so a capture-mutating body
        // (`for_each(|x| total = total + x)`) INLINES and propagates correctly
        // (the same live-outer-access `fold`/`any`/`all` get; it never
        // constructs a closure value, so the stored-mut-ref-closure refusal in
        // `compile_closure` does not apply). Yields unit.
        if method == "for_each"
            && args.len() == 1
            && matches!(
                &object.kind,
                ExprKind::MethodCall { .. } | ExprKind::Range { .. }
            )
        {
            if let ExprKind::Closure { params, body, .. } = &args[0].value.kind {
                if params.len() == 1 {
                    if let Some(param) = Self::closure_param_name(&params[0].pattern, "__fep") {
                        if let Some(v) =
                            self.try_compile_iter_chain_for_each(object, &param, body, call_span)?
                        {
                            return Ok(v);
                        }
                    }
                }
            }
        }

        // `<iter-chain>.reduce(|a, x| ..)` — the `Option[A]`-returning fold
        // terminal (B-2026-07-11-19). For a SCALAR element it desugars to an
        // `Option[A]` accumulator seeded `None`, folded per element via a `match`
        // (`None => Some(x)`, `Some(acc) => Some(body)`) — the type-erased Option
        // layout makes the synthetic `Some(...)` / `None` construction and the
        // tag-dispatched match work without a typecheck pass over the nodes. A
        // HEAP element (String/Vec/struct) falls through to the loud deferral
        // below (its payload rc-accounting in the synthetic match is the
        // remaining piece; the interpreter runs it). Also fails closed when the
        // element type wasn't recorded or the chain shape isn't peelable.
        if method == "reduce"
            && args.len() == 1
            && matches!(
                &object.kind,
                ExprKind::MethodCall { .. } | ExprKind::Range { .. }
            )
        {
            if let ExprKind::Closure { params, body, .. } = &args[0].value.kind {
                if params.len() == 2 {
                    if let (Some(acc_p), Some(x_p)) = (
                        Self::closure_param_name(&params[0].pattern, "__rda"),
                        Self::closure_param_name(&params[1].pattern, "__rdx"),
                    ) {
                        if let Some(v) = self
                            .try_compile_iter_chain_reduce(object, &acc_p, &x_p, body, call_span)?
                        {
                            return Ok(v);
                        }
                    }
                }
            }
            return Err(
                "`Iterator.reduce()` is not yet supported under `karac build` for this shape \
                        (codegen); it works under `karac run --interp` (the tree-walk interpreter). \
                        Re-run with `--interp` (or `KARAC_RUN_JIT=0`), or use `.fold(init, f)` \
                        for a non-optional accumulation."
                    .to_string(),
            );
        }

        // General owned-temp tracking, slice 3b — element-type-aware read
        // methods (`get`/`first`/`last`/`get_unchecked`/`contains`) on a
        // FRESH-TEMP `Vec`/`VecDeque` receiver (`make_vec().get(0)`). Needs the
        // receiver's element type, recorded span-keyed by the typechecker in
        // `temp_recv_elem_types` (unrecoverable from the LLVM `{ptr,len,cap}`
        // shape, which is element-erased). Runs before the String redispatch
        // below; no-ops (returns `Ok(None)`) when there's no recorded element
        // type, so the String path and the diagnostic are untouched.
        if let Some(result) =
            self.try_compile_freshtemp_vec_read_method(object, method, args, call_span)?
        {
            return Ok(result);
        }

        // Slice 3d sibling — read methods (`get`/`contains_key`/`contains`) on a
        // FRESH-TEMP `Map`/`Set` receiver (`make_map().get(k)`). The handle is a
        // plain `ptr` (no struct shape to detect), so it keys off the
        // typechecker's `temp_recv_mapset_types`; no-ops (`Ok(None)`) when absent.
        if let Some(result) =
            self.try_compile_freshtemp_mapset_read_method(object, method, args, call_span)?
        {
            return Ok(result);
        }

        // Last-resort before the dispatch-fail error: a String collection
        // method (`split`, `contains`, …) on a **non-identifier** receiver
        // (`"a,b,c".split(",")`, `make_csv().split(",")`). The collection
        // dispatch above is identifier-keyed (it looks the receiver up by name
        // in `vec_elem_types`), so a literal / call-result receiver falls
        // through. Materialize it into a synthetic local and re-route through
        // `compile_vec_method`.
        if let Some(result) = self.try_compile_nonident_collection_method(
            object,
            method,
            args,
            dispatch_key.as_deref(),
        )? {
            return Ok(result);
        }

        // Slice 3j — a USER impl-block method on a FRESH-TEMP (non-identifier)
        // struct receiver (`make_thing().method()`). The identifier-keyed
        // user-impl dispatch above resolves only Identifier / self receivers
        // (`inferred_receiver_type` reads `var_type_names`), so a call-result
        // receiver falls through here even though `Type.method` exists.
        // Materialize the receiver into a synth local and re-dispatch.
        if let Some(result) = self.try_compile_freshtemp_user_method(
            object,
            method,
            args,
            dispatch_key.as_deref(),
            call_span,
        )? {
            return Ok(result);
        }

        // Generic user impl/trait method on a concrete receiver: route through
        // the same monomorphization pipeline as a generic free fn
        // (B-2026-07-03-15). The declaration pass registered the method as
        // `generic_fns["Type.method"]` with `self` prepended as an ordinary
        // (ref/owned) param 0; prepend the receiver as the first call arg so
        // `compile_generic_call` infers the method's OWN type-params from the
        // arg value types and mangles a per-instantiation mono. `self`'s
        // concrete receiver type contributes no type-param, and its ref/owned
        // ABI is handled by the generic path's arg lowering exactly as for a
        // `ref T` / by-value free-fn param. Runs after every builtin and the
        // non-generic-method arm (`module.get_function("Type.method")` returned
        // None for a generic method), so only genuine generic methods reach
        // here — `generic_fns` holds a `Type.method` key for those alone (free
        // fns are keyed by bare name).
        if let Some(receiver_type) = self.inferred_receiver_type(object) {
            let qualified = format!("{}.{}", receiver_type, method);
            if let Some(generic_fn) = self.mono_state.generic_fns.get(&qualified) {
                let mut all_args: Vec<CallArg> = Vec::with_capacity(args.len() + 1);
                all_args.push(CallArg {
                    label: None,
                    mut_marker: false,
                    mut_marker_span: None,
                    value: object.clone(),
                    span: object.span,
                });
                all_args.extend(args.iter().cloned());

                // Method on a GENERIC struct impl (`impl[T] Box[T]`,
                // B-2026-07-03-23 layer 4): the impl's type params (`T`) are
                // the leading generic axis, but they only appear inside the
                // `self` param's `Box[T]` shape — which `infer_type_args` /
                // `unify_type_expr` do NOT recurse into (they bind bare-`T`
                // params only). So bind them explicitly from the RECEIVER's
                // recorded struct instantiation (`Box[f64]` → `[f64]`).
                //
                // `make_generic_impl_method_function` puts the impl's params
                // FIRST in the merged `generic_params`, and the receiver's args
                // correspond to them positionally, so the receiver's args are a
                // PREFIX of the formal params. `compile_generic_call` zips
                // formals with the explicit list (stopping at the shorter), so
                // passing the receiver's args as a prefix binds the impl-`T`
                // axis and leaves any method-OWN params (`fn pair[U]`) to be
                // inferred from the other args. Gate on
                // `receiver_args <= formal_params` so a spurious over-long list
                // never mis-zips; the impl-`T`-only case (the headline shape)
                // is the equality sub-case.
                //
                // A method with its own generic params on a CONCRETE
                // (non-generic) receiver has no recorded receiver instantiation
                // (`enum_inst_type_of_expr` returns `None` — no generic args),
                // so this yields `None` there and inference runs exactly as
                // before (B-2026-07-03-15).
                let explicit: Option<Vec<GenericArg>> = generic_fn
                    .generic_params
                    .as_ref()
                    .map(|gp| gp.params.len())
                    .and_then(|n_params| {
                        if n_params == 0 {
                            return None;
                        }
                        // B-2026-07-04-16: a handle-backed container receiver
                        // (`Column[T]` / `Tensor[T, S]`) binds the impl's leading
                        // type param from its REGISTERED element type — the
                        // annotation-derived `column_var_infos` / `tensor_var_infos`
                        // entry — NOT from the span-recorded instantiation used
                        // below. The recorded instantiation's element is the
                        // constructor LITERAL's default: an `f32` tensor built as
                        // `Tensor.from([1.0, …])` records `f64` there (the array
                        // literal defaults to `f64`, while the binding's `f32`
                        // annotation drives the actual narrow storage). Binding `T`
                        // from that stale `f64` made `self.sum()` read the `f32`
                        // buffer with an `f64` stride → silent garbage under
                        // `build` for BOTH Column and Tensor. The registered
                        // element is authoritative (it drives the real load
                        // widths), so source `T` from it. The element is always the
                        // container's leading type param, so a single-element
                        // `explicit` prefix binds it and leaves any method-own
                        // params to be inferred from the other args.
                        if let Some(arg) = self.container_receiver_elem_arg(object) {
                            return Some(vec![arg]);
                        }
                        // Recover the receiver's concrete struct instantiation
                        // (`Box[f64]`). Identifier receivers (`b.get()`) and
                        // `self` receivers (a nested `self.hi()` inside another
                        // generic-impl method) resolve through the name-keyed
                        // `enum_inst_var_types` (seeded at the `let` site / the
                        // mono param prologue); struct-literal / fresh-temp
                        // receivers fall back to the span-keyed record.
                        // `enum_inst_type_of_expr` only consults the name table
                        // for `Identifier`, so handle the `self` binding name
                        // explicitly here.
                        let te = match &object.kind {
                            ExprKind::SelfValue => self
                                .type_decls
                                .enum_inst_var_types
                                .get("self")
                                .cloned()
                                .or_else(|| self.enum_inst_type_from_span(object)),
                            _ => self.enum_inst_type_of_expr(object),
                        }?;
                        let TypeKind::Path(p) = &te.kind else {
                            return None;
                        };
                        let args = p.generic_args.as_ref()?;
                        // A `Tensor[T, [3]]` receiver's recorded instantiation
                        // carries a SHAPE arg (`[3]`) alongside the element type
                        // arg. A `Shape` arg is shape-kinded — it never binds a
                        // type/const param, and the mono explicit loop already
                        // skips it — but counting it here would inflate
                        // `args.len()` past the `<= n_params` gate. Count only the
                        // binding (`Type`/`Const`) args, and pass that filtered
                        // list so a shape never mis-zips against a formal type
                        // param. (Reached only for a fresh-temp container receiver
                        // — an identifier / `self` container receiver already
                        // returned above via the registered element.)
                        //
                        // Drop a receiver type arg that is itself a BARE, still-
                        // unsolved impl type param (`H[T]` recorded for a
                        // `Vec[T]`-only struct whose `T` the typechecker could not
                        // solve from field values — `Vec.new()` leaves the element
                        // unconstrained, so the literal freezes as `H[TypeParam(T)]`,
                        // not `H[String]`). Such an arg lowers to the `i64`
                        // unknown-name default and, worse, UNCONDITIONALLY OVERRIDES
                        // (in `compile_generic_call`'s explicit-args loop) the
                        // correct `T` that `infer_type_args` binds from a concrete
                        // method argument — `add(x: T)` with a `String` arg → the
                        // mono mangled `add$i64` and passed a String to an i64 param
                        // (B-2026-07-11-31). Truncate at the first bare param so the
                        // remaining concrete prefix still zips positionally and the
                        // unsolved axis falls to arg inference. A concrete receiver
                        // instantiation (`Box[f64]`, direct-`T` field) has no bare
                        // arg, so it is unaffected.
                        let impl_param_names: Vec<&str> = generic_fn
                            .generic_params
                            .as_ref()
                            .map(|gp| gp.params.iter().map(|p| p.name.as_str()).collect())
                            .unwrap_or_default();
                        let is_bare_param = |a: &GenericArg| -> bool {
                            let GenericArg::Type(te) = a else {
                                return false;
                            };
                            let TypeKind::Path(p) = &te.kind else {
                                return false;
                            };
                            p.generic_args.as_ref().is_none_or(|g| g.is_empty())
                                && p.segments.len() == 1
                                && impl_param_names.iter().any(|&n| n == p.segments[0])
                        };
                        let binding_args: Vec<GenericArg> = args
                            .iter()
                            .filter(|a| !matches!(a, GenericArg::Shape(_)))
                            .take_while(|a| !is_bare_param(a))
                            .cloned()
                            .collect();
                        (binding_args.len() <= n_params && !binding_args.is_empty())
                            .then_some(binding_args)
                    });
                return self.compile_generic_call(
                    &qualified,
                    &all_args,
                    explicit.as_deref(),
                    call_span,
                );
            }
        }

        // A Lazy-typed call reaching this fallthrough should be impossible:
        // the `try_compile_lazy_method` hook lowers every supported method
        // and bails loudly (by name) on the unsupported ones, and the
        // recursive receiver classifier covers chains. A landing here means
        // a receiver shape the classifier can't name AND a stale/aliased
        // span-table entry — keep it loud with the twin's `karac run`
        // pointer rather than the generic fallthrough below.
        let key = (call_span.offset, call_span.length);
        if self
            .span_tables
            .method_callee_types
            .get(&key)
            .is_some_and(|k| {
                k.starts_with("LazyFrame.")
                    || k.starts_with("LazyExpr.")
                    || k.starts_with("LazyGroupBy.")
            })
        {
            return Err(format!(
                "codegen: Lazy method '{method}' fell through dispatch (the codegen twin \
                 lowers the full LazyFrame/LazyExpr/LazyGroupBy surface; a landing here \
                 means a receiver shape the classifier can't name) — this is a codegen \
                 bug in the LazyFrame twin; run the program with `karac run` meanwhile \
                 (tracker: phase-11-stdlib-longtail.md § LazyDataFrame)"
            ));
        }
        // Arrow IPC interchange. All three receivers — Column, DataFrame,
        // Tensor — now have AOT twins (`src/codegen/arrow.rs` → the
        // `karac_arrow_*_to_ipc` entrypoints), each reached from its own
        // receiver-typed dispatcher. A landing HERE therefore means a receiver
        // shape none of those classifiers could name (an unregistered binding,
        // a chained/value receiver), not a deferred leg. Say so, rather than
        // fall through to the generic "this is a codegen bug" message which
        // would send the reader looking for a missing dispatcher arm.
        if method == "to_arrow_ipc" {
            return Err(format!(
                "codegen: `.{method}()` (Arrow IPC interchange) has AOT twins for Column, \
                 DataFrame, and Tensor receivers, but this receiver isn't classified as any \
                 of them — bind it to a variable of the concrete type first, or run with \
                 `karac run` (which routes Arrow IPC programs to the tree-walk interpreter). \
                 If the receiver IS one of the three, this is a codegen bug in the receiver \
                 classifier (tracker: phase-11-stdlib-longtail.md § Arrow IPC)"
            ));
        }
        // B-2026-08-02-10 — a TUPLE-ELEMENT receiver (`t.0.push(x)`,
        // `t.0.len()`): resolve the element's storage via the place-chain
        // machinery, mint a synth identifier over it, and re-dispatch — the
        // B-2026-08-01-35 synth dance. Requires a USABLE element TypeExpr
        // (an annotated tuple binding's full TE; the names registry's
        // empty-path/bare-name synthesis is rejected below), so an
        // unannotated `(Vec.new(), 3)` binding keeps the loud fall-through
        // with an actionable hint instead of mis-registering the synth.
        if let ExprKind::TupleIndex {
            object: tup_obj,
            index,
        } = &object.kind
        {
            let elem_te = self
                .place_chain_tuple_tes(tup_obj)
                .and_then(|tes| tes.get(*index as usize).cloned())
                .filter(|te| match &te.kind {
                    // An empty path is the names registry's None rendering;
                    // a bare container name with no generic args is its
                    // erased synthesis — both unusable as a synth source.
                    TypeKind::Path(p) => match p.segments.as_slice() {
                        [] => false,
                        [only] => {
                            let erased_container =
                                matches!(
                                    only.as_str(),
                                    "Vec" | "Map" | "Set" | "VecDeque" | "SortedMap" | "SortedSet"
                                ) && p.generic_args.as_ref().is_none_or(|g| g.is_empty());
                            !erased_container
                        }
                        _ => true,
                    },
                    _ => true,
                });
            if let (Some(te), Some(elem_ptr), Some(tuple_ty)) = (
                elem_te,
                self.field_chain_place_ptr(object),
                self.place_chain_aggregate_llvm_type(tup_obj),
            ) {
                if let Some(elem_ll) = tuple_ty.get_field_type_at_index(*index as u32) {
                    let synth = format!("__field_elem_{}", self.indexed_elem_counter);
                    self.indexed_elem_counter += 1;
                    self.variables.insert(
                        synth.clone(),
                        super::state::VarSlot {
                            ptr: elem_ptr,
                            ty: elem_ll,
                        },
                    );
                    self.register_var_from_type_expr(&synth, &te);
                    let synth_expr = Expr {
                        kind: ExprKind::Identifier(synth.clone()),
                        span: object.span,
                    };
                    let out = self.compile_method_call(
                        &synth_expr,
                        method,
                        args,
                        call_span,
                        args_close_span,
                    );
                    self.variables.remove(&synth);
                    self.var_types.vec_elem_types.remove(&synth);
                    self.var_types.slice_elem_types.remove(&synth);
                    self.var_types.var_elem_type_exprs.remove(&synth);
                    self.var_types.var_type_names.remove(&synth);
                    self.mapset.map_key_types.remove(&synth);
                    self.mapset.map_val_types.remove(&synth);
                    self.mapset.map_key_type_names.remove(&synth);
                    self.mapset.map_key_type_exprs.remove(&synth);
                    self.mapset.set_elem_types.remove(&synth);
                    self.mapset.set_elem_type_names.remove(&synth);
                    self.mapset.set_elem_type_exprs.remove(&synth);
                    return out;
                }
            }
            return Err(format!(
                "codegen: no handler for method '{method}' on this tuple-element receiver — \
                 annotate the tuple binding with its full element types \
                 (`let t: (Vec[i64], i64) = …`) so the element's type is known here, or \
                 bind the element to a local first (B-2026-08-02-10)"
            ));
        }
        // B-2026-08-14-20 — LAST resort before the dispatch-fail error: a
        // `Slice` builtin method on a NON-IDENTIFIER receiver
        // (`s.bytes().to_vec()`, `v.as_slice().first()`). The whole `Slice`
        // method block is identifier-keyed — it looks the receiver up in
        // `slice_elem_types` by name — so a chained receiver never reached it
        // and every method but the few the String path answers loud-failed
        // under `karac build` while running fine under `--interp`.
        //
        // Materializing is cheap and carries no cleanup obligation: a slice is
        // a `{ptr, len}` VIEW that owns nothing, so unlike the fresh-temp `Vec`
        // sibling above there is no buffer to drop-track — spill the two words
        // to a slot, register the name, re-dispatch, unregister.
        //
        // Placed last on purpose. The syntactic gate (`bytes` / `as_bytes` /
        // `as_slice` / index) can name a receiver that is NOT a slice —
        // `Response.bytes()` returns an owned `Vec[u8]` — so the real
        // discrimination is the compiled value's SHAPE, which costs a
        // `compile_expr`. Running after every other dispatcher has declined
        // means the only path left is the error, so a receiver compiled here
        // and then rejected leaves dead IR in an already-failing compile
        // rather than a duplicated side effect in a working one.
        if let Some(result) = self.try_compile_nonident_slice_method(
            object,
            method,
            args,
            call_span,
            args_close_span,
        )? {
            return Ok(result);
        }

        // B-2026-08-21-25 — the fixed-`Array[T, N]` sibling of the arm above,
        // for the same gap in the same shape: the array dispatch block reads
        // `self.variables[name].ty`, so an array-valued TEMPORARY
        // (`n.to_ne_bytes().len()`, `mk().first()`) had no slot to key on and
        // every method on the surface fell through to the error below while
        // `--interp` answered. Materializes and re-dispatches by identifier;
        // see the helper for why the scalar-element gate is what makes that
        // materialization carry no cleanup obligation.
        if let Some(result) = self.try_compile_nonident_fixed_array_method(
            object,
            method,
            args,
            call_span,
            args_close_span,
        )? {
            return Ok(result);
        }

        // `self.len()` inside an `impl Trait for Map[K, V]` / `Set[T]` body.
        // The main Map/Set dispatch sits inside an `ExprKind::Identifier` arm,
        // so a `SelfValue` receiver never reached it and every builtin method
        // on such a body fell through to the error below — while the identical
        // body over a `Vec[i64]` or `Slice[i64]` head compiled. That asymmetry
        // was not a decision: a Vec/Slice/String receiver compiles to a STRUCT
        // VALUE, which the `len` / `is_empty` / `count` intercept above reads
        // the length field straight out of, and a Map/Set handle is a bare
        // pointer that the same intercept declines. So Vec looked handled and
        // Map/Set looked unimplemented, when neither had a `self` arm at all
        // (B-2026-08-18-12).
        //
        // Placed LAST, immediately before the diagnostic, so it can only fire
        // where compilation was already going to fail: every earlier
        // dispatcher — including the generic user-impl path that gives a
        // user-declared `Map.<method>` priority over the builtin — has already
        // declined. `self` is registered under the name "self" in both
        // registries, exactly as the index and for-loop arms rely on.
        if matches!(&object.kind, ExprKind::SelfValue) {
            if self.mapset.map_key_types.contains_key("self") {
                return self.compile_map_method("self", method, args);
            }
            if self.mapset.set_elem_types.contains_key("self") {
                return self.compile_set_method("self", method, args);
            }
        }

        let receiver_desc = match &object.kind {
            ExprKind::Identifier(name) => format!("variable '{}'", name),
            _ => "non-identifier receiver".to_string(),
        };
        Err(format!(
            "codegen: no handler for method '{}' on {} (method dispatch fell through; \
             this is a codegen bug — add a dispatcher arm in `compile_method_call` \
             or mark the test `#[ignore]` if the method is genuinely deferred)",
            method, receiver_desc
        ))
    }

    /// Extract `(data, len)` of a `Regex` receiver's pattern `String`.
    /// `Regex { pattern: String }` lowers either flattened (field 0 is the data
    /// pointer, field 1 the len) or nested (field 0 is the `{ptr,len,cap}`
    /// String sub-struct); handle both. Shared by every `karac_regex_*` method
    /// arm (B-2026-07-14-19).
    fn regex_pattern_data_len(
        &self,
        recv_sv: inkwell::values::StructValue<'ctx>,
    ) -> (
        inkwell::values::PointerValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
    ) {
        let field0 = self
            .builder
            .build_extract_value(recv_sv, 0, "rx.recv.f0")
            .unwrap();
        if field0.is_struct_value() {
            let ssv = field0.into_struct_value();
            let d = self
                .builder
                .build_extract_value(ssv, 0, "rx.recv.pat.data")
                .unwrap()
                .into_pointer_value();
            let l = self
                .builder
                .build_extract_value(ssv, 1, "rx.recv.pat.len")
                .unwrap()
                .into_int_value();
            (d, l)
        } else {
            let d = field0.into_pointer_value();
            let l = self
                .builder
                .build_extract_value(recv_sv, 1, "rx.recv.pat.len")
                .unwrap()
                .into_int_value();
            (d, l)
        }
    }

    /// Extract `(data, len)` (fields 0 and 1) of a `String` / `Vec`
    /// `{ptr, len, cap}` struct value — the subject / replacement arguments of
    /// the regex method arms (and the LazyExpr col/lit-str lowerings).
    /// Allocate a `count`-element buffer, or hand back `null` when `count` is
    /// 0 (B-2026-08-14-20).
    ///
    /// The empty answer has to be a NULL pointer, not a zero-size block,
    /// because `FreeVecBuffer` — the cleanup every Vec-producing arm arms — is
    /// guarded on `cap > 0`. An empty result carries `cap == 0`, so the guard
    /// skips the free and a zero-size allocation is leaked outright: real
    /// memory (the allocator still returns a live chunk), invisible at -O2
    /// where the whole call folds away, and reported by LeakSanitizer at -O0.
    /// `{null, 0, 0}` is the invariant the empty Vec literal and `Vec.new()`
    /// already emit, so this makes the computed arms agree with the
    /// constructed ones.
    ///
    /// A branch rather than a `select`: both operands of a select are
    /// evaluated, so the allocation would still happen on the empty path.
    fn alloc_buffer_or_null(
        &mut self,
        count: inkwell::values::IntValue<'ctx>,
        alloc_bytes: inkwell::values::IntValue<'ctx>,
        tag: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        let fn_val = self
            .current_fn
            .ok_or_else(|| format!("codegen: {tag} buffer allocation outside a function"))?;
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let zero = self.context.i64_type().const_zero();
        let pos = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SGT,
                count,
                zero,
                &format!("{tag}.ab.pos"),
            )
            .unwrap();
        let alloc_bb = self
            .context
            .append_basic_block(fn_val, &format!("{tag}.ab.alloc"));
        let none_bb = self
            .context
            .append_basic_block(fn_val, &format!("{tag}.ab.none"));
        let join_bb = self
            .context
            .append_basic_block(fn_val, &format!("{tag}.ab.join"));
        self.builder
            .build_conditional_branch(pos, alloc_bb, none_bb)
            .unwrap();
        self.builder.position_at_end(alloc_bb);
        let allocated = self
            .builder
            .build_call(
                self.runtime_fns.alloc_or_panic_fn,
                &[alloc_bytes.into()],
                &format!("{tag}.buf"),
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let alloc_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(join_bb).unwrap();
        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(join_bb).unwrap();
        self.builder.position_at_end(join_bb);
        let phi = self
            .builder
            .build_phi(ptr_ty, &format!("{tag}.ab.buf"))
            .unwrap();
        phi.add_incoming(&[(&allocated, alloc_end), (&ptr_ty.const_null(), none_bb)]);
        Ok(phi.as_basic_value().into_pointer_value())
    }

    /// Load a slice header's `{ptr, len}` pair. Shared by the arms that
    /// implement a `Slice` method directly rather than routing it to `Vec`
    /// (B-2026-08-14-9) — each needs exactly these two loads and nothing else
    /// from the header.
    fn slice_data_and_len(
        &mut self,
        slice_ty: inkwell::types::StructType<'ctx>,
        slice_ptr: inkwell::values::PointerValue<'ctx>,
        tag: &str,
    ) -> (
        inkwell::values::PointerValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
    ) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let dp = self
            .builder
            .build_struct_gep(slice_ty, slice_ptr, 0, &format!("{tag}.data.p"))
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, dp, &format!("{tag}.data"))
            .unwrap()
            .into_pointer_value();
        let lp = self
            .builder
            .build_struct_gep(slice_ty, slice_ptr, 1, &format!("{tag}.len.p"))
            .unwrap();
        let len = self
            .builder
            .build_load(i64_t, lp, &format!("{tag}.len"))
            .unwrap()
            .into_int_value();
        (data, len)
    }

    pub(super) fn str_data_len(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
    ) -> (
        inkwell::values::PointerValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
    ) {
        let d = self
            .builder
            .build_extract_value(sv, 0, "rx.str.data")
            .unwrap()
            .into_pointer_value();
        let l = self
            .builder
            .build_extract_value(sv, 1, "rx.str.len")
            .unwrap()
            .into_int_value();
        (d, l)
    }

    /// Build an owned `String` holding a fresh heap copy of the subject byte
    /// range `s_data[start..end]` — the `text` field of a regex `Match`. The
    /// copy (never an alias into the subject buffer, which drops at the call's
    /// statement end) is what keeps `find` / `find_all` memory-clean.
    fn build_regex_match_text(
        &mut self,
        s_data: inkwell::values::PointerValue<'ctx>,
        start: inkwell::values::IntValue<'ctx>,
        end: inkwell::values::IntValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let i8_t = self.context.i8_type();
        let text_ptr = unsafe {
            self.builder
                .build_gep(i8_t, s_data, &[start], "rx.match.text.ptr")
                .unwrap()
        };
        let text_len = self
            .builder
            .build_int_sub(end, start, "rx.match.text.len")
            .unwrap();
        self.build_owned_string_from_parts(text_ptr, text_len)
    }

    /// Assemble a `Match { text, start, end }` struct value from SSA fields.
    /// `Match` lowers to the nested `{ {i8*,i64,i64}, i64, i64 }` aggregate
    /// (String kept as field 0's sub-struct), so `text` inserts whole; the
    /// layout is registered because regex.kara is in `compiled_stdlib_programs`.
    fn build_match_struct(
        &mut self,
        text: BasicValueEnum<'ctx>,
        start: inkwell::values::IntValue<'ctx>,
        end: inkwell::values::IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let match_ty = self
            .type_decls
            .struct_types
            .get("Match")
            .copied()
            .ok_or_else(|| {
                "codegen: Regex.find needs the `Match` struct layout \
             (regex.kara not registered in compiled_stdlib_programs)"
                    .to_string()
            })?;
        let mut m = match_ty.get_undef();
        m = self
            .builder
            .build_insert_value(m, text, 0, "rx.match.text")
            .unwrap()
            .into_struct_value();
        m = self
            .builder
            .build_insert_value(m, start, 1, "rx.match.start")
            .unwrap()
            .into_struct_value();
        m = self
            .builder
            .build_insert_value(m, end, 2, "rx.match.end")
            .unwrap()
            .into_struct_value();
        Ok(m.into())
    }

    /// For a handle-backed container receiver (`Column[T]` / `Tensor[T, S]`)
    /// bound to a variable / `self`, build the `GenericArg` that binds the
    /// impl's leading type param from the container's REGISTERED element type
    /// (`column_var_infos` / `tensor_var_infos`, seeded from the binding's
    /// annotation). This is the authoritative element — it drives the actual
    /// load/store widths — unlike the span-recorded instantiation, whose
    /// element is the constructor literal's default (`f64` for a narrow-`f32`
    /// tensor). Used by the generic user-impl-method dispatch to bind `T`
    /// (B-2026-07-04-16). Returns `None` for a non-container receiver or one
    /// whose element isn't a primitive we can name.
    fn container_receiver_elem_arg(&self, object: &Expr) -> Option<GenericArg> {
        let name = match &object.kind {
            ExprKind::Identifier(n) => n.as_str(),
            ExprKind::SelfValue => "self",
            _ => return None,
        };
        let (elem, unsigned) = if let Some(ti) = self.accel.tensor_var_infos.get(name) {
            (ti.elem, ti.elem_unsigned)
        } else {
            let ci = self.accel.column_var_infos.get(name)?;
            (ci.elem, ci.elem_unsigned)
        };
        let prim = self.primitive_type_name_for_llvm(elem, unsigned)?;
        Some(GenericArg::Type(TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![prim],
                generic_args: None,
                span: object.span,
            }),
            span: object.span,
        }))
    }

    /// Kāra primitive type name for a numeric LLVM element type — `f32` / `f64`
    /// / `i8`…`i64` / `u8`…`u64` / `bool`. `unsigned` disambiguates the int
    /// width's signedness (the `IntType` alone can't). `None` for a
    /// non-primitive LLVM type (e.g. an aggregate element). Companion to
    /// [`Self::container_receiver_elem_arg`].
    fn primitive_type_name_for_llvm(
        &self,
        ty: BasicTypeEnum<'ctx>,
        unsigned: bool,
    ) -> Option<String> {
        if ty == self.context.f32_type().into() {
            return Some("f32".to_string());
        }
        if ty == self.context.f64_type().into() {
            return Some("f64".to_string());
        }
        if let BasicTypeEnum::IntType(it) = ty {
            let w = it.get_bit_width();
            if w == 1 {
                return Some("bool".to_string());
            }
            let base = if unsigned { 'u' } else { 'i' };
            return Some(format!("{base}{w}"));
        }
        None
    }

    /// Materialize a **non-identifier String** method receiver into a synthetic
    /// local, then route through the identifier-keyed collection dispatch
    /// (`compile_vec_method`). Closes the Weave non-identifier-receiver gap for
    /// String collection methods like `"a,b,c".split(",")` /
    /// `make_csv().split(",")` — the receiver-shape-keyed dispatch in
    /// `compile_method_call` only fires for `Identifier` receivers, so a literal
    /// or call-result String receiver fell through to "no handler". (The
    /// call-result `.to_string()` case already works via the receiver-shape-
    /// agnostic `String.to_string` arm.) Returns `Ok(None)` when the receiver
    /// isn't a String — the caller falls through to its diagnostic, so this is a
    /// pure addition that can't change existing cases.
    ///
    /// Scoped to String deliberately: the receiver type is resolved from the
    /// `Type.method` callee key's receiver segment (span-independent — robust),
    /// and String needs no element type. A non-identifier **Vec** receiver
    /// (`make_vec().contains(x)`) additionally needs the element type, which is
    /// only available span-keyed in `owned_temp_drops` — and a `Call` receiver's
    /// `object.span` is the callee-name span, not the call-expr span those
    /// tables use, so it doesn't resolve. That's a separate follow-on (tracked
    /// in phase-7-codegen.md "non-identifier receiver"); it errors loudly today
    /// exactly as before, no regression.
    ///
    /// Drop: the receiver temp's free is owned by the existing statement-level
    /// owned-temp machinery (the RHS sub-expression's `owned_temp_drops` entry
    /// queues it), so the synth slot is NOT separately drop-tracked — tracking
    /// it too double-frees a heap receiver like `make_csv().split(",")` (proven:
    /// a tracked variant SIGABRT'd at scope exit; the untracked one is leak- and
    /// double-free-clean under `leaks` + ASAN). IR parity with the one-line
    /// `let s = <recv>; s.split(",")` workaround.
    /// A method call whose RECEIVER is a borrow-returning user accessor:
    /// `h.view().is_empty()` where `fn view(ref self) -> ref Vec[i64]`.
    ///
    /// The receiver classifier is identifier-keyed (it looks the receiver up by
    /// name in `vec_elem_types` / `var_type_names`), so a call-result receiver
    /// had no name to look up and the call died in the dispatcher's catch-all
    /// with "no handler for method '…' on non-identifier receiver"
    /// (B-2026-07-29-12). Binding the receiver first — `let v = h.view();
    /// v.is_empty()` — always worked, which is the whole shape of the bug: the
    /// `let` arm in `stmts.rs` already knows how to register a borrow-return as
    /// a usable local. This does the same registration against a SYNTHETIC name
    /// and re-enters dispatch, so the two spellings lower identically.
    ///
    /// The borrow is bound, not copied: a `-> ref T` method returns the
    /// pointee's address, so the synthetic slot holds that pointer and every
    /// read goes through it. Nothing is drop-tracked — `vec_elem_types` and
    /// friends are type registries, not drop lists, and the owner still frees
    /// the storage. `ref T` is an immutable borrow, so the typechecker has
    /// already rejected any mutating method here.
    ///
    /// Returns `Ok(None)` for any receiver that is not a borrow-returning user
    /// call, so this is a pure addition ahead of the existing fall-throughs.
    fn try_compile_ref_return_receiver_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Prefer the span-keyed record (it is what the `let` arm uses), then
        // fall back to the declaration-derived one by method name. The fallback
        // is what actually fires for a chain: `h.view().is_empty()` puts both
        // calls at one span and the outer `bool` result evicts the inner
        // `ref Vec[i64]` from the span table.
        let inner_te = match self.ref_return_inner_for_call_pub(object) {
            Some(te) => te,
            None => {
                let ExprKind::MethodCall {
                    method: recv_method,
                    ..
                } = &object.kind
                else {
                    return Ok(None);
                };
                match self.user_ref_method_inner.get(recv_method) {
                    Some(te) => te.clone(),
                    None => return Ok(None),
                }
            }
        };
        // The `ref String` inner goes through the same materialization as the
        // Vec one. It was refused for a while on leak evidence — one extra
        // leaked block per chained call against the bound-receiver spelling —
        // but that measurement was reading the DOUBLE-CALL defect fixed in
        // B-2026-07-29-15, not a property of the String leg: the helper used
        // to sit after an arm that had already emitted the receiver, so the
        // accessor ran twice and the first result was dropped on the floor.
        // With the helper hoisted, the leak count is per-accessor-invocation
        // and IDENTICAL between the two spellings (1 call → 1 block, 3 calls
        // → 3 blocks, chained or bound). That residual is a pre-existing leak
        // in `-> ref String` accessors themselves (B-2026-07-29-21), which
        // refusing here would not avoid — the bound form leaks it too.
        let fn_val = self
            .current_fn
            .ok_or_else(|| "ref-return receiver materialization outside fn".to_string())?;

        // Emit the accessor with the bind-directly gate bypassed — this IS the
        // sanctioned binding site, just an anonymous one.
        let prev = self.compiling_ref_return_let_rhs;
        self.compiling_ref_return_let_rhs = true;
        let ptr_res = self.compile_expr(object);
        self.compiling_ref_return_let_rhs = prev;
        let ptr_val = ptr_res?;

        let synth = format!("__refrecv_tmp_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let alloca = self.create_entry_alloca(fn_val, &synth, ptr_ty.into());
        self.builder.build_store(alloca, ptr_val).unwrap();
        self.variables.insert(
            synth.clone(),
            super::VarSlot {
                ptr: alloca,
                ty: ptr_ty.into(),
            },
        );

        // Same registration ladder as the `let` arm's borrow-return binding
        // (`compile_let`, `ref_return_inner_for_call`), in the same order: a
        // `ref Tensor` rides the by-value ref ABI and binds as a borrowed
        // tensor var; everything else binds as a deref-on-use ref-local plus
        // the Vec/String element registry that value-receiver dispatch needs.
        let mut is_tensor = false;
        if let Some(info) = self.tensor_var_info_from_type_expr(&inner_te) {
            self.accel.tensor_var_infos.insert(synth.clone(), info);
            is_tensor = true;
        } else {
            let inner_llvm = self.llvm_type_for_type_expr(&inner_te);
            self.borrow_vars
                .ref_params
                .insert(synth.clone(), inner_llvm);
            if let TypeKind::Path(p) = &inner_te.kind {
                if let Some(seg) = p.segments.first() {
                    self.var_types
                        .var_type_names
                        .insert(synth.clone(), seg.clone());
                }
            }
            if let Some(elem_ty) = self.extract_vec_elem_type(&inner_te) {
                self.var_types.vec_elem_types.insert(synth.clone(), elem_ty);
                if let Some(inner) = super::helpers::vec_inner_type_expr(&inner_te) {
                    self.var_types
                        .var_elem_type_exprs
                        .insert(synth.clone(), inner);
                }
            } else if self.is_string_type_expr(&inner_te) {
                self.var_types
                    .vec_elem_types
                    .insert(synth.clone(), self.context.i8_type().into());
                self.var_types.string_vars.insert(synth.clone());
            }
        }

        let synth_recv = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: object.span,
        };
        // Synthetic caller: pass `call_span` for the paren span too, per
        // `compile_method_call`'s contract (`method_call_key` then falls back
        // to the receiver span, preserving prior behavior).
        let result =
            self.compile_method_call(&synth_recv, method, args, &object.span, &object.span);

        // Dispatch-only registrations; the name is unique per call site.
        self.variables.remove(&synth);
        self.var_types.var_type_names.remove(&synth);
        self.var_types.vec_elem_types.remove(&synth);
        self.var_types.var_elem_type_exprs.remove(&synth);
        self.var_types.string_vars.remove(&synth);
        self.borrow_vars.ref_params.remove(&synth);
        if is_tensor {
            self.accel.tensor_var_infos.remove(&synth);
        }
        result.map(Some)
    }

    /// `(self mode, returns-a-borrow)` for the user impl method
    /// `type_name.method`, resolved from the program snapshot. `None` when no
    /// impl block whose target head is `type_name` declares `method` with a
    /// receiver (associated fns and generic impls resolved through other
    /// channels stay `None` — the caller treats that as not body-eligible,
    /// the conservative silent direction). B-2026-08-01-5.
    pub(super) fn impl_method_self_and_borrow_return(
        &self,
        type_name: &str,
        method: &str,
    ) -> Option<(crate::ast::SelfParam, bool)> {
        let program = self.program_snapshot.as_deref()?;
        for item in &program.items {
            let crate::ast::Item::ImplBlock(imp) = item else {
                continue;
            };
            let target_ok = matches!(&imp.target_type.kind, crate::ast::TypeKind::Path(p)
                if p.segments.last().is_some_and(|s| s == type_name));
            if !target_ok {
                continue;
            }
            for ii in &imp.items {
                let crate::ast::ImplItem::Method(f) = ii else {
                    continue;
                };
                if f.name == method {
                    let sp = f.self_param.clone()?;
                    let borrow_ret = matches!(
                        f.return_type.as_ref().map(|t| &t.kind),
                        Some(crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_))
                    );
                    return Some((sp, borrow_ret));
                }
            }
        }
        None
    }

    fn try_compile_nonident_collection_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        dispatch_key: Option<&str>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Identifier / self receivers already route through the main dispatch.
        if matches!(&object.kind, ExprKind::Identifier(_) | ExprKind::SelfValue) {
            return Ok(None);
        }
        let span_key = (object.span.offset, object.span.length);

        // Receiver must be a String. Prefer the `Type.method` callee key's
        // receiver segment (span-independent); fall back to the
        // String-typed-expr span set.
        let recv_is_string = dispatch_key
            .and_then(|k| k.rsplit_once('.'))
            .map(|(t, _)| t == "String")
            .unwrap_or(false)
            || self.span_tables.string_typed_exprs.contains(&span_key)
            // Third signal, and the one that carries a CHAIN. Both of the
            // above are span-keyed, and the parser gives a MethodCall its
            // RECEIVER's span — so in `(…).to_uppercase().len()` the inner
            // and outer calls share one key and the outer wins both tables:
            // `method_callee_types` resolves to `String.len` (which the
            // `m == method` filter then rejects, yielding `None`) and
            // `expr_types` records the outer `i64`, evicting the inner
            // `String`. Measured, not inferred: the same receiver reports
            // `key=Some("String.to_uppercase") is_str=true` standalone and
            // `key=None is_str=false` under a chain, at the identical span.
            // That is B-2026-08-05-28 — not the missing dispatcher arm the
            // row diagnosed (the arm is right below, and it is why the
            // STANDALONE form has always worked).
            //
            // The method name is span-free evidence. Restricted to methods
            // that exist ONLY on String, so a Vec receiver can never take
            // this path: `sorted`, `contains`, `len` and friends are shared
            // and are deliberately absent. A non-String receiver here would
            // already have failed the typechecker, which is what makes the
            // name sufficient.
            //
            // One exception since B-2026-08-12-25: `to_uppercase`/`to_lowercase`
            // now also name the char→char folds, so a char receiver is excluded
            // explicitly. `compile_method_call`'s char arm runs long before this
            // fallback, so this is the belt to that braces — but the premise
            // above ("only on String") is no longer true for those two names,
            // and a signal keyed on an untrue premise is how the next receiver
            // type gets misrouted into the String path.
            || (matches!(
                method,
                "to_uppercase"
                    | "to_lowercase"
                    | "trim"
                    | "trim_start"
                    | "trim_end"
                    | "starts_with"
                    | "ends_with"
                    | "replace"
                    | "replacen"
                    | "repeat"
                    | "split"
                    | "split_whitespace"
                    | "lines"
                    // `normalize` is String-only and always will be — the
                    // premise this list rests on (B-2026-08-20-41).
                    | "normalize"
            ) && !self.expr_is_char(object));
        if !recv_is_string {
            return Ok(None);
        }

        let cur_fn = self
            .current_fn
            .ok_or_else(|| "method receiver materialization outside fn".to_string())?;
        let val = self.compile_expr(object)?;

        // Store the receiver value into a synthetic slot for dispatch. NOT
        // drop-tracked — see the doc comment (the statement-level owned-temp
        // machinery owns the free; double-tracking double-frees).
        let slot = self.create_entry_alloca(cur_fn, "__recv_tmp", val.get_type());
        self.builder.build_store(slot, val).unwrap();

        let synth = format!("__recv_tmp_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        let i8t = self.context.i8_type().into();
        self.variables.insert(
            synth.clone(),
            super::VarSlot {
                ptr: slot,
                ty: val.get_type(),
            },
        );
        self.var_types.vec_elem_types.insert(synth.clone(), i8t);
        self.var_types.string_vars.insert(synth.clone());
        self.var_types
            .var_type_names
            .insert(synth.clone(), "String".to_string());

        let result = self.compile_vec_method(&synth, slot, method, args);

        // Drop the dispatch-only registrations (unique synth name).
        self.variables.remove(&synth);
        self.var_types.var_type_names.remove(&synth);
        self.var_types.vec_elem_types.remove(&synth);
        self.var_types.string_vars.remove(&synth);

        // Free the intermediate receiver temp's buffer. When `object` is a
        // fresh owned String — a chained `s.to_uppercase().to_lowercase()`
        // receiver, an `f"…".trim()`, a `make_str().to_uppercase()` — nothing
        // else owns its `{ptr,len,cap}` buffer: the statement-level owned-temp
        // machinery tracks only the OUTERMOST temp, so the intermediate leaks
        // once per call (unbounded in a loop). Free it here, but ONLY when the
        // method's result cannot alias the receiver buffer — a scalar / bool /
        // Option-of-scalar result never does, and the freshly-ALLOCATING
        // String→String family returns an independent copy. A heap-struct
        // result from a BORROWING method (`split`, slicing) may view into the
        // receiver, so those stay conservatively un-freed (a safe leak, never a
        // dangle). `free_str_vec_buffer_if_heap`'s `cap > 0` guard no-ops on a
        // borrowed (cap == 0) view. B-2026-07-16-21.
        if let Ok(rv) = &result {
            // B-2026-08-05-27 — the surface-concat receiver, admitted here for
            // the same reason it was admitted on the len-family gate and on the
            // argument side before that: a concat the `String.add` desugar
            // skipped stays an `ExprKind::Binary`, which
            // `expr_yields_fresh_owned_temp` matches Call/MethodCall only and so
            // declines. `("p:".to_string() + s).starts_with(…)` therefore freed
            // nothing and leaked the concat once per evaluation — 8200 B over
            // 200 iterations on a DEFAULT -O2 build, since `starts_with` reads
            // the BYTES and the buffer is not dead.
            //
            // The Call-receiver twin (`mk().starts_with(…)`) was already clean,
            // which is what localized this to the predicate rather than to the
            // free itself: the machinery below is correct, it was just never
            // reached for a Binary receiver. The result-aliasing analysis that
            // follows is unchanged and still decides whether the free is safe —
            // `starts_with` returns a bool, so it is receiver-independent.
            if self.expr_yields_fresh_owned_temp(object)
                || self.expr_is_fresh_owned_string_slice(object)
                || self.expr_is_fresh_owned_string_concat(object, val.get_type())
            {
                let result_is_heap_struct = self.llvm_ty_is_vec_struct(rv.get_type());
                // A heap-struct result is receiver-independent only for methods
                // that ALLOCATE a fresh copy: the String→String xform family
                // and the split family, whose runtime helpers
                // (`karac_runtime_string_split{,_whitespace}` / `_lines`)
                // `copy_nonoverlapping` each piece into its OWN `cap>0` buffer —
                // no element views into the receiver. Any other heap-struct
                // result is treated as possibly-aliasing and left un-freed.
                let result_independent_of_receiver = !result_is_heap_struct
                    || matches!(
                        method,
                        "trim"
                            | "trim_start"
                            | "trim_end"
                            | "to_lowercase"
                            | "to_uppercase"
                            | "sorted"
                            | "replace"
                            | "replacen"
                            | "to_string"
                            | "repeat"
                            | "split"
                            | "split_whitespace"
                            | "lines"
                    );
                if result_independent_of_receiver {
                    self.free_str_vec_buffer_if_heap(val);
                }
            }
        }

        result.map(Some)
    }

    /// Slice 3j — a USER impl-block method on a FRESH-TEMP (non-identifier)
    /// receiver whose type is a non-shared user struct (`make_thing().method()`,
    /// `build().total()`). The identifier-keyed user-impl dispatch
    /// (`inferred_receiver_type` → `Type.method`) resolves only Identifier / self
    /// receivers, so a call-result receiver falls through to the dispatch-fail
    /// error even though `Type.method` exists — a silent hard error, not a
    /// miscompile. Recover the struct type from the typechecker's `Type.method`
    /// callee key, materialize the receiver value into a synth local, register it
    /// under that struct name (so the recursion's `inferred_receiver_type`
    /// resolves and `get_data_ptr` yields the ptr-self ABI address), drop-track it
    /// **iff `self` is borrowed** (`ref self` / `mut ref self` — the caller owns
    /// the temp; owned `self` moves it into the method, which drops its fields, so
    /// tracking the caller's shallow copy too would double-free the shared heap
    /// buffers), then re-dispatch through the identifier path by recursing into
    /// `compile_method_call` with a synth Identifier receiver (which hits the
    /// user-impl arm *before* reaching this helper again — no infinite recursion).
    ///
    /// Returns `Ok(None)` when the receiver isn't a serviceable fresh-temp user
    /// struct (no callee key, not a known struct, shared, or `Type.method`
    /// absent), so the caller falls through to its own diagnostic — a pure
    /// addition that can't change any existing case. Enum / shared-struct
    /// receivers (heap-pointer self, RC drop) are follow-ons.
    fn try_compile_freshtemp_user_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        dispatch_key: Option<&str>,
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Identifier / self receivers already route through the main user-impl
        // dispatch.
        if matches!(&object.kind, ExprKind::Identifier(_) | ExprKind::SelfValue) {
            return Ok(None);
        }
        // Recover the struct type from the `Type.method` callee key. When the
        // key is unavailable — the chained-call span collision leaves only ONE
        // `method_callee_types` entry per chain, so in a MIXED-NAME chain
        // (`Command.new("ls").arg("-l").stdout(cfg)`) every link except the
        // surviving one sees a mismatched key filtered to `None` by the caller
        // — fall back to the static receiver-type walk (`type_name_of_expr`,
        // which resolves chains recursively via `fn_return_type_names`). The
        // `qualified`-function-existence check below remains the real "is this
        // a user method" gate, so the wider recovery cannot hijack builtin
        // (never-declared) methods.
        let type_name = match dispatch_key
            .and_then(|k| k.rsplit_once('.'))
            .map(|(t, _)| t.to_string())
        {
            Some(t) => t,
            None => match self.type_name_of_expr(object) {
                Some(t) => t,
                None => return Ok(None),
            },
        };
        // Accept any user type that carries `impl`-block methods: a non-shared
        // struct, a value enum, or a shared struct/enum (RC). The three differ
        // only in the scope-exit DROP they need for the materialized temp (see
        // the drop-track block below); the DISPATCH is uniform — store the
        // receiver into a synth local and re-enter `compile_method_call` with an
        // Identifier, which resolves the same for all three. The `qualified`
        // function-existence check below is the real "is this a method" gate.
        let is_shared = self.type_decls.shared_types.contains_key(&type_name);
        let is_value_enum = !is_shared && self.type_decls.enum_layouts.contains_key(&type_name);
        let is_plain_struct = !is_shared && self.type_decls.struct_types.contains_key(&type_name);
        if !(is_shared || is_value_enum || is_plain_struct) {
            return Ok(None);
        }
        let qualified = format!("{type_name}.{method}");
        // Accept a concrete `Type.method` (declared) OR a GENERIC impl method
        // registered in `generic_fns` (B-2026-07-03-15): materialize the
        // fresh-temp receiver into a synth local and re-enter, which routes the
        // now-Identifier receiver through the generic-method mono arm.
        if self.module.get_function(&qualified).is_none()
            && !self.mono_state.generic_fns.contains_key(&qualified)
        {
            return Ok(None);
        }
        let cur_fn = self
            .current_fn
            .ok_or_else(|| "user-method receiver materialization outside fn".to_string())?;
        let val = self.compile_expr(object)?;
        let slot = self.create_entry_alloca(cur_fn, "__urecv_tmp", val.get_type());
        self.builder.build_store(slot, val).unwrap();

        let synth = format!("__urecv_tmp_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            super::VarSlot {
                ptr: slot,
                ty: val.get_type(),
            },
        );
        self.var_types
            .var_type_names
            .insert(synth.clone(), type_name.clone());
        // B-2026-08-06-12 — the synth local needs the receiver's INSTANTIATION
        // too, not just its base name. Re-entering with an Identifier routes
        // through the generic-method mono arm, and that arm reads
        // `enum_inst_var_types` to pick the monomorph; with only
        // `var_type_names` seeded it sees a bare `Box` and emits ONE unmangled
        // `@Box.take({ i64 })` at the erased base layout for every
        // instantiation. `let b = Box { .. }; b.take()` works purely because
        // the let-site seeds this same record (stmts.rs).
        //
        // Both halves of the bug key off this one record — the literal's own
        // layout (`struct_inst_mono_type_for_expr`) and the callee selection —
        // which is why fixing either alone just moves the verifier error:
        // a correctly-shaped `{ { ptr, i64, i64 } }` receiver handed to a
        // callee still declared `{ i64 }`.
        //
        // Span-keyed lookup first (correct wherever the span is uncontested),
        // then the field-expression recovery for the receiver-position literal
        // whose span the MethodCall clobbers.
        // B-2026-08-06-22 — resolve the receiver expression's OWN instantiation
        // before consulting its span. A `MethodCall` inherits its RECEIVER's
        // span from the parser, so for a chained `outer.take()` the span-keyed
        // lookup finds `outer`'s record and answers `Box[Box[Wide]]` — the type
        // being called ON, not the type the call RETURNS. It passes the generic
        // filter, so the synth local was seeded one level too high and the next
        // link in the chain selected its monomorph at the wrong instantiation
        // (emitting a `take` over `{ i64 }` for `Box[Wide]`). Same span
        // collision B-2026-08-06-19 fixed inside `enum_inst_type_of_expr`,
        // which is exactly why calling THAT here is the fix: it tries the
        // call's own return instantiation first and falls back to the span.
        if let Some(inst) = self
            .enum_inst_type_of_expr(object)
            .filter(|te| {
                self.is_generic_named_struct_type_expr(te)
                    || self.is_generic_named_enum_type_expr(te)
            })
            .or_else(|| self.struct_literal_inst_from_fields(object))
        {
            self.type_decls
                .enum_inst_var_types
                .insert(synth.clone(), inst);
        }

        // Drop-track the materialized temp (for a fresh-owned receiver),
        // mirroring the `let`-binding path in `stmts.rs`. MEMORY tracking is
        // unconditional for the caller-owned temp (proven by LSan: the
        // owned-`self` struct case leaked the field `Vec` once per call
        // without it — the user-impl dispatch passes the receiver by shallow
        // value copy and emits no receiver drop). BODY tracking is
        // self-mode-gated since B-2026-08-01-5: only a `ref self` /
        // `mut ref self` method that does NOT return a borrow registers the
        // user-Drop body, under the drain-eligible `__urecv_drop_tmp` name so
        // it fires at STATEMENT END — where the interpreter's receiver hook
        // fires it. An owned-`self` method CONSUMED the value (the old
        // unconditional wrapper registration double-fired passthrough chains
        // — `mk(3).me().ident()` printed the body twice at scope exit and
        // `mk(1).plus(10)` fired over a stale slot), and a borrow-returning
        // method's result outlives the statement (a statement-end wrapper
        // free would dangle it) — both stay body-silent on both backends,
        // memory-only here. The drop machinery by kind:
        //   • shared struct / enum (or `par`): one scope-exit `RcDec` on the
        //     box — `track_rc_var` with the heap type from `shared_types`.
        //   • value enum: `track_enum_var` (memory), plus the own-body
        //     wrapper or declared-type payload walker when body-eligible.
        //   • non-shared struct: the `karac_drop_<T>` wrapper (body+memory,
        //     statement-end) when body-eligible with an own `impl Drop`;
        //     else the field-bodies walk (bodies) + `track_struct_var`
        //     (memory); else `track_struct_var` alone.
        // B-2026-08-04-17 — a STRUCT-LITERAL receiver is a fresh owned temp
        // too, and was silently excluded here.
        //
        // `expr_yields_fresh_owned_temp` admits only `Call` / `MethodCall`
        // (54 call sites depend on that narrowness, so it is widened HERE
        // rather than there — the same local-widening precedent as
        // `call_dispatch.rs`'s fresh-arg gate). A struct literal constructs a
        // new value and borrows nothing, so it is fresh-owned by construction.
        //
        // The disagreement was internal to this block: `shape_ok` below reads
        // `matches!(object.kind, StructLiteral | Call)`, i.e. the body already
        // anticipates a struct-literal receiver that this guard could never
        // let through. So the whole drop-track block was skipped for
        // `R { v: "x".to_string() }.get()` and the materialized temp's heap
        // field was never freed.
        //
        // Measured at -O0 (the level at which an ownership-emission fixture
        // actually tests what codegen emitted): 9 bytes leaked, exactly the
        // field String. `let r = R { .. }; r.get()` and `mk().get()` were both
        // clean — only the inline literal receiver leaked, which is what
        // localises the fault to this guard rather than to the drop machinery.
        // Invisible at -O2, where the allocation is deleted outright: the
        // vacuous-fixture hazard this row is about, caught by its own -O0 leg.
        let receiver_is_fresh_owned = self.expr_yields_fresh_owned_temp(object)
            || matches!(&object.kind, ExprKind::StructLiteral { .. });
        if receiver_is_fresh_owned {
            if is_shared {
                if let Some(heap_type) = self
                    .type_decls
                    .shared_types
                    .get(&type_name)
                    .map(|i| i.heap_type)
                {
                    self.track_rc_var(&synth, val.into_pointer_value(), heap_type);
                }
            } else {
                // Shape gate mirrors the interpreter hook exactly: only a
                // Call (user fn / variant ctor) or struct-literal receiver
                // is body-eligible — a CHAIN link (MethodCall receiver)
                // stays body-silent on both backends (recorded residual).
                let shape_ok = matches!(
                    &object.kind,
                    ExprKind::StructLiteral { .. } | ExprKind::Call { .. }
                );
                let bodies_eligible = shape_ok
                    && !self.user_ref_method_names.contains(method)
                    && matches!(
                        self.impl_method_self_and_borrow_return(&type_name, method),
                        Some((
                            crate::ast::SelfParam::Ref | crate::ast::SelfParam::MutRef,
                            false
                        ))
                    );
                let has_user_drop = self
                    .program_snapshot
                    .as_deref()
                    .map(|p| p.drop_method_keys.contains_key(&type_name))
                    .unwrap_or(false);
                if is_value_enum {
                    // Memory only. ENUM receiver bodies are deliberately NOT
                    // registered: a ref-self method that matches on `self`
                    // and binds the payload fires the interpreter's arm
                    // channel (a pre-existing interp-only fire on borrowed
                    // self, B-2026-08-01-6) — registering a walker here
                    // would stack a second fire on one side or the other.
                    // Struct receivers below carry the body work.
                    self.track_enum_var(&type_name, slot);
                } else if bodies_eligible && has_user_drop {
                    self.track_user_drop_var(&type_name, "__urecv_drop_tmp", slot);
                } else if bodies_eligible && self.type_runs_user_drop(&type_name, &mut Vec::new()) {
                    if let Some(f) = self.field_bodies_fn_for_owned_temp(&type_name) {
                        self.track_user_drop_var_with_fn(&type_name, "__urecv_drop_tmp", slot, f);
                    }
                    self.track_struct_var(&type_name, slot);
                } else {
                    self.track_struct_var(&type_name, slot);
                }
            }
        }

        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: object.span,
        };
        let result = self.compile_method_call(&synth_expr, method, args, call_span, call_span);

        // Drop the dispatch-only registrations (the queued drop, if any,
        // references the alloca, not the name, so it stays armed).
        self.variables.remove(&synth);
        self.var_types.var_type_names.remove(&synth);

        result.map(Some)
    }

    /// General owned-temp tracking, slice 3b — element-type-aware read methods
    /// (`get`/`first`/`last`/`get_unchecked`/`contains`) on a FRESH-TEMP
    /// (non-identifier) `Vec`/`VecDeque` receiver: `make_vec().get(0)`,
    /// `build_ids().contains(x)`. The typechecker records the (scalar) element
    /// `TypeExpr` keyed by the MethodCall span in `temp_recv_elem_types` — it
    /// can't be recovered from `expr_types` because the receiver and the
    /// method call share one span, which holds the method's `Option[T]`
    /// *result* type, and the LLVM `{ptr,len,cap}` shape is element-erased.
    /// With the element type in hand: compile the receiver, materialize it into
    /// a synthetic local, register the element type, drop-track the fresh temp
    /// (a `FreeVecBuffer` at the enclosing frame's exit — the read methods
    /// borrow `self`, so the caller owns the temp), then re-dispatch through
    /// the identifier-keyed `compile_vec_method`.
    ///
    /// Returns `Ok(None)` when there's no recorded element type (not a
    /// serviceable fresh-temp Vec read), so the caller falls through to the
    /// String redispatch / diagnostic — a pure addition that can't change any
    /// existing case.
    ///
    /// Element-type-generic: the typechecker records SCALAR elements for all
    /// five read methods, STRING elements for the borrow-returning
    /// `get`/`first`/`last` plus `contains` (slice 3b-heap), and one-level nested
    /// `Vec[scalar]` / `VecDeque[scalar]` elements (`Vec[Vec[i64]]`) for
    /// `get`/`first`/`last` (slice 3e). For a String *or* nested-Vec element the
    /// recorded
    /// `TypeExpr` lowers to `vec_struct_type`, so `track_vec_var`'s
    /// `FreeVecBuffer` takes the vec-struct recursion and per-element frees each
    /// inner buffer (a `String`'s bytes, or a row's POD data) before the outer
    /// buffer — and the `Option[ref String]` / `Option[ref Vec[scalar]]`
    /// `get`/`first`/`last` return is suppressed from independent drop at the
    /// match arm by `scrutinee_is_borrow_call` (which keys off the method, not
    /// the receiver shape), so the per-element storage is freed exactly once at
    /// frame exit while the borrow reads it. `contains` returns `bool` — no
    /// borrow escapes, so it carries no suppression obligation; it only needs
    /// the same per-element receiver free, and the compared arg is borrowed not
    /// consumed (a fresh-owned arg is the separate 3b-c operand-temp leak). A
    /// scalar element owns no nested
    /// heap, so the outer-buffer `FreeVecBuffer` is its complete drop. The
    /// drop-track is gated on `expr_yields_fresh_owned_temp`, and the `cap > 0`
    /// guard inside `FreeVecBuffer` is a second backstop, so a (hypothetical)
    /// borrow-returning receiver is never double-freed. Other heap elements
    /// (`Vec[T]`, user struct/enum, Map/Set) are not recorded — they need
    /// element-drop threading (`elem_agg_drop`) this helper doesn't carry.
    fn try_compile_freshtemp_vec_read_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Identifier / self receivers route through the named-binding dispatch.
        if matches!(&object.kind, ExprKind::Identifier(_) | ExprKind::SelfValue) {
            return Ok(None);
        }
        // B-2026-08-18-24 — keyed by the CALL's span, which is what
        // `record_temp_receiver_types` inserts under ("keyed by the call span
        // — the same collision dodge `method_unwrap_inner_types` /
        // `method_callee_types` use"). Reading it at the RECEIVER's span
        // resolved only while `MethodCall` copied its object's span; once the
        // two are distinct the lookup misses, this arm returns `Ok(None)`, and
        // dispatch falls through to "no handler for method '<m>' on
        // non-identifier receiver".
        let span_key = (call_span.offset, call_span.length);
        let Some(elem_te) = self
            .span_tables
            .temp_recv_elem_types
            .get(&span_key)
            .cloned()
        else {
            return Ok(None);
        };
        let cur_fn = self
            .current_fn
            .ok_or_else(|| "fresh-temp Vec read method outside fn".to_string())?;

        let recv_val = self.compile_expr(object)?;
        // The receiver must be the `{ptr, len, cap}` Vec struct; bail otherwise
        // (the typechecker gate should guarantee it, but stay shape-defensive).
        let BasicValueEnum::StructValue(sv) = recv_val else {
            return Ok(None);
        };
        if sv.get_type() != self.vec_struct_type() {
            return Ok(None);
        }

        let elem_llvm = self.llvm_type_for_type_expr(&elem_te);
        let slot = self.create_entry_alloca(cur_fn, "__vrecv_tmp", recv_val.get_type());
        self.builder.build_store(slot, recv_val).unwrap();

        // Drop the fresh-owned receiver at the enclosing frame's exit (the
        // position ceiling). The cleanup references the slot pointer, not the
        // synth name, so it stays valid after the name is unregistered below.
        // For a user-STRUCT element (slice 3f), thread the synthesized
        // per-element `__karac_drop_<S>` so the `FreeVecBuffer` runs it on every
        // live element (freeing String/Vec/shared fields) before releasing the
        // outer buffer — the inline vec-struct recursion only reaches elements
        // that are *themselves* Vec/String. Scalar/String/nested-Vec elements
        // return `None` here (not in `struct_types`) and keep the plain path.
        if self.expr_yields_fresh_owned_temp(object) {
            if let Some(agg_drop) = self.vec_elem_agg_drop_for_type_expr(&elem_te) {
                self.track_vec_of_aggs_var(slot, elem_llvm, agg_drop);
            } else {
                self.track_vec_var(slot, Some(elem_llvm));
            }
        }

        // Register the synth name so the identifier-keyed `compile_vec_method`
        // resolves the element type and the slot. Unique per call site.
        let synth = format!("__vrecv_tmp_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            super::VarSlot {
                ptr: slot,
                ty: recv_val.get_type(),
            },
        );
        self.var_types
            .vec_elem_types
            .insert(synth.clone(), elem_llvm);
        self.var_types
            .var_elem_type_exprs
            .insert(synth.clone(), elem_te);
        self.var_types
            .var_type_names
            .insert(synth.clone(), "Vec".to_string());

        let result = self.compile_vec_method(&synth, slot, method, args);

        // Drop the dispatch-only registrations.
        self.variables.remove(&synth);
        self.var_types.vec_elem_types.remove(&synth);
        self.var_types.var_elem_type_exprs.remove(&synth);
        self.var_types.var_type_names.remove(&synth);

        result.map(Some)
    }

    /// Drop-track a fresh-owned `Vec` receiver of `len`/`is_empty`/`count`
    /// with a PER-ELEMENT walk, using the element type the typechecker
    /// recorded in `temp_recv_len_elem_types` (B-2026-07-31-43). The generic
    /// chokepoint (`materialize_owned_temp`) can't recover the element type
    /// for a chained receiver — the parser gives a MethodCall its receiver's
    /// span, so the chain's scalar result span-clobbers the receiver's
    /// `Vec[T]` in `expr_types` and the `owned_temp_drops` hint is absent —
    /// leaving an outer-buffer-only free that leaks every element
    /// String/row/aggregate. Element-shape routing mirrors
    /// `try_compile_freshtemp_vec_read_method`: a user struct/enum element
    /// threads its synthesized per-element drop (`track_vec_of_aggs_var`);
    /// String / nested-POD-Vec elements lower to `vec_struct_type`, which the
    /// `FreeVecBuffer` recursion per-element frees (`track_vec_var`).
    ///
    /// Returns `false` when there's no recorded element type (scalar elements
    /// — the outer-buffer free is already complete — or an unsupported shape)
    /// or the value isn't the `{ptr,len,cap}` struct; the caller falls back
    /// to `materialize_owned_temp` unchanged.
    fn try_track_len_family_recv_temp(
        &mut self,
        val: BasicValueEnum<'ctx>,
        span_key: (usize, usize),
    ) -> bool {
        let Some(elem_te) = self
            .span_tables
            .temp_recv_len_elem_types
            .get(&span_key)
            .cloned()
        else {
            return false;
        };
        if !self.llvm_ty_is_vec_struct(val.get_type()) {
            return false;
        }
        let Some(cur_fn) = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
        else {
            return false;
        };
        let elem_llvm = self.llvm_type_for_type_expr(&elem_te);
        let slot = self.create_entry_alloca(cur_fn, "__owned_tmp", val.get_type());
        self.builder.build_store(slot, val).unwrap();
        if let Some(agg_drop) = self.vec_elem_agg_drop_for_type_expr(&elem_te) {
            self.track_vec_of_aggs_var(slot, elem_llvm, agg_drop);
        } else {
            self.track_vec_var(slot, Some(elem_llvm));
        }
        true
    }

    /// `<chain>.get(i).unwrap()` / `.first().unwrap()` / `.last().expect(..)`
    /// — an `unwrap`/`expect` peeled off a borrow accessor
    /// (`scrutinee_is_borrow_call`). The result is a BORROW of the
    /// container's element storage — the typechecker types it `ref T` and
    /// rejects owned use — so despite being MethodCall-shaped it is NOT a
    /// fresh owned temp, and a consuming site must not drop-track it: when
    /// the container is itself a tracked fresh temp, its per-element walk
    /// already frees that storage, and a second free aborts
    /// (B-2026-08-01-1: `mk_rows().first().unwrap().len()`). `unwrap_or`
    /// variants are excluded — their miss arm substitutes an owned default,
    /// so their ownership is branch-dependent, and no double-free shape has
    /// been proven for them.
    fn expr_is_unwrap_of_borrow_accessor(&self, expr: &Expr) -> bool {
        let ExprKind::MethodCall { object, method, .. } = &expr.kind else {
            return false;
        };
        matches!(method.as_str(), "unwrap" | "expect") && self.scrutinee_is_borrow_call(object)
    }

    /// B-2026-08-14-20 — a `Slice` builtin method on a NON-IDENTIFIER
    /// receiver: `s.bytes().to_vec()`, `v.as_slice().first()`,
    /// `chunks[0].len()`. Sibling of `try_compile_nonident_collection_method`
    /// (String) and `try_compile_freshtemp_vec_read_method` (Vec), for the
    /// receiver shape neither of those answers.
    ///
    /// Much simpler than the Vec twin, and for one reason: a slice OWNS
    /// NOTHING. It is a `{ptr, len}` view into somebody else's buffer, so
    /// there is no fresh-temp to drop-track, no element walk to thread, and no
    /// span-keyed element table to consult — the two words spill to a slot,
    /// the synthetic name carries the element type for the duration of the
    /// re-dispatch, and both come back off the tables immediately after.
    ///
    /// Returns `Ok(None)` for anything not provably a slice, leaving the
    /// caller's dispatch-fail error to fire unchanged: a non-`Slice` method
    /// name, a receiver shape `infer_slice_elem_from_rhs` cannot type, or —
    /// the case that makes the shape check mandatory rather than defensive —
    /// a receiver whose compiled value is not the 2-field header. `bytes()`
    /// is not slice-exclusive: `Response.bytes()` hands back an owned
    /// `Vec[u8]`, and reading its 3-field `{ptr,len,cap}` aggregate as a
    /// slice header would silently mistake `cap` for nothing at all.
    fn try_compile_nonident_slice_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
        args_close_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Identifier / self receivers already have a name to key on.
        if matches!(&object.kind, ExprKind::Identifier(_) | ExprKind::SelfValue) {
            return Ok(None);
        }
        // One list rather than two, same as the interpreter's slice gate: a
        // name outside the builtin `Slice` surface belongs to whatever
        // dispatcher owns it, not here.
        //
        // B-2026-08-18-14 — …EXCEPT a method a USER impl block declared on the
        // `Slice` head. `impl Head for Slice[i64] { fn first_or(...) }` called
        // on `v[0..3]` is not a builtin name, so this declined and the caller
        // fell through to the element-pointer lowering, which compiled the
        // RANGE as if it were a subscript and died on "no handler for
        // expression kind Range" — a check/build divergence, since `karac
        // check` and `--interp` both accept the program.
        //
        // Nothing below this gate is method-specific: it materializes the
        // view's `{ptr, len}` header into a synth local and re-dispatches by
        // IDENTIFIER, which is precisely the spelling that already worked
        // (`let s: Slice[i64] = v[0..3]; s.first_or(-1)`). So admitting the
        // user method here routes it to the same path its bound twin takes,
        // rather than adding one.
        if !crate::typechecker::SLICE_BUILTIN_METHODS.contains(&method)
            && !self.user_impl_method_exists(call_span, "Slice", method)
        {
            return Ok(None);
        }
        let Some(elem_llvm) = self.infer_slice_elem_from_rhs(object) else {
            return Ok(None);
        };
        let elem_te = self.slice_elem_type_expr_from_rhs(object);
        let cur_fn = self
            .current_fn
            .ok_or_else(|| "slice method on a temporary outside a fn".to_string())?;

        let recv_val = self.compile_expr(object)?;
        let BasicValueEnum::StructValue(sv) = recv_val else {
            return Ok(None);
        };
        if sv.get_type() != self.slice_struct_type() {
            return Ok(None);
        }
        let slot = self.create_entry_alloca(cur_fn, "__srecv_tmp", recv_val.get_type());
        self.builder.build_store(slot, recv_val).unwrap();

        let synth = format!("__srecv_tmp_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            super::state::VarSlot {
                ptr: slot,
                ty: recv_val.get_type(),
            },
        );
        self.var_types
            .slice_elem_types
            .insert(synth.clone(), elem_llvm);
        if let Some(te) = elem_te {
            self.var_types.var_elem_type_exprs.insert(synth.clone(), te);
        }
        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: object.span,
        };
        let out = self.compile_method_call(&synth_expr, method, args, call_span, args_close_span);

        // Drop the dispatch-only registrations.
        self.variables.remove(&synth);
        self.var_types.slice_elem_types.remove(&synth);
        self.var_types.var_elem_type_exprs.remove(&synth);

        out.map(Some)
    }

    /// B-2026-08-21-25 — the fixed-`Array[T, N]` twin of
    /// [`Self::try_compile_nonident_slice_method`]: a method call whose
    /// receiver is an array-valued TEMPORARY rather than a named binding —
    /// `n.to_ne_bytes().len()`, `mk().first()`, `b"abc".is_sorted()`.
    ///
    /// The whole fixed-array dispatch block is identifier-keyed: it reads
    /// `self.variables[name].ty` and fires only when that slot's LLVM type is
    /// an `ArrayType`. A temporary has no slot and no name, so every method on
    /// the surface reached the dispatch-fail error under `karac build` while
    /// `--interp` — which dispatches a fixed array as a Vec — answered. The
    /// two-line spelling (`let b = n.to_ne_bytes(); b.len()`) always worked,
    /// which is the oracle this arm is measured against: materialize the
    /// aggregate into a slot, register the synthetic name, re-dispatch by
    /// IDENTIFIER, unregister. Nothing here is method-specific.
    ///
    /// The SCALAR-element gate is not a shortcut, it is what makes the
    /// materialization free of any cleanup obligation. An array of `Int` /
    /// `Float` (which covers `bool` and `char`, both lowered to `IntType`)
    /// owns no heap, so spilling it to an alloca creates no second owner and
    /// no drop to track — the same property that made the slice sibling safe,
    /// arrived at differently (a slice owns nothing because it is a view; a
    /// scalar array owns nothing because its elements are scalars). It is
    /// also exactly the gate the typechecker puts on the builtin surface
    /// (`method_sequence_mutation.rs`, B-2026-07-17-19) and the one
    /// `compile_fixed_array_read` is written against, so for those methods it
    /// excludes nothing reachable. A USER impl on a non-scalar head —
    /// `impl Tag for Array[String, 2]` — is reachable and stays loud here
    /// (B-2026-08-21-43): giving it a lowering means answering who frees the
    /// temporary's element buffers, which this arm does not establish.
    ///
    /// `as_ptr` / `as_mut_ptr` are deliberately absent from the gate even
    /// though the identifier arm answers them. Handing out the interior
    /// address of a value with no name is a different question from reading
    /// it, and the interpreter refuses the same program ("method 'as_ptr' not
    /// found"), so admitting it here would replace an agreement between the
    /// backends with a divergence pointing the other way.
    ///
    /// Placed last, immediately before the diagnostic, for the reason the
    /// slice sibling records: every other dispatcher has already declined, so
    /// a receiver compiled here and then rejected on shape leaves dead IR in
    /// an already-failing compile rather than a duplicated side effect in a
    /// working one.
    fn try_compile_nonident_fixed_array_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
        args_close_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Identifier / self receivers already have a name to key on.
        if matches!(&object.kind, ExprKind::Identifier(_) | ExprKind::SelfValue) {
            return Ok(None);
        }
        // The builtin fixed-array read surface, plus any method a USER impl
        // declared on the `Array` head — the same pairing the slice sibling
        // admits (B-2026-08-18-14), and for the same reason: this arm routes
        // a call to the path its bound twin already takes rather than adding
        // one, so a user method belongs here exactly as much as a builtin.
        if !matches!(
            method,
            "len" | "is_empty" | "get" | "first" | "last" | "contains" | "is_sorted"
        ) && !self.user_impl_method_exists(call_span, "Array", method)
        {
            return Ok(None);
        }
        let cur_fn = self
            .current_fn
            .ok_or_else(|| "Array method on a temporary outside a fn".to_string())?;

        let recv_val = self.compile_expr(object)?;
        let BasicTypeEnum::ArrayType(at) = recv_val.get_type() else {
            return Ok(None);
        };
        if !matches!(
            at.get_element_type(),
            BasicTypeEnum::IntType(_) | BasicTypeEnum::FloatType(_)
        ) {
            return Ok(None);
        }
        let slot = self.create_entry_alloca(cur_fn, "__arecv_tmp", recv_val.get_type());
        self.builder.build_store(slot, recv_val).unwrap();

        let synth = format!("__arecv_tmp_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            super::state::VarSlot {
                ptr: slot,
                ty: recv_val.get_type(),
            },
        );
        // `var_type_names` is what `inferred_receiver_type` reads to qualify a
        // user-impl dispatch as `Array.<method>`; an Array local carries the
        // bare head name `"Array"` there, so the synthetic name carries it too.
        self.var_types
            .var_type_names
            .insert(synth.clone(), "Array".to_string());
        if let Some(te) = self.array_elem_type_expr_from_rhs(object) {
            self.var_types
                .array_elem_type_exprs
                .insert(synth.clone(), te);
        }
        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: object.span,
        };
        let out = self.compile_method_call(&synth_expr, method, args, call_span, args_close_span);

        // Drop the dispatch-only registrations.
        self.variables.remove(&synth);
        self.var_types.var_type_names.remove(&synth);
        self.var_types.array_elem_type_exprs.remove(&synth);

        out.map(Some)
    }

    /// General owned-temp tracking, slice 3d — read methods on a FRESH-TEMP
    /// (non-identifier) `Map`/`Set` receiver: `make_map().get(k)`,
    /// `make_map().contains_key(k)`, `make_set().contains(x)`. The Map/Set
    /// handle is a plain `ptr`, so unlike the Vec path there's no struct shape
    /// to key off — the receiver's whole `Map[K,V]` / `Set[T]` `TypeExpr` is
    /// recorded span-keyed by the typechecker in `temp_recv_mapset_types`
    /// (`compile_map_method` needs K+V; the handle drop is classified from the
    /// full type). With it in hand: compile the receiver to its handle,
    /// materialize the handle into a synthetic slot, register K/V (or elem) so
    /// the identifier-keyed `compile_map_method` / `compile_set_method` resolve
    /// it, drop-track the handle (a `FreeMapHandle` via `track_map_var`,
    /// classified by `map_temp_cleanup_parts`, at the enclosing frame's exit —
    /// the read methods borrow the map, so the caller owns the temp), then
    /// re-dispatch.
    ///
    /// Returns `Ok(None)` when there's no recorded type (not a serviceable
    /// fresh-temp Map/Set read), so the caller falls through unchanged.
    ///
    /// Type-generic over the recorded K/V/elem: the typechecker records SCALAR
    /// and owned-`String` K/V/elem (slice 3d + 3d-heap). The helper itself needs
    /// no per-type branching — `map_temp_cleanup_parts` classifies `key_is_vec`/
    /// `val_is_vec` from the `TypeExpr`, so a `String` K/V makes the single
    /// `FreeMapHandle` per-entry free the element buffers
    /// (`karac_map_free_with_drop_vec`), and `compile_map_method` resolves the
    /// String LLVM type for the lookup. `Map.get` returns `Option[ref V]`
    /// aliasing a value slot inside the map; the arm binding's independent drop
    /// is suppressed by `scrutinee_is_borrow_call` (keys off the method, not the
    /// receiver), so for a String V the per-entry buffer is freed exactly once at
    /// frame exit while the borrow reads it — the same single-free shape the
    /// `Vec[String]` slice established. `contains_key`/`contains` return `bool`
    /// (no borrow). The drop-track is gated on `expr_yields_fresh_owned_temp`.
    /// Other heap K/V (`Vec[T]`, user struct/enum, nested Map) are excluded by
    /// the typechecker gate — they need element-drop threading not carried here.
    fn try_compile_freshtemp_mapset_read_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        if matches!(&object.kind, ExprKind::Identifier(_) | ExprKind::SelfValue) {
            return Ok(None);
        }
        // Call-span keyed, for the reason `try_compile_freshtemp_vec_read_method`
        // records (B-2026-08-18-24).
        let span_key = (call_span.offset, call_span.length);
        let Some(recv_te) = self.mapset.temp_recv_mapset_types.get(&span_key).cloned() else {
            return Ok(None);
        };
        let cur_fn = self
            .current_fn
            .ok_or_else(|| "fresh-temp Map/Set read method outside fn".to_string())?;

        // Extract the container head + K/V (or elem) TypeExprs from the recorded
        // `Map[K,V]` / `Set[T]` type.
        let crate::ast::TypeKind::Path(path) = &recv_te.kind else {
            return Ok(None);
        };
        let head = path
            .segments
            .first()
            .map(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let nth = |i: usize| -> Option<TypeExpr> {
            match path.generic_args.as_ref()?.get(i)? {
                crate::ast::GenericArg::Type(t) => Some(t.clone()),
                _ => None,
            }
        };

        let recv_val = self.compile_expr(object)?;
        // The handle must be a plain pointer; bail otherwise (the typechecker
        // gate should guarantee it, but stay shape-defensive).
        if !recv_val.is_pointer_value() {
            return Ok(None);
        }
        let slot = self.create_entry_alloca(cur_fn, "__mrecv_tmp", recv_val.get_type());
        self.builder.build_store(slot, recv_val).unwrap();

        // Drop the fresh-owned handle at the enclosing frame's exit, classified
        // from the full receiver type (scalar K/V → no per-entry heap drop).
        if self.expr_yields_fresh_owned_temp(object) {
            let (key_is_vec, val_is_vec, key_shared, val_shared, val_drop_fn, key_drop_fn) =
                self.map_temp_cleanup_parts(&recv_te);
            self.track_map_var_with_val_drop(
                slot,
                key_is_vec,
                val_is_vec,
                val_shared,
                key_shared,
                val_drop_fn,
                key_drop_fn,
            );
        }

        // Register the synth name so the identifier-keyed dispatch resolves the
        // slot + the K/V (or elem) LLVM types. Unique per call site.
        let synth = format!("__mrecv_tmp_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            super::VarSlot {
                ptr: slot,
                ty: recv_val.get_type(),
            },
        );

        let result = if head == "Set" {
            self.var_types
                .var_type_names
                .insert(synth.clone(), "Set".to_string());
            if let Some(elem) = nth(0) {
                self.mapset
                    .set_elem_types
                    .insert(synth.clone(), self.llvm_type_for_type_expr(&elem));
            }
            let r = self.compile_set_method(&synth, method, args);
            self.mapset.set_elem_types.remove(&synth);
            r
        } else {
            self.var_types
                .var_type_names
                .insert(synth.clone(), "Map".to_string());
            if let Some(k) = nth(0) {
                self.mapset
                    .map_key_types
                    .insert(synth.clone(), self.llvm_type_for_type_expr(&k));
            }
            if let Some(v) = nth(1) {
                self.mapset
                    .map_val_types
                    .insert(synth.clone(), self.llvm_type_for_type_expr(&v));
            }
            let r = self.compile_map_method(&synth, method, args);
            self.mapset.map_key_types.remove(&synth);
            self.mapset.map_val_types.remove(&synth);
            r
        };

        self.variables.remove(&synth);
        self.var_types.var_type_names.remove(&synth);

        result.map(Some)
    }
}

/// Map a bare lowercase ambient-resource alias (`env`, `clock`, ...) to
/// its capitalized effect-resource name, mirroring the interpreter's
/// alias table in `src/interpreter/method_call.rs`. Returns `None` for
/// any identifier that is not an ambient resource alias. Codegen lowers
/// only the subset the runtime currently backs (see
/// `compile_ambient_resource_method`); the rest still resolve here so
/// they get a precise "not yet lowered" error rather than the generic
/// dispatch fall-through.
pub(super) fn ambient_resource_for_alias(alias: &str) -> Option<&'static str> {
    match alias {
        "clock" => Some("Clock"),
        "env" => Some("Env"),
        "rand" => Some("RandomSource"),
        "stdin" => Some("Stdin"),
        "stdout" => Some("Stdout"),
        "stderr" => Some("Stderr"),
        "fs" => Some("FileSystem"),
        _ => None,
    }
}

/// Vtable slot index of `method` within `resource`'s canonical method
/// order (`prelude::AMBIENT_RESOURCE_METHODS`), or `None` if the pair has
/// no slot — in which case there's no runtime override dispatch for it
/// and the call falls straight to the FFI default.
pub(super) fn ambient_method_index(resource: &str, method: &str) -> Option<usize> {
    crate::prelude::AMBIENT_RESOURCE_METHODS
        .iter()
        .find(|(r, _)| *r == resource)
        .and_then(|(_, methods)| methods.iter().position(|m| *m == method))
}

/// True iff `compile_ambient_ffi` has a builtin-default lowering for this
/// `(resource, method)` pair. MUST stay in lockstep with that match's arms.
///
/// Used to route a capitalized `Resource.method()` call (`call_dispatch.rs`)
/// to `compile_ambient_resource_method` even when the pair has no
/// `AMBIENT_RESOURCE_METHODS` vtable slot — i.e. FFI-default methods like
/// `RandomSource.next_u64` / `Env.args`. Without this, only the lowercase
/// alias form (`rand.next_u64()`, routed in `compile_method_call`) reached
/// the FFI lowering; the capitalized form fell through to `compile_assoc_call`
/// and errored "no handler". (Vtable-slotted pairs — `Clock.now`, `Env.set` —
/// are already routed by the `ambient_method_index` check at the call site;
/// this is purely the no-slot complement.)
pub(super) fn ambient_ffi_lowered(resource: &str, method: &str) -> bool {
    matches!(
        (resource, method),
        ("Env", "set")
            | ("Clock", "now")
            | ("RandomSource", "next_u64")
            | ("Env", "args")
            | ("Env", "var")
            | ("Stdin", "read_line")
            | ("Stdin", "read_to_string")
            | ("Stdout", "print")
            | ("Stdout", "println")
            | ("Stdout", "flush")
            | ("Stderr", "print")
            | ("Stderr", "println")
            | ("Stderr", "flush")
    )
}
