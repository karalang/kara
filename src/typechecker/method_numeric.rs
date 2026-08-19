//! Scalar-primitive method typechecking — the numeric and `char` surface.
//!
//! Split out of `expr_method_call.rs` as the first slice of the
//! `infer_method_call` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Holds every
//! built-in method whose receiver is an `Int` / `UInt` / `Float` / `Char`
//! primitive: `abs` / `signum` / `sqrt`, the `crate::float_math`
//! transcendental + rounding table, the float/int bit-width converters and
//! bit intrinsics (`count_ones`, `leading_zeros`, …), the wrapping /
//! saturating / checked / overflowing arithmetic families, `div_euclid` /
//! `rem_euclid`, `pow`, `min` / `max` / `clamp`, `abs_diff`, the rotates, and
//! the `char` classification + conversion surface.
//!
//! The block order inside `try_scalar_primitive_method` is load-bearing —
//! `infer_method_call` is a first-match-wins chain, so these guards keep the
//! exact relative order they had inline, and the function is called from the
//! same position in that chain.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::token::Span;

use super::types::{type_display, FloatSize, IntSize, Type, UIntSize};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    /// Type a built-in method on a scalar primitive receiver.
    ///
    /// Returns `Some(ty)` when this surface claims `method` (including
    /// `Some(Type::Error)` when it claims the name but the call is
    /// ill-formed and a diagnostic has been emitted), and `None` when the
    /// name belongs to some later link in the `infer_method_call` chain.
    pub(super) fn try_scalar_primitive_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
        args_close_span: &Span,
        receiver_for_lookup: &Type,
    ) -> Option<Type> {
        // Built-in `abs` on signed-integer and float primitives — `x.abs() ->
        // Self`. Handled here as a dedicated value-receiver method rather than
        // through the registered builtin-impl table: those `Neg`/`Ord` impls
        // model the *type-receiver* / operator-lowering form (`self` in the
        // params list, e.g. `i64.cmp(a, b)`), whose arity is incompatible with
        // the value-receiver `x.abs()` shape. Restricted to `Int` (signed) and
        // `Float`; unsigned `abs` is rejected (no `abs` on `u*`, matching
        // Rust), falling through to the `NoMethodFound` arm below. Backends:
        // interpreter `method_call.rs` (`checked_abs`, traps on `iN::MIN`),
        // codegen `method_call.rs` (`select(x<0, trapping(-x), x)`).
        if method == "abs"
            && args.is_empty()
            && matches!(receiver_for_lookup, Type::Int(_) | Type::Float(_))
        {
            return Some(receiver_for_lookup.clone());
        }
        // Built-in `signum` — `x.signum() -> Self`. Signed-int receivers yield
        // -1 / 0 / 1 (Rust `iN::signum`); float receivers yield -1.0 / +1.0 /
        // NaN, with `signum` carrying the sign of a signed zero (Rust
        // `f64::signum` = `copysign(1.0, x)`, NaN-preserving). Unsigned integers
        // have no `signum` in Rust, so `UInt` falls through to `NoMethodFound`.
        // Backends: interpreter `method_call.rs`, codegen `method_call.rs`.
        if method == "signum"
            && args.is_empty()
            && matches!(receiver_for_lookup, Type::Int(_) | Type::Float(_))
        {
            return Some(receiver_for_lookup.clone());
        }
        // Built-in `sqrt` on float primitives — `x.sqrt() -> Self`. Float-only
        // (no integer square root); lowers to the `llvm.sqrt` intrinsic in
        // codegen (a single `f64.sqrt` instruction on wasm — no libm) and
        // `f64::sqrt` in the interpreter. The first piece of a numeric math
        // surface, driven by Plume's flow field needing vector normalization
        // (`docs/dogfooding.md`). The rest of that surface (sin/cos/tan/exp/ln/
        // log2/pow/atan2/floor/ceil/round) lives in the `crate::float_math`
        // block just below. Backends: interpreter `method_call.rs`, codegen
        // `method_call.rs`.
        if method == "sqrt" && args.is_empty() && matches!(receiver_for_lookup, Type::Float(_)) {
            return Some(receiver_for_lookup.clone());
        }
        // Built-in float arithmetic helpers — `x.recip() -> Self` (`1.0 / x`),
        // `x.to_degrees() -> Self`, `x.to_radians() -> Self`, `x.fract() -> Self`
        // (`x - x.trunc()`). Pure IEEE arithmetic (no libm, no intrinsic):
        // `recip` is a single `fdiv`, the angle conversions a single `fmul` by
        // the same constant the interpreter uses, and `fract` an `fsub` against
        // `llvm.trunc`, so `run == build` is bit-exact. Float-only.
        if matches!(method, "recip" | "to_degrees" | "to_radians" | "fract")
            && args.is_empty()
            && matches!(receiver_for_lookup, Type::Float(_))
        {
            return Some(receiver_for_lookup.clone());
        }
        // Built-in scalar transcendental + rounding math on float primitives —
        // `x.sin()` / `x.cos()` / `x.tan()` / `x.exp()` / `x.ln()` / `x.log2()`
        // / `x.floor()` / `x.ceil()` / `x.round()` (unary, `-> Self`) and
        // `x.pow(y)` / `x.atan2(y)` (binary, one argument of the same float
        // type, `-> Self`). The value-receiver shape, mirroring `sqrt`/`abs`;
        // the surface is the single `crate::float_math` table the interpreter
        // and codegen share. Float-only — integer receivers fall through to
        // `NoMethodFound`. Backends: interpreter `method_call.rs` (Rust
        // `f64::*`), codegen `method_call.rs` (LLVM intrinsics; `atan2` via a
        // direct libm call). Driven by the Plume flow-field dogfood.
        if matches!(receiver_for_lookup, Type::Float(_)) {
            if let Some(kind) = crate::float_math::classify(method) {
                match kind {
                    crate::float_math::FloatMathKind::Unary => {
                        if !args.is_empty() {
                            self.type_error(
                                format!("{method} expects 0 arguments, got {}", args.len()),
                                *span,
                                TypeErrorKind::WrongNumberOfArgs,
                            );
                            return Some(Type::Error);
                        }
                        return Some(receiver_for_lookup.clone());
                    }
                    crate::float_math::FloatMathKind::Binary => {
                        if args.len() != 1 {
                            self.type_error(
                                format!("{method} expects 1 argument, got {}", args.len()),
                                *span,
                                TypeErrorKind::WrongNumberOfArgs,
                            );
                            return Some(Type::Error);
                        }
                        // The argument is the same float type as the receiver. A
                        // suffix-free float literal promotes to it (Q4 rule, like
                        // `wrapping_*`); otherwise it must match exactly.
                        let arg = &args[0].value;
                        let arg_ty = self.infer_expr(arg);
                        if matches!(&arg.kind, ExprKind::Float(_, None)) {
                            self.record_expr_type(&arg.span, receiver_for_lookup);
                            return Some(receiver_for_lookup.clone());
                        }
                        if arg_ty != Type::Error && arg_ty != *receiver_for_lookup {
                            self.type_error(
                                format!(
                                    "{method} expects an argument of type `{}`, got `{}`",
                                    type_display(receiver_for_lookup),
                                    type_display(&arg_ty)
                                ),
                                arg.span,
                                TypeErrorKind::TypeMismatch,
                            );
                            return Some(Type::Error);
                        }
                        return Some(receiver_for_lookup.clone());
                    }
                }
            }
        }
        // IEEE-754 bit reinterpretation (used by protobuf `float`/`double`
        // fixed-width codecs). `to_bits` → `u64` (f64 pattern), `to_bits32` →
        // `u32` (the value rounded to f32, then its 32-bit pattern). The width
        // is in the method name so no receiver-width recovery is needed.
        if args.is_empty() && matches!(receiver_for_lookup, Type::Float(_)) {
            if method == "to_bits" {
                return Some(Type::UInt(UIntSize::U64));
            }
            if method == "to_bits32" {
                return Some(Type::UInt(UIntSize::U32));
            }
        }
        // The inverse: reinterpret an integer's low bits as a float.
        // `bits_as_f64` (from a `u64`) / `bits_as_f32` (from a `u32`).
        if args.is_empty() && matches!(receiver_for_lookup, Type::Int(_) | Type::UInt(_)) {
            if method == "bits_as_f64" {
                return Some(Type::Float(FloatSize::F64));
            }
            if method == "bits_as_f32" {
                return Some(Type::Float(FloatSize::F32));
            }
        }
        // Float→int conversion families (phase-8 § "Saturating float→int",
        // slice 2): `f.{saturating,wrapping,checked,trunc}_to_<intN>()` on
        // `f32`/`f64`. `checked_*` returns `Option[intN]` (None on
        // NaN/out-of-range); the others return `intN`. `trunc_*` additionally
        // carries `panics` (seeded in effectchecker). Method-name → family +
        // target shared with the interpreter / effectchecker via
        // `crate::numeric_conv`. Backends: interpreter `method_call.rs`
        // computes via `numeric_conv::convert_float_to_int`; the bit-exact
        // `fptosi.sat`/`fptoui.sat` codegen is slice 4 (interpreter-only until
        // then — `karac build` errors loudly rather than miscompiling).
        if args.is_empty() && matches!(receiver_for_lookup, Type::Float(_)) {
            if let Some((family, target, _, _)) = crate::numeric_conv::parse_float_to_int(method) {
                if let Some(int_ty) = self.primitive_type(target) {
                    return Some(match family {
                        crate::numeric_conv::FloatToIntFamily::Checked => Type::Named {
                            name: "Option".to_string(),
                            args: vec![int_ty],
                        },
                        _ => int_ty,
                    });
                }
            }
        }
        // Int→float conversions (same slice): `n.to_f32()` / `n.to_f64()` on
        // every signed/unsigned integer. The implicit-widening cases already
        // work without `as`; these method forms ship for code-style
        // consistency with the float→int families above. Effect-free.
        if args.is_empty() && matches!(receiver_for_lookup, Type::Int(_) | Type::UInt(_)) {
            if method == "to_f32" {
                if let Some(t) = self.primitive_type("f32") {
                    return Some(t);
                }
            }
            if method == "to_f64" {
                if let Some(t) = self.primitive_type("f64") {
                    return Some(t);
                }
            }
        }
        // ASCII byte-classification predicates on integer scalars (notably the
        // `u8` bytes yielded by `String.bytes()`): `b.is_ascii_digit()`,
        // `b.is_ascii_alphabetic()`, `b.is_ascii_hexdigit()` → `bool`. Phase-8
        // floor for the self-hosting lexer's byte-indexed scan
        // (phase-12-self-hosting.md); mirror Rust's `u8::is_ascii_*`. Effect-free
        // value-receiver methods (codegen lowers to inline range checks; no
        // extern). `is_ascii_alpha`-vs-`_` (`is_alpha`) is composed in Kāra as
        // `b.is_ascii_alphabetic() or b == b'_'`.
        if args.is_empty()
            && matches!(receiver_for_lookup, Type::Int(_) | Type::UInt(_))
            && matches!(
                method,
                "is_ascii_digit" | "is_ascii_alphabetic" | "is_ascii_hexdigit"
            )
        {
            return Some(Type::Bool);
        }
        // Wrapping integer arithmetic — `wrapping_add` / `wrapping_sub` /
        // `wrapping_mul` (design.md § Arithmetic Overflow, the `wrapping_*`
        // family): two's-complement wraparound with NO overflow trap, the
        // non-trapping sibling of the checked `+`/`-`/`*` path
        // (`emit_checked_int_arith`). Both operands and the result are the
        // receiver's type.
        //
        // **Every width from `i8`/`u8` up to the 64-bit pair** (`i128`/`u128`
        // remain out — they are not i64-representable in the interpreter, and
        // `NoMethodFound` still fires for them). The narrow widths were
        // originally excluded because they need width-masking; only the
        // INTERPRETER actually did, since it is i64-backed, and it already
        // recovers the receiver width for the `checked_*` / `saturating_*` /
        // `overflowing_*` families via `overflow_arg_width`. Codegen needs
        // nothing: `i32`/`u32` lower to a real LLVM `i32`
        // (`types_lowering.rs`), so `build_int_add` wraps at the right width by
        // construction.
        //
        // Widening was forced by B-2026-08-19-1: a `#[gpu]` kernel's element
        // types are `i32` / `u32` / `f32`, and `wrapping_*` existed only on
        // `i64` / `u64` / `usize` — disjoint sets, so there was NO way to spell
        // wrapping integer arithmetic in a kernel, while bare `+` silently
        // wrapped on the device. design.md § Arithmetic Overflow requires the
        // escape hatch be named at the site; this is what makes naming it
        // possible.
        //
        // Backends: codegen `method_call.rs` (`build_int_{add,sub,mul}`),
        // interpreter `method_call.rs` (`eval_wrapping_arith`, masked to the
        // receiver width). Secondary motivation, from the original slice: a
        // wrapping kernel body is straight-line (no per-element overflow-trap
        // branch), which is what lets LLVM auto-vectorize integer slice
        // kernels — see `roadmap.md` § Codegen Optimization.
        if matches!(method, "wrapping_add" | "wrapping_sub" | "wrapping_mul")
            && matches!(
                receiver_for_lookup,
                // The 128-bit widths belong here for the same reason every
                // other width does — `eval_wrapping_arith` and codegen's
                // `build_int_{add,sub,mul}` are both width-parameterized and
                // already handle 128. They were simply missed when 128-bit
                // landed, so `m.wrapping_mul(3i128)` reported "no method
                // 'wrapping_mul' on type 'i128'" (B-2026-08-19-19).
                Type::Int(IntSize::I8 | IntSize::I16 | IntSize::I32 | IntSize::I64 | IntSize::I128,)
                    | Type::UInt(
                        UIntSize::U8
                            | UIntSize::U16
                            | UIntSize::U32
                            | UIntSize::U64
                            | UIntSize::U128
                            | UIntSize::Usize,
                    )
            )
        {
            if args.len() != 1 {
                self.type_error(
                    format!("{method} expects 1 argument, got {}", args.len()),
                    *span,
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Some(Type::Error);
            }
            // Q4 literal promotion (mirrors `infer_binary` in expr_ops.rs): a
            // suffix-free integer literal argument is promoted to the receiver
            // type, so `x.wrapping_add(1)` type-checks. Otherwise the argument
            // must match the receiver type exactly — the same strict same-type
            // rule the `+`/`-`/`*` operators enforce (mixed concrete integer
            // types are a hard error; cast with `as`).
            let arg = &args[0].value;
            let arg_ty = self.infer_expr(arg);
            if matches!(&arg.kind, ExprKind::Integer(_, None)) {
                self.record_expr_type(&arg.span, receiver_for_lookup);
                return Some(receiver_for_lookup.clone());
            }
            if arg_ty != Type::Error && arg_ty != *receiver_for_lookup {
                self.type_error(
                    format!(
                        "{method} expects an argument of type `{}`, got `{}`",
                        type_display(receiver_for_lookup),
                        type_display(&arg_ty)
                    ),
                    arg.span,
                    TypeErrorKind::TypeMismatch,
                );
                return Some(Type::Error);
            }
            return Some(receiver_for_lookup.clone());
        }
        // Euclidean division / remainder — `div_euclid` / `rem_euclid`
        // (design.md § Arithmetic Overflow, the Rust `iN::div_euclid` /
        // `rem_euclid` semantics: the remainder is always non-negative, so
        // `(-7).rem_euclid(3) == 2`). Both trap like `/` and `%` — `division by
        // zero` on a zero divisor, `integer overflow` on `iN::MIN / -1`.
        // **Scoped to `i64` in this slice** (same 64-bit-first cut as
        // `wrapping_*`): i64 is i64-backed end-to-end, so the interpreter's
        // `checked_div_euclid`/`checked_rem_euclid` and codegen's signed
        // correction agree without width-masking. Narrow signed widths and the
        // unsigned widths (where Euclidean == truncating) are a tracked
        // follow-on (`NoMethodFound` until then). Same strict same-type /
        // literal-promotion arg rule as `wrapping_*`.
        if matches!(method, "div_euclid" | "rem_euclid")
            && matches!(receiver_for_lookup, Type::Int(IntSize::I64))
        {
            if args.len() != 1 {
                self.type_error(
                    format!("{method} expects 1 argument, got {}", args.len()),
                    *span,
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Some(Type::Error);
            }
            let arg = &args[0].value;
            let arg_ty = self.infer_expr(arg);
            if matches!(&arg.kind, ExprKind::Integer(_, None)) {
                self.record_expr_type(&arg.span, receiver_for_lookup);
                self.record_expr_type(args_close_span, receiver_for_lookup);
                return Some(receiver_for_lookup.clone());
            }
            if arg_ty != Type::Error && arg_ty != *receiver_for_lookup {
                self.type_error(
                    format!(
                        "{method} expects an argument of type `{}`, got `{}`",
                        type_display(receiver_for_lookup),
                        type_display(&arg_ty)
                    ),
                    arg.span,
                    TypeErrorKind::TypeMismatch,
                );
                return Some(Type::Error);
            }
            self.record_expr_type(args_close_span, receiver_for_lookup);
            return Some(receiver_for_lookup.clone());
        }
        // Overflow-aware integer arithmetic — `{checked,saturating,overflowing}_{add,sub,mul}`
        // (design.md § Arithmetic Overflow): the explicit-overflow siblings of the
        // checked `+`/`-`/`*` path. Unlike `wrapping_*` (64-bit only), these are
        // defined on EVERY integer width: codegen is naturally width-aware (LLVM
        // overflow/saturating intrinsics on the receiver's iN/uN type), and the
        // interpreter recovers the receiver width from `expr_types` (the same
        // span→type lookup `narrow_oob` uses). Return shapes:
        //   checked_*      -> Option[Self]   (None on overflow)
        //   saturating_*   -> Self           (clamped to iN::MAX/MIN / uN::MAX/0)
        //   overflowing_*  -> (Self, bool)    (result + overflow flag)
        // Both operands and the result are the receiver's type (same strict
        // same-type / literal-promotion rule as `wrapping_*`). Backends:
        // interpreter + codegen `method_call.rs`.
        {
            let checked = matches!(method, "checked_add" | "checked_sub" | "checked_mul");
            let saturating = matches!(
                method,
                "saturating_add" | "saturating_sub" | "saturating_mul"
            );
            let overflowing = matches!(
                method,
                "overflowing_add" | "overflowing_sub" | "overflowing_mul"
            );
            if (checked || saturating || overflowing)
                && matches!(receiver_for_lookup, Type::Int(_) | Type::UInt(_))
            {
                if args.len() != 1 {
                    self.type_error(
                        format!("{method} expects 1 argument, got {}", args.len()),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    return Some(Type::Error);
                }
                let arg = &args[0].value;
                let arg_ty = self.infer_expr(arg);
                // Suffix-free integer literal arg promotes to the receiver type
                // (mirrors `wrapping_*`); otherwise it must match exactly.
                if matches!(&arg.kind, ExprKind::Integer(_, None)) {
                    self.record_expr_type(&arg.span, receiver_for_lookup);
                } else if arg_ty != Type::Error && arg_ty != *receiver_for_lookup {
                    self.type_error(
                        format!(
                            "{method} expects an argument of type `{}`, got `{}`",
                            type_display(receiver_for_lookup),
                            type_display(&arg_ty)
                        ),
                        arg.span,
                        TypeErrorKind::TypeMismatch,
                    );
                    return Some(Type::Error);
                }
                let self_ty = receiver_for_lookup.clone();
                return Some(if checked {
                    Type::Named {
                        name: "Option".to_string(),
                        args: vec![self_ty],
                    }
                } else if saturating {
                    self_ty
                } else {
                    Type::Tuple(vec![self_ty, Type::Bool])
                });
            }
        }
        // Integer `.pow(exp)` — `n.pow(k) -> Self`, the repeated-multiply power
        // (design.md § Arithmetic). The exponent is `u32` (matching Rust's
        // `iN::pow(self, exp: u32)`); a suffix-free integer-literal exponent is
        // promoted to `u32`, otherwise it must already be `u32` (cast with
        // `as u32`). Overflow TRAPS as `integer overflow` — the same app/lib
        // behavior as the `*` operator it iterates. Defined on every integer
        // width; the interpreter recovers the receiver width from the receiver
        // type stashed at `args_close_span` (the non-aliased close-paren leaf)
        // so the trap fires at the declared width. Backends: interpreter +
        // codegen `method_call.rs`.
        if method == "pow" && matches!(receiver_for_lookup, Type::Int(_) | Type::UInt(_)) {
            if args.len() != 1 {
                self.type_error(
                    format!("pow expects 1 argument, got {}", args.len()),
                    *span,
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Some(Type::Error);
            }
            let u32_ty = Type::UInt(UIntSize::U32);
            let arg = &args[0].value;
            let arg_ty = self.infer_expr(arg);
            if matches!(&arg.kind, ExprKind::Integer(_, None)) {
                self.record_expr_type(&arg.span, &u32_ty);
            } else if arg_ty != Type::Error && arg_ty != u32_ty {
                self.type_error(
                    format!(
                        "pow expects an exponent of type `u32`, got `{}` (cast with `as u32`)",
                        type_display(&arg_ty)
                    ),
                    arg.span,
                    TypeErrorKind::TypeMismatch,
                );
                return Some(Type::Error);
            }
            self.record_expr_type(args_close_span, receiver_for_lookup);
            return Some(receiver_for_lookup.clone());
        }
        // `min` / `max` on a numeric scalar — `a.min(b)` / `a.max(b)` return the
        // smaller / larger of the two (Rust's `Ord::min`/`max`, and `f64::min`/
        // `max` for floats, which are NaN-propagating-free like Rust). The arg
        // must be the same numeric type (a bare literal coerces to it). Gated on
        // a scalar receiver so it never shadows `Vec`/iterator `min`/`max`.
        if matches!(method, "min" | "max")
            && matches!(
                receiver_for_lookup,
                Type::Int(_) | Type::UInt(_) | Type::Float(_)
            )
        {
            if args.len() != 1 {
                self.type_error(
                    format!("`{method}` expects 1 argument, got {}", args.len()),
                    *span,
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Some(Type::Error);
            }
            let arg = &args[0].value;
            let arg_ty = self.infer_expr(arg);
            let is_bare_num_lit = matches!(
                &arg.kind,
                ExprKind::Integer(_, None) | ExprKind::Float(_, None)
            );
            if is_bare_num_lit {
                self.record_expr_type(&arg.span, receiver_for_lookup);
            } else if arg_ty != Type::Error && arg_ty != *receiver_for_lookup {
                self.type_error(
                    format!(
                        "`{method}` expects an argument of type `{}`, got `{}`",
                        type_display(receiver_for_lookup),
                        type_display(&arg_ty)
                    ),
                    arg.span,
                    TypeErrorKind::TypeMismatch,
                );
                return Some(Type::Error);
            }
            self.record_expr_type(args_close_span, receiver_for_lookup);
            return Some(receiver_for_lookup.clone());
        }
        // `clamp` on a numeric scalar — `v.clamp(lo, hi)` pins `v` into the
        // inclusive range `[lo, hi]`, the method sibling of the `clamp[T: Ord]`
        // free fn (ordering.kara). Same nested-bound semantics: `v < lo → lo`,
        // else `v > hi → hi`, else `v` (so `lo` wins on an inverted range).
        // Both bounds must be the receiver type (bare literals coerce). Gated
        // on a scalar receiver so it never shadows a user/collection `clamp`.
        if method == "clamp"
            && matches!(
                receiver_for_lookup,
                Type::Int(_) | Type::UInt(_) | Type::Float(_)
            )
        {
            if args.len() != 2 {
                self.type_error(
                    format!("`clamp` expects 2 arguments, got {}", args.len()),
                    *span,
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Some(Type::Error);
            }
            for arg in args.iter() {
                let arg = &arg.value;
                let arg_ty = self.infer_expr(arg);
                let is_bare_num_lit = matches!(
                    &arg.kind,
                    ExprKind::Integer(_, None) | ExprKind::Float(_, None)
                );
                if is_bare_num_lit {
                    self.record_expr_type(&arg.span, receiver_for_lookup);
                } else if arg_ty != Type::Error && arg_ty != *receiver_for_lookup {
                    self.type_error(
                        format!(
                            "`clamp` expects an argument of type `{}`, got `{}`",
                            type_display(receiver_for_lookup),
                            type_display(&arg_ty)
                        ),
                        arg.span,
                        TypeErrorKind::TypeMismatch,
                    );
                    return Some(Type::Error);
                }
            }
            self.record_expr_type(args_close_span, receiver_for_lookup);
            return Some(receiver_for_lookup.clone());
        }
        // Bit intrinsics on integer scalars — `count_ones` / `leading_zeros` /
        // `trailing_zeros` -> u32 (Rust's `iN::{count_ones,leading_zeros,
        // trailing_zeros}`). All width-dependent: `leading_zeros` / `trailing_zeros`
        // count within the receiver's bit width, and `count_ones` over its `bits`
        // low bits (a signed `iN`'s sign-extended interpreter representation is
        // masked to width first). The `u32` result differs from the receiver, so
        // the generic `infer_expr` post-record clobbers `expr_types[receiver.span]`
        // — the interpreter reads the receiver type stashed at the non-aliased
        // `args_close_span` leaf instead. Effect-free; codegen lowers to the
        // overloaded `llvm.ctpop` / `llvm.ctlz` / `llvm.cttz` intrinsics.
        if args.is_empty()
            && matches!(receiver_for_lookup, Type::Int(_) | Type::UInt(_))
            && matches!(
                method,
                "count_ones" | "count_zeros" | "leading_zeros" | "trailing_zeros"
            )
        {
            self.record_expr_type(args_close_span, receiver_for_lookup);
            return Some(Type::UInt(UIntSize::U32));
        }
        // `is_power_of_two` on unsigned integer scalars -> bool (Rust's
        // `uN::is_power_of_two`; unsigned-only, since power-of-two-ness is
        // meaningless for a signed/negative value). The bool result differs from
        // the receiver, so the receiver type is stashed at `args_close_span` for
        // the interpreter to recover the width (it masks the stored value to
        // width before the single-bit test). Effect-free; codegen lowers to the
        // inline `(x != 0) & ((x & (x-1)) == 0)`.
        if args.is_empty()
            && matches!(receiver_for_lookup, Type::UInt(_))
            && method == "is_power_of_two"
        {
            self.record_expr_type(args_close_span, receiver_for_lookup);
            return Some(Type::Bool);
        }
        // `next_power_of_two` on unsigned integer scalars -> Self (Rust's
        // `uN::next_power_of_two`; unsigned-only). The smallest power of two ≥
        // self (0 and 1 both → 1). TRAPS `integer overflow` when the result
        // would exceed the width (`self > 2^(bits-1)`), matching the `*`/`pow`
        // trap policy. The Self result keeps the receiver span's type; the
        // interpreter recovers the width from `args_close_span`. Effect-free;
        // codegen lowers via `llvm.ctlz` + a shift with an overflow-trap branch.
        if args.is_empty()
            && matches!(receiver_for_lookup, Type::UInt(_))
            && method == "next_power_of_two"
        {
            self.record_expr_type(args_close_span, receiver_for_lookup);
            return Some(receiver_for_lookup.clone());
        }
        // `abs_diff(self, other) -> unsigned sibling` (Rust `iN/uN::abs_diff`):
        // the absolute difference of two same-type integers ALWAYS fits the
        // unsigned type of the same width (`i8::MIN.abs_diff(i8::MAX) == 255u8`),
        // so it never overflows and never traps. The result type differs from
        // the receiver (signed → unsigned sibling; unsigned → itself), so the
        // receiver type is stashed at `args_close_span` for the interpreter's
        // width recovery. Effect-free; codegen lowers to `select(a≥b, a-b, b-a)`
        // (signed/unsigned compare per receiver signedness) then zero-extends the
        // iN magnitude to the i64-backed representation.
        if method == "abs_diff" && matches!(receiver_for_lookup, Type::Int(_) | Type::UInt(_)) {
            if args.len() != 1 {
                self.type_error(
                    format!("abs_diff expects 1 argument, got {}", args.len()),
                    *span,
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Some(Type::Error);
            }
            let arg = &args[0].value;
            let arg_ty = self.infer_expr(arg);
            // A suffix-free integer literal arg promotes to the receiver type;
            // otherwise it must match exactly (mirrors `checked_*`).
            if matches!(&arg.kind, ExprKind::Integer(_, None)) {
                self.record_expr_type(&arg.span, receiver_for_lookup);
            } else if arg_ty != Type::Error && arg_ty != *receiver_for_lookup {
                self.type_error(
                    format!(
                        "abs_diff expects an argument of type `{}`, got `{}`",
                        type_display(receiver_for_lookup),
                        type_display(&arg_ty)
                    ),
                    arg.span,
                    TypeErrorKind::TypeMismatch,
                );
                return Some(Type::Error);
            }
            self.record_expr_type(args_close_span, receiver_for_lookup);
            return Some(match receiver_for_lookup {
                Type::Int(IntSize::I8) => Type::UInt(UIntSize::U8),
                Type::Int(IntSize::I16) => Type::UInt(UIntSize::U16),
                Type::Int(IntSize::I32) => Type::UInt(UIntSize::U32),
                Type::Int(IntSize::I64) => Type::UInt(UIntSize::U64),
                Type::Int(IntSize::I128) => Type::UInt(UIntSize::U128),
                // Already unsigned — `abs_diff` returns the same unsigned type.
                other => other.clone(),
            });
        }
        // Bit-permutation intrinsics on integer scalars — `reverse_bits` /
        // `swap_bytes` -> Self (Rust's `iN::{reverse_bits,swap_bytes}`). Both
        // are width-dependent (they permute within the receiver's `bits`), so
        // the `Self` result means the receiver span keeps its type; the
        // interpreter recovers the width from `args_close_span` like the count
        // family. Effect-free; codegen lowers to `llvm.bitreverse` / `llvm.bswap`
        // on the receiver's iN type.
        if args.is_empty()
            && matches!(receiver_for_lookup, Type::Int(_) | Type::UInt(_))
            && matches!(method, "reverse_bits" | "swap_bytes")
        {
            self.record_expr_type(args_close_span, receiver_for_lookup);
            return Some(receiver_for_lookup.clone());
        }
        // Bit-rotation intrinsics on integer scalars — `rotate_left(n)` /
        // `rotate_right(n)` -> Self (Rust's `iN::rotate_{left,right}`, `n: u32`).
        // Width-dependent: the rotation wraps within the receiver's `bits`
        // (`n` is taken mod `bits`). The `Self` result keeps the receiver span's
        // type; the interpreter recovers the width from `args_close_span`.
        // Codegen lowers to `llvm.fshl` / `llvm.fshr` on the receiver's iN.
        if matches!(method, "rotate_left" | "rotate_right")
            && matches!(receiver_for_lookup, Type::Int(_) | Type::UInt(_))
        {
            if args.len() != 1 {
                self.type_error(
                    format!(
                        "`{method}` expects 1 argument (the rotation amount), got {}",
                        args.len()
                    ),
                    *span,
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Some(Type::Error);
            }
            let arg = &args[0].value;
            let arg_ty = self.infer_expr(arg);
            // The amount is `u32`; a suffix-free integer literal promotes.
            if matches!(&arg.kind, ExprKind::Integer(_, None)) {
                self.record_expr_type(&arg.span, &Type::UInt(UIntSize::U32));
            } else if arg_ty != Type::Error && !matches!(arg_ty, Type::Int(_) | Type::UInt(_)) {
                self.type_error(
                    format!(
                        "`{method}` expects an integer rotation amount, got `{}`",
                        type_display(&arg_ty)
                    ),
                    arg.span,
                    TypeErrorKind::TypeMismatch,
                );
                return Some(Type::Error);
            }
            self.record_expr_type(args_close_span, receiver_for_lookup);
            return Some(receiver_for_lookup.clone());
        }
        // Built-in `clone` / `to_string` on the scalar numeric + bool + char
        // primitives (all `Copy`). `clone` is identity → `Self`; `to_string`
        // renders the value → `String` (`Type::Str`). Like `abs`, these are
        // dedicated value-receiver methods (the registered builtin impls model
        // the type-receiver/operator form). Backends: interpreter clones the
        // `Value` / formats via `Display`; codegen returns the scalar
        // unchanged / builds an owning `String` from the f-string renderer.
        // `String`/struct receivers are left to their existing paths (not
        // matched here — `Type::Str` and `Type::Named` are excluded).
        if args.is_empty()
            && matches!(
                receiver_for_lookup,
                Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char
            )
        {
            if method == "clone" {
                return Some(receiver_for_lookup.clone());
            }
            if method == "to_string" {
                return Some(Type::Str);
            }
        }
        // Unicode `char` classification predicates (phase-12 #13):
        // `char.is_alphabetic()` / `is_numeric()` / `is_alphanumeric()` /
        // `is_whitespace()` → bool. The Unicode-aware companions of the
        // `u8.is_ascii_*` byte predicates; backed by interp (`char` methods) and
        // codegen (`karac_runtime_char_is_*` externs). Restricted to a `char`
        // receiver — the ASCII predicates stay on the byte/integer scalars.
        if args.is_empty()
            && matches!(receiver_for_lookup, Type::Char)
            && matches!(
                method,
                "is_alphabetic"
                    | "is_numeric"
                    | "is_alphanumeric"
                    | "is_whitespace"
                    | "is_uppercase"
                    | "is_lowercase"
                    | "is_ascii"
            )
        {
            return Some(Type::Bool);
        }
        // ASCII case folding on a `char` — `to_ascii_uppercase` /
        // `to_ascii_lowercase` -> char (Rust's `char::to_ascii_*case`): only the
        // ASCII letters `a`..`z` / `A`..`Z` are mapped, every other codepoint
        // (incl. non-ASCII) is returned unchanged. Unlike the Unicode
        // `to_uppercase` (which yields an *iterator* — `ß` → `SS`), the ASCII
        // form is a pure char→char map, so it lowers to inline codepoint
        // arithmetic in codegen (no Unicode tables). Char-only.
        if args.is_empty()
            && matches!(receiver_for_lookup, Type::Char)
            && matches!(method, "to_ascii_uppercase" | "to_ascii_lowercase")
        {
            return Some(Type::Char);
        }
        // Unicode case folding on a `char` — `to_lowercase` / `to_uppercase`
        // -> char (B-2026-08-12-25). These are the names a writer reaches for
        // first (`to_ascii_lowercase` reads as the narrowing special case), so
        // their absence sent case-folding code hunting a surface that had only
        // the qualified spelling.
        //
        // THE SPEC DECISION the row flagged: a scalar can case-fold to SEVERAL
        // scalars (`ß` → `SS`), which is why Rust's `char::to_uppercase` returns
        // an iterator. Kāra returns `char` and applies the full mapping only
        // when it yields exactly one scalar, leaving `self` unchanged when it
        // expands — the same rule as Go's `unicode.ToLower` and Java's
        // `Character.toLowerCase(char)`. Full mapping is not lost: it is what
        // `String.to_uppercase()` already does, so `c.to_string().to_uppercase()`
        // renders `SS`. Backed by the `karac_runtime_char_to_*case` externs in
        // codegen and Rust's own iterator in interp, collapsed identically.
        //
        // Char-only: the `String` receiver's `to_lowercase`/`to_uppercase` are
        // the String→String transforms typed in `stdlib_seq.rs`, and they keep
        // that path because this arm requires `Type::Char`.
        if args.is_empty()
            && matches!(receiver_for_lookup, Type::Char)
            && matches!(method, "to_uppercase" | "to_lowercase")
        {
            return Some(Type::Char);
        }
        // `char.to_digit(radix) -> Option[u32]` (Rust's `char::to_digit`): the
        // numeric value of `self` as a digit in `radix`, `None` if `self` is not
        // a digit in that radix. `radix` is `u32` (a suffix-free literal
        // promotes); an out-of-range radix (`< 2` or `> 36`) traps at run time,
        // matching Rust's panic. Complete on BOTH backends: the interpreter uses
        // Rust's `char::to_digit`, codegen classifies the codepoint inline and
        // wraps the result with the `checked_to_*` Option constructor
        // (`build_checked_to_int_option`, codegen/method_call.rs). The older
        // "codegen emits a not-yet-supported error" note here outlived the
        // lowering that landed it; corrected while wiring B-2026-08-11-2, whose
        // `is_digit` diagnostic points authors at this method.
        //
        // `char.is_digit(radix) -> bool` (B-2026-08-12-25) rides the same arm:
        // identical receiver, identical radix rules, identical trap — it is
        // `to_digit(radix).is_some()`, and Rust spells it the same way. Sharing
        // the arm is what keeps the two from drifting apart on the radix.
        if matches!(method, "to_digit" | "is_digit") && matches!(receiver_for_lookup, Type::Char) {
            if args.len() != 1 {
                let mut msg = format!("{method} expects 1 argument, got {}", args.len());
                // The bare `c.is_digit()` is the spelling a writer reaches for
                // (Rust's radix argument is the surprise, not the method), so
                // name both routes rather than leaving an arity count to be
                // decoded. This replaces the `no method 'is_digit'` hint added
                // by B-2026-08-11-2 — the method exists now, so the miss is an
                // arity miss and lands here instead.
                if method == "is_digit" && args.is_empty() {
                    msg.push_str(
                        ": write `is_digit(10)` for decimal, or `is_numeric()` \
                         for the Unicode predicate",
                    );
                }
                self.type_error(msg, *span, TypeErrorKind::WrongNumberOfArgs);
                return Some(Type::Error);
            }
            let u32_ty = Type::UInt(UIntSize::U32);
            let arg = &args[0].value;
            let arg_ty = self.infer_expr(arg);
            if matches!(&arg.kind, ExprKind::Integer(_, None)) {
                self.record_expr_type(&arg.span, &u32_ty);
            } else if arg_ty != Type::Error && arg_ty != u32_ty {
                self.type_error(
                    format!(
                        "{method} expects a radix of type `u32`, got `{}` (cast with `as u32`)",
                        type_display(&arg_ty)
                    ),
                    arg.span,
                    TypeErrorKind::TypeMismatch,
                );
                return Some(Type::Error);
            }
            if method == "is_digit" {
                return Some(Type::Bool);
            }
            return Some(Type::Named {
                name: "Option".to_string(),
                args: vec![u32_ty],
            });
        }
        None
    }
}
