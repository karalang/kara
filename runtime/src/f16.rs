//! IEEE-754 half-precision conversion builtins for the wasm archives
//! (B-2026-08-30-24).
//!
//! LLVM legalizes every `half` operation on wasm32 into a pair of
//! libcalls — `__truncsfhf2` (f32 → f16 bits) and `__extendhfsf2` (f16
//! bits → f32) — and then expects the link to supply them, exactly as it
//! does on a native target. It does not on wasm: the two symbols live in
//! compiler-rt, and neither Rust's `compiler_builtins` rlib for
//! `wasm32-wasip1` nor wasi-libc's `libc.a` carries them (measured with
//! `llvm-nm` on both). There is no `libclang_rt.builtins-wasm32.a` in the
//! rustup self-contained sysroot to add to the link line either, so the
//! only way to supply them is to define them — which is what this module
//! does. Before it, ANY f16 program failed `karac build --target=wasm_*`
//! with a raw `rust-lld: undefined symbol` dump; a bare runtime
//! `as f16` conversion was enough, since printing one needs the widening
//! call. `bf16` was unaffected, because codegen emits its conversions as
//! integer shifts and never asks the backend for a `bfloat` node at all.
//!
//! THE ABI IS NOT GUESSED. It is read out of the object LLVM emits: for
//! `wasm32-unknown-wasi` the type section declares `__truncsfhf2` as
//! `(f32) -> i32` and `__extendhfsf2` as `(i32) -> f32`, i.e. the half
//! travels as its 16-bit pattern widened into an i32, and a `half`
//! function ARGUMENT arrives already widened to f32 (an `fpext half to
//! float` on wasm compiles to nothing at all). Note this differs from the
//! x86-64 host, where `__extendhfsf2` takes `_Float16` in an SSE
//! register — a fact worth recording, because it means a native harness
//! cannot call the host builtin to cross-check this direction.
//!
//! THE ROUNDING IS THE INTERPRETER'S, DELIBERATELY. `f32_to_f16_bits` is
//! the same algorithm as `src/interpreter/eval_expr.rs`'s function of
//! that name, so a value narrowed to f16 lands on identical bits whether
//! it went through the tree-walk, a native binary, or a wasm module.
//! That is not asserted by convention: the algorithm was compared against
//! the host's compiler-rt `__truncsfhf2` over ALL 2^32 f32 bit patterns,
//! NaN payloads included, with zero differences — so agreeing with it is
//! the same thing as agreeing with every native target.
//!
//! Native builds keep the platform's own builtins; the `#[no_mangle]`
//! exports below are wasm-gated so this can never collide with them. The
//! pure functions stay compiled on native under `cfg(test)` so the
//! conversion is unit-testable without a wasm host — the same split
//! `seq_scheduler` uses.

/// f32 → f16 bit pattern, round-to-nearest-even, handling
/// Inf / NaN / overflow / subnormals / underflow.
///
/// Kept byte-identical to `src/interpreter/eval_expr.rs`'s
/// `f32_to_f16_bits`; see the module doc for the exhaustive comparison
/// that pins both to compiler-rt.
#[cfg(any(target_family = "wasm", test))]
pub(crate) fn f32_to_f16_bits(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xFF) as i32;
    let frac = b & 0x007F_FFFF;
    if exp == 0xFF {
        // Inf / NaN (quiet the NaN, keep the top payload bits).
        return if frac != 0 {
            sign | 0x7E00 | ((frac >> 13) as u16)
        } else {
            sign | 0x7C00
        };
    }
    if exp == 0 && frac == 0 {
        return sign;
    }
    let e = exp - 127;
    if e >= 16 {
        return sign | 0x7C00; // ≥ 2^16 → Inf
    }
    if e >= -14 {
        // Normal f16 range: 24-bit significand → 11 bits, RNE on the cut.
        let mant = 0x0080_0000 | frac;
        let base = mant >> 13;
        let rem = mant & 0x1FFF;
        let mut r = base;
        if rem > 0x1000 || (rem == 0x1000 && (base & 1) == 1) {
            r += 1;
        }
        let mut ee = (e + 15) as u32;
        let mut mm = r & 0x3FF;
        if r == 0x800 {
            // Mantissa carry (0x7FF+1): bump the exponent.
            ee += 1;
            mm = 0;
        }
        if ee >= 31 {
            return sign | 0x7C00; // rounded past 65504 → Inf
        }
        return sign | ((ee as u16) << 10) | (mm as u16);
    }
    if e < -25 {
        return sign; // below half the smallest subnormal → ±0
    }
    // Subnormal f16: shift the significand into the subnormal position,
    // RNE on the cut. (`r` reaching 0x400 IS the smallest normal — the
    // bit pattern composes correctly.)
    let mant = 0x0080_0000u32 | frac;
    let shift = (13 + (-14 - e)) as u32;
    let base = mant >> shift;
    let rem = mant & ((1u32 << shift) - 1);
    let half = 1u32 << (shift - 1);
    let mut r = base as u16;
    if rem > half || (rem == half && (r & 1) == 1) {
        r += 1;
    }
    sign | r
}

/// f16 bit pattern → f32. Exact in every case: f32 covers f16's whole
/// exponent range, so even an f16 subnormal is an ordinary f32 normal
/// and no rounding is possible.
///
/// One corner is deliberately left alone: a SIGNALLING NaN's payload is
/// carried across unchanged rather than quieted. The narrowing direction
/// does quiet, so a round trip still lands on a quiet NaN, and Kāra has
/// no way to spell a signalling f16 in the first place — the tests
/// therefore assert NaN-ness rather than the payload bits.
#[cfg(any(target_family = "wasm", test))]
pub(crate) fn f16_bits_to_f32(h: u16) -> f32 {
    let sign = ((h as u32) & 0x8000) << 16;
    let exp = ((h >> 10) & 0x1F) as u32;
    let frac = (h as u32) & 0x03FF;
    if exp == 0x1F {
        // Inf / NaN — re-seat the payload in f32's wider field.
        return f32::from_bits(sign | 0x7F80_0000 | (frac << 13));
    }
    if exp == 0 {
        if frac == 0 {
            return f32::from_bits(sign); // ±0
        }
        // Subnormal half → normal float: an f16 subnormal is
        // `frac * 2^-24`, and f32's exponent range covers that with room
        // to spare, so it renormalizes to an ordinary f32 normal. With
        // `k` the index of the leading set bit of `frac`, the value is
        // `(1 + g/2^k) * 2^(k-24)` for `g = frac - 2^k`, which gives the
        // biased exponent `k - 24 + 127` and the mantissa `g << (23-k)`.
        let k = 31 - frac.leading_zeros(); // 0..=9
        let g = frac - (1 << k);
        return f32::from_bits(sign | ((k + 103) << 23) | (g << (23 - k)));
    }
    f32::from_bits(sign | ((exp + 127 - 15) << 23) | (frac << 13))
}

/// compiler-rt's `__truncsfhf2` for wasm: f32 → the f16 bit pattern,
/// returned in an i32 (see the module doc for where that ABI is read
/// from).
#[cfg(target_family = "wasm")]
#[no_mangle]
pub extern "C" fn __truncsfhf2(x: f32) -> u32 {
    f32_to_f16_bits(x) as u32
}

/// compiler-rt's `__extendhfsf2` for wasm: the f16 bit pattern in an i32
/// → f32. The mask is deliberate: the C prototype takes a 16-bit value
/// and the wasm ABI widens it to i32, so the high half is not ours to
/// trust.
#[cfg(target_family = "wasm")]
#[no_mangle]
pub extern "C" fn __extendhfsf2(h: u32) -> f32 {
    f16_bits_to_f32((h & 0xFFFF) as u16)
}

#[cfg(test)]
mod tests {
    use super::{f16_bits_to_f32, f32_to_f16_bits};

    /// An f16 bit pattern's value, derived by ARITHMETIC rather than by
    /// bit-shuffling — deliberately a different derivation from
    /// [`f16_bits_to_f32`], so the exhaustive comparison below is a real
    /// cross-check and not a restatement. Mirrors the interpreter's
    /// `f16_bits_to_f64`.
    fn value_of(h: u16) -> f64 {
        let neg = h & 0x8000 != 0;
        let exp = ((h >> 10) & 0x1F) as i32;
        let mant = (h & 0x3FF) as f64;
        let v = if exp == 31 {
            if mant != 0.0 {
                f64::NAN
            } else {
                f64::INFINITY
            }
        } else if exp == 0 {
            mant * (2.0f64).powi(-24)
        } else {
            (1.0 + mant / 1024.0) * (2.0f64).powi(exp - 15)
        };
        if neg {
            -v
        } else {
            v
        }
    }

    #[test]
    fn extend_is_exact_for_every_f16_bit_pattern() {
        // All 65536 patterns, which is the whole domain — there is no
        // sampling question here. The subnormal quarter is the half that
        // actually needs the test: a first draft renormalized them with
        // the exponent and the mantissa shift each off by one, and it
        // was right on all 63490 normal/Inf/NaN patterns while being
        // wrong on all 2046 subnormal ones.
        for hb in 0u32..=0xFFFF {
            let h = hb as u16;
            let got = f16_bits_to_f32(h);
            let want = value_of(h);
            if want.is_nan() {
                assert!(got.is_nan(), "{h:#06x} should be NaN, got {got}");
                continue;
            }
            assert_eq!(got as f64, want, "{h:#06x} widened wrong");
            if want == 0.0 {
                assert_eq!(
                    got.is_sign_negative(),
                    h & 0x8000 != 0,
                    "{h:#06x} lost its zero sign"
                );
            }
        }
    }

    #[test]
    fn narrowing_a_widened_f16_returns_the_same_bits() {
        // The two directions are inverses on the whole domain: widening
        // is exact, so narrowing must land back on the identical pattern
        // for every non-NaN input, negative zero and subnormals included.
        for hb in 0u32..=0xFFFF {
            let h = hb as u16;
            if (h & 0x7C00) == 0x7C00 && (h & 0x03FF) != 0 {
                continue; // NaN: payload handling is asserted separately
            }
            assert_eq!(
                f32_to_f16_bits(f16_bits_to_f32(h)),
                h,
                "round trip {h:#06x}"
            );
        }
    }

    #[test]
    fn narrowing_rounds_to_nearest_even_at_the_hard_boundaries() {
        // The cases a round-trip test cannot reach, because they are the
        // f32 values that are NOT f16-representable: ties, the overflow
        // edge, and the two ends of the subnormal range.
        //
        // Every value is built from exact binary fractions rather than
        // written as decimal digits. Decimal spellings of these would be
        // both unreadable (2^-25 is 2.9802322387695312e-8) and fragile —
        // the reader cannot tell by eye whether the literal lands on the
        // tie or one ulp off it, which is the whole question here.
        let ulp1 = 1.0f32 / 1024.0; // f16's ulp at 1.0 is 2^-10
        let sub_min = 1.0f32 / 16_777_216.0; // 2^-24, smallest f16 subnormal
        let half_sub = sub_min / 2.0; // 2^-25, exactly the tie down to zero
        let cases: &[(f32, u16, &str)] = &[
            (1.0, 0x3C00, "one"),
            (-2.0, 0xC000, "minus two"),
            (65504.0, 0x7BFF, "largest finite f16"),
            (65520.0, 0x7C00, "first f32 that rounds up to Inf"),
            (65519.0, 0x7BFF, "just under that, stays finite"),
            (1.0 + ulp1, 0x3C01, "one ulp above one"),
            // Ties land on the even neighbour, in both directions.
            (1.0 + ulp1 / 2.0, 0x3C00, "tie below, rounds to even (down)"),
            (1.0 + ulp1 * 1.5, 0x3C02, "tie above, rounds to even (up)"),
            (sub_min * 1023.0, 0x03FF, "largest subnormal"),
            (sub_min, 0x0001, "smallest subnormal"),
            (
                half_sub,
                0x0000,
                "exactly the tie to zero: rounds to even zero",
            ),
            (
                f32::from_bits(half_sub.to_bits() + 1),
                0x0001,
                "one f32 ulp above that tie: rounds up instead",
            ),
            (-0.0, 0x8000, "negative zero keeps its sign"),
            (f32::INFINITY, 0x7C00, "infinity"),
            (f32::NEG_INFINITY, 0xFC00, "negative infinity"),
        ];
        for (x, want, why) in cases {
            assert_eq!(f32_to_f16_bits(*x), *want, "{why}: {x}");
        }
        // NaN stays NaN and is quieted; the payload is not asserted bit
        // for bit, only that it does not collapse to Inf.
        let nan = f32_to_f16_bits(f32::NAN);
        assert_eq!(nan & 0x7C00, 0x7C00, "NaN keeps the all-ones exponent");
        assert_ne!(nan & 0x03FF, 0, "NaN must not narrow to Inf");
    }
}
