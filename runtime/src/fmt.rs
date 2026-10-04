//! f-string format-spec runtime formatter — the shared-renderer path for
//! the specifiers `snprintf` can't express (binary `b`, center align `^`,
//! and custom fill chars). Phase-8 stdlib floor follow-up.
//!
//! ## Why this exists
//!
//! Codegen renders most `f"{x:spec}"` holes inline via `snprintf`
//! (`FormatSpec::to_printf` → `%04lld` etc.), which matches the
//! interpreter's `apply_*` byte-for-byte for the printf-expressible subset.
//! But printf has no binary conversion, no center alignment, and no custom
//! (non-space) fill char. For those three, codegen instead compiles the raw
//! spec string + the value into a call to one of the `karac_runtime_fmt_*`
//! entrypoints below, which parse the spec and render it through the SAME
//! `crate::format_spec::FormatSpec::apply_*` helpers the interpreter calls.
//! One renderer, one source of truth → `karac run` == `karac build`.
//!
//! ## Single source of truth
//!
//! `format_spec.rs` lives in the compiler crate (the interpreter needs it in
//! non-`llvm` builds, where `karac-runtime` isn't even a dependency). It is
//! freestanding — std-only, no `use crate::` — so we compile the *same file*
//! into the runtime crate via `#[path]` rather than duplicate it. Editing the
//! parser or an `apply_*` helper updates both the interpreter and this
//! formatter at once. `#[allow(dead_code)]`: the runtime uses `apply_*` and
//! `parse` but not the codegen-only `to_printf` / `int_conv`.
//!
//! ## ABI
//!
//! Each entrypoint takes the raw spec bytes (`spec_ptr` / `spec_len`), the
//! value, and a caller-provided output buffer (`out_buf` / `out_cap`). It
//! writes the rendered UTF-8 bytes into the buffer and returns the byte
//! length written (never exceeding `out_cap`). Codegen sizes `out_buf` to the
//! spec's guaranteed maximum output (`max(width, 72)` for numerics; `width`
//! for the string pad branch, which it only enters when the source is
//! shorter than `width`), so no truncation occurs in practice; the `out_cap`
//! bound is a hard safety net against overflow regardless.

#[allow(dead_code)]
#[path = "../../src/format_spec.rs"]
mod format_spec;

use format_spec::FormatSpec;

/// Parse the raw spec bytes into a `FormatSpec`, falling back to the default
/// (no-op) spec if the bytes are somehow invalid. The typechecker validated
/// the spec at compile time, so the fallback never fires in practice — it
/// just keeps the runtime total rather than panicking across the FFI edge.
unsafe fn parse_spec(spec_ptr: *const u8, spec_len: i64) -> FormatSpec {
    unsafe {
        let default = || FormatSpec {
            fill: None,
            align: None,
            plus: false,
            zero_pad: false,
            width: None,
            precision: None,
            radix: format_spec::Radix::Dec,
        };
        if spec_ptr.is_null() || spec_len < 0 {
            return default();
        }
        let raw = std::slice::from_raw_parts(spec_ptr, spec_len as usize);
        match std::str::from_utf8(raw) {
            Ok(s) => FormatSpec::parse(s).unwrap_or_else(|_| default()),
            Err(_) => default(),
        }
    }
}

/// Copy `s`'s bytes into `out_buf` bounded by `out_cap`, returning the number
/// of bytes written.
unsafe fn write_out(s: &str, out_buf: *mut u8, out_cap: i64) -> i64 {
    unsafe {
        if out_buf.is_null() || out_cap <= 0 {
            return 0;
        }
        let bytes = s.as_bytes();
        let n = std::cmp::min(bytes.len(), out_cap as usize);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, n);
        n as i64
    }
}

/// Render an integer hole. `is_unsigned != 0` selects `apply_uint128` (never
/// negative, and the value's bits are the magnitude); otherwise
/// `apply_int128`.
///
/// **The value arrives as two 64-bit WORDS**, little-endian, for the same
/// reason [`crate::karac_runtime_int_fmt`] and `karac_runtime_i128_to_str`
/// take it that way: passing an `i128` across the C ABI is not uniformly
/// defined across this runtime's targets. A single `i64` here was the
/// B-2026-09-07-45 defect — codegen handed a 128-bit value to an `i64`
/// parameter and LLVM's verifier rejected the module, so `f"{x:^44}"`,
/// `f"{x:b}"` and `f"{x:*>44}"` on a `u128`/`i128` FAILED TO COMPILE. That is
/// the same failure B-2026-09-07-34 fixed on the pre-decoded fast path; this
/// is its spec-re-parsing sibling, which that fix did not reach.
///
/// **The caller owns the extension, and it is keyed on RADIX as well as
/// signedness.** A non-decimal radix reinterprets the value at the HOLE'S OWN
/// width — `{-1i64:b}` is sixty-four ones, not a hundred and twenty-eight — so
/// a narrower hole ZERO-extends there and only decimal sign-extends. Codegen
/// applies exactly that rule before splitting the words (see
/// `compile_fstr_part_spec_runtime`); rendering here is then width-agnostic.
///
/// # Safety
///
/// `spec_ptr`/`out_buf` must satisfy the ABI in the module doc.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_fmt_int(
    spec_ptr: *const u8,
    spec_len: i64,
    lo: u64,
    hi: u64,
    is_unsigned: i32,
    out_buf: *mut u8,
    out_cap: i64,
) -> i64 {
    unsafe {
        let fs = parse_spec(spec_ptr, spec_len);
        let raw: u128 = (u128::from(hi) << 64) | u128::from(lo);
        let rendered = if is_unsigned != 0 {
            fs.apply_uint128(raw)
        } else {
            fs.apply_int128(raw as i128)
        };
        write_out(&rendered, out_buf, out_cap)
    }
}

/// Render a float hole (`apply_float`).
///
/// # Safety
///
/// `spec_ptr`/`out_buf` must satisfy the ABI in the module doc.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_fmt_float(
    spec_ptr: *const u8,
    spec_len: i64,
    value: f64,
    out_buf: *mut u8,
    out_cap: i64,
) -> i64 {
    unsafe {
        let fs = parse_spec(spec_ptr, spec_len);
        let rendered = fs.apply_float(value);
        write_out(&rendered, out_buf, out_cap)
    }
}

/// Render a string hole (`apply_str` — width padding + align + fill). The
/// source bytes are borrowed read-only; codegen only calls this on the
/// pad branch (source shorter than `width`), so the result fits in a
/// `width`-sized buffer.
///
/// # Safety
///
/// `spec_ptr`/`s_ptr`/`out_buf` must satisfy the ABI in the module doc.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_fmt_str(
    spec_ptr: *const u8,
    spec_len: i64,
    s_ptr: *const u8,
    s_len: i64,
    out_buf: *mut u8,
    out_cap: i64,
) -> i64 {
    unsafe {
        let fs = parse_spec(spec_ptr, spec_len);
        let s = if s_ptr.is_null() || s_len < 0 {
            ""
        } else {
            let bytes = std::slice::from_raw_parts(s_ptr, s_len as usize);
            std::str::from_utf8(bytes).unwrap_or("")
        };
        let rendered = fs.apply_str(s);
        write_out(&rendered, out_buf, out_cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Call the entrypoint with a 128-bit raw value already split into words.
    unsafe fn call_raw(spec: &str, raw: u128, unsigned: bool) -> String {
        unsafe {
            let mut buf = [0u8; 256];
            let n = karac_runtime_fmt_int(
                spec.as_ptr(),
                spec.len() as i64,
                raw as u64,
                (raw >> 64) as u64,
                unsigned as i32,
                buf.as_mut_ptr(),
                buf.len() as i64,
            );
            String::from_utf8(buf[..n as usize].to_vec()).unwrap()
        }
    }

    /// A 64-bit hole, widened the way CODEGEN widens one before the call:
    /// ZERO-extend for an unsigned hole or any NON-DECIMAL radix (which
    /// reinterprets at the hole's own width, so `{-1:b}` must stay sixty-four
    /// ones), SIGN-extend only for a signed decimal one. Getting this rule
    /// wrong here would hide getting it wrong in codegen, so it is written the
    /// same way in both places.
    unsafe fn call_int(spec: &str, v: i64, unsigned: bool) -> String {
        let dec = FormatSpec::parse(spec)
            .map(|f| f.radix == super::format_spec::Radix::Dec)
            .unwrap_or(true);
        let raw: u128 = if unsigned || !dec {
            u128::from(v as u64)
        } else {
            v as i128 as u128
        };
        unsafe { call_raw(spec, raw, unsigned) }
    }
    unsafe fn call_float(spec: &str, v: f64) -> String {
        unsafe {
            let mut buf = [0u8; 128];
            let n = karac_runtime_fmt_float(
                spec.as_ptr(),
                spec.len() as i64,
                v,
                buf.as_mut_ptr(),
                buf.len() as i64,
            );
            String::from_utf8(buf[..n as usize].to_vec()).unwrap()
        }
    }
    unsafe fn call_str(spec: &str, s: &str) -> String {
        unsafe {
            let mut buf = [0u8; 128];
            let n = karac_runtime_fmt_str(
                spec.as_ptr(),
                spec.len() as i64,
                s.as_ptr(),
                s.len() as i64,
                buf.as_mut_ptr(),
                buf.len() as i64,
            );
            String::from_utf8(buf[..n as usize].to_vec()).unwrap()
        }
    }

    #[test]
    fn runtime_matches_apply_helpers() {
        unsafe {
            // Binary radix (the printf-inexpressible case).
            assert_eq!(call_int("b", 5, false), "101");
            assert_eq!(call_int("08b", 5, false), "00000101");
            assert_eq!(call_int("b", 255, true), "11111111");
            // Center align.
            assert_eq!(call_int("^6", 42, false), "  42  ");
            assert_eq!(call_str("^7", "hi"), "  hi   ");
            assert_eq!(call_float("^8.2", 1.5), "  1.50  ");
            // Custom fill.
            assert_eq!(call_str("*^7", "hi"), "**hi***");
            assert_eq!(call_str("*<7", "hi"), "hi*****");
            assert_eq!(call_int("*>7", 42, false), "*****42");
            assert_eq!(call_float("*^8.2", 1.5), "**1.50**");
        }
    }

    #[test]
    fn runtime_agrees_with_direct_apply() {
        // The FFI wrappers must return exactly what a direct `apply_*` call
        // returns — the property that guarantees run==build. Cross-check a
        // spread of specs against the shared helpers directly.
        unsafe {
            for (spec, v) in [("b", 42i64), ("^10", -7), ("=^9b", 6), ("*>12", 100)] {
                let fs = FormatSpec::parse(spec).unwrap();
                assert_eq!(call_int(spec, v, false), fs.apply_int(v), "int spec {spec}");
            }
            for spec in ["^9", "*^11", "*<8", ">6"] {
                let fs = FormatSpec::parse(spec).unwrap();
                assert_eq!(
                    call_str(spec, "kara"),
                    fs.apply_str("kara"),
                    "str spec {spec}"
                );
            }
        }
    }

    #[test]
    fn out_cap_never_overflows() {
        // A buffer smaller than the rendered output truncates rather than
        // overflowing (the hard safety net).
        unsafe {
            let mut buf = [0xAAu8; 4];
            let n = karac_runtime_fmt_int(
                "b".as_ptr(),
                1,
                255,
                0,
                0,
                buf.as_mut_ptr(),
                buf.len() as i64,
            );
            assert_eq!(n, 4);
            assert_eq!(&buf, b"1111");
        }
    }

    /// B-2026-09-07-45 — the SLOW path at 128 bits.
    ///
    /// `karac_runtime_fmt_int` took a single `i64` value, so codegen handed a
    /// 128-bit hole to an `i64` parameter and LLVM's verifier rejected the
    /// module: `f"{x:^44}"`, `f"{x:b}"` and `f"{x:*>44}"` on a `u128`/`i128`
    /// FAILED TO COMPILE. B-2026-09-07-34 fixed exactly this on the
    /// pre-decoded fast path and did not reach here — the two entrypoints look
    /// alike and are reached by disjoint specs, which is how one kept the
    /// defect after the other lost it.
    ///
    /// `FormatSpec::apply_int128` / `apply_uint128` are the oracle, as
    /// `apply_int` / `apply_uint` are for every narrower width — the agreement
    /// that makes `karac run` == `karac build` for these specs.
    #[test]
    fn runtime_fmt_int_renders_128_bit_values() {
        unsafe {
            // Only specs `needs_runtime_formatter()` actually diverts here.
            let specs = ["b", "^44", "*>44", "*^46", "=^50", "08b", "^12"];
            let unsigned: [u128; 5] = [0, 1, u128::MAX, u128::MAX - 1, 1 << 127];
            for raw in specs {
                let fs = FormatSpec::parse(raw).unwrap();
                for v in unsigned {
                    assert_eq!(
                        call_raw(raw, v, true),
                        fs.apply_uint128(v),
                        "unsigned spec {raw:?} value {v}"
                    );
                }
                for v in [
                    0i128,
                    1,
                    -1,
                    -7,
                    i128::MAX,
                    i128::MIN,
                    1 << 100, // low word is ZERO — what a 64-bit truncation renders as 0
                ] {
                    // Codegen's radix rule, applied by the caller: a
                    // non-decimal radix reinterprets at the hole's own width,
                    // and a 128-bit hole is already that width, so the raw
                    // pattern crosses unchanged either way.
                    assert_eq!(
                        call_raw(raw, v as u128, false),
                        fs.apply_int128(v),
                        "signed spec {raw:?} value {v}"
                    );
                }
            }
            // The 64-bit readings must be untouched by the widening: a hole
            // narrower than 128 bits still renders at ITS width.
            assert_eq!(call_int("b", -1, false), "1".repeat(64));
            assert_eq!(call_raw("b", u128::MAX, false), "1".repeat(128));
            assert_eq!(call_int("^6", -7, false), "  -7  ");
        }
    }

    /// ORACLE AGREEMENT for the allocation-free fast path.
    ///
    /// `karac_runtime_int_fmt` reproduces `FormatSpec::apply_int` /
    /// `apply_uint` from PRE-DECODED constants instead of parsing the spec and
    /// building a `String`. Those two are the interpreter's path, so the fast
    /// path is only correct insofar as it matches them byte for byte — and the
    /// failure mode is silent (a wrong pad or a dropped sign still prints
    /// something plausible). So this asserts the whole cross product rather
    /// than a handful of eyeballed cases.
    ///
    /// Specs `needs_runtime_formatter()` diverts (binary radix, center align,
    /// non-space fill) are excluded: codegen never routes them here.
    #[test]
    fn int_fmt_fast_path_matches_apply_int_over_a_matrix() {
        // `raw` is the value already widened to 128 bits the way codegen
        // widens it — SIGN-extended for a signed hole, ZERO-extended for an
        // unsigned one — then split into little-endian words for the call.
        unsafe fn fast(fs: &FormatSpec, raw: u128, signed: bool) -> String {
            unsafe {
                let mut buf = [0u8; 256];
                let n = crate::karac_runtime_int_fmt(
                    raw as u64,
                    (raw >> 64) as u64,
                    signed as i32,
                    fs.fast_radix_code(),
                    fs.zero_pad as i32,
                    fs.width.unwrap_or(0) as i64,
                    fs.numeric_align_left() as i32,
                    buf.as_mut_ptr(),
                    buf.len() as i64,
                );
                String::from_utf8(buf[..n as usize].to_vec()).unwrap()
            }
        }

        // Every spec shape the fast path can actually receive.
        let specs = [
            "", "5", "1", "20", "<5", ">5", "<12", ">12", "05", "020", "08", "x", "X", "o", "5x",
            "5X", "5o", "05x", "05X", "05o", "<8x", ">8X", "<8o", "020x",
        ];
        // Boundaries first: i64::MIN is where a naive negate wraps, and the
        // powers of ten/two are where digit counts cross the width.
        let signed_vals: [i64; 14] = [
            0,
            1,
            -1,
            7,
            -7,
            9,
            -9,
            10,
            -10,
            99999,
            -99999,
            i64::MAX,
            i64::MIN,
            i64::MIN + 1,
        ];
        let unsigned_vals: [u64; 8] = [0, 1, 9, 10, 255, u64::MAX, u64::MAX - 1, 1 << 63];

        for raw in specs {
            let fs = FormatSpec::parse(raw).expect("spec parses");
            if fs.needs_runtime_formatter() {
                continue;
            }
            for v in signed_vals {
                let want = fs.apply_int(v);
                // Mirror codegen's widening rule exactly: decimal sign-extends,
                // every other radix zero-extends at the hole's own width.
                let widened = if fs.radix == format_spec::Radix::Dec {
                    v as i128 as u128
                } else {
                    u128::from(v as u64)
                };
                let got = unsafe { fast(&fs, widened, true) };
                assert_eq!(
                    got, want,
                    "signed mismatch: spec {raw:?} value {v} -> fast {got:?} vs apply_int {want:?}"
                );
            }
            for v in unsigned_vals {
                let want = fs.apply_uint(v);
                let got = unsafe { fast(&fs, u128::from(v), false) };
                assert_eq!(
                    got, want,
                    "unsigned mismatch: spec {raw:?} value {v} -> fast {got:?} vs apply_uint {want:?}"
                );
            }
        }
    }

    /// The 128-BIT arm of the fast path.
    ///
    /// This used to reach for Rust's own `{}` / `{:x}` / `{:o}` as its
    /// reference, because `apply_int` took `i64` and `apply_uint` took `u64` —
    /// `FormatSpec` simply had no 128-bit renderer to differ from, which is a
    /// weaker check than the interpreter-vs-runtime agreement every narrower
    /// width gets. `apply_int128` / `apply_uint128` (B-2026-09-07-35, which
    /// added them so the INTERPRETER would stop aborting on these holes) closed
    /// that gap, so the spec matrix below is now a real oracle. Rust's
    /// formatting is kept for the bare digits: two independent references cost
    /// nothing and disagree loudly if either side drifts.
    ///
    /// This arm exists because passing a single `i64` here made a spec'd
    /// `i128` hole fail LLVM module verification outright — `f"{x:44}"` on an
    /// `i128` did not compile — where the older `snprintf` path had silently
    /// truncated to the low 64 bits instead.
    #[test]
    fn int_fmt_fast_path_renders_128_bit_values() {
        unsafe fn fast(raw: u128, signed: bool, radix: i32, width: i64) -> String {
            unsafe {
                let mut buf = [0u8; 256];
                let n = crate::karac_runtime_int_fmt(
                    raw as u64,
                    (raw >> 64) as u64,
                    signed as i32,
                    radix,
                    0,
                    width,
                    0,
                    buf.as_mut_ptr(),
                    buf.len() as i64,
                );
                String::from_utf8(buf[..n as usize].to_vec()).unwrap()
            }
        }
        /// The same call, driven by a parsed `FormatSpec` instead of loose
        /// radix/width arguments — so the oracle matrix below can exercise
        /// zero-pad and align, which the four-argument form cannot express.
        unsafe fn fast_fs(fs: &FormatSpec, raw: u128, signed: bool) -> String {
            unsafe {
                let mut buf = [0u8; 256];
                let n = crate::karac_runtime_int_fmt(
                    raw as u64,
                    (raw >> 64) as u64,
                    signed as i32,
                    fs.fast_radix_code(),
                    fs.zero_pad as i32,
                    fs.width.unwrap_or(0) as i64,
                    fs.numeric_align_left() as i32,
                    buf.as_mut_ptr(),
                    buf.len() as i64,
                );
                String::from_utf8(buf[..n as usize].to_vec()).unwrap()
            }
        }
        fn pad_right(body: &str, width: usize) -> String {
            if body.len() >= width {
                body.to_string()
            } else {
                format!("{}{}", " ".repeat(width - body.len()), body)
            }
        }

        let signed: [i128; 6] = [
            0,
            1,
            -1,
            i128::MAX,
            i128::MIN,
            1_267_650_600_228_229_401_496_703_205_376, // 2^100 — low word is ZERO,
        ]; // which is exactly what a 64-bit truncation renders as 0
        for v in signed {
            assert_eq!(unsafe { fast(v as u128, true, 10, 0) }, format!("{v}"));
            assert_eq!(
                unsafe { fast(v as u128, true, 10, 44) },
                pad_right(&format!("{v}"), 44),
                "signed 128-bit width padding, value {v}"
            );
        }
        for v in [0u128, 1, u128::MAX, u128::MAX - 1, 1u128 << 127] {
            assert_eq!(unsafe { fast(v, false, 10, 0) }, format!("{v}"));
            assert_eq!(unsafe { fast(v, false, 16, 0) }, format!("{v:x}"));
            assert_eq!(unsafe { fast(v, false, -16, 0) }, format!("{v:X}"));
            assert_eq!(unsafe { fast(v, false, 8, 0) }, format!("{v:o}"));
        }
        // octal u128::MAX is 43 digits — the widest rendering the scratch
        // buffer has to hold, so it is the one that would overflow it.
        assert_eq!(unsafe { fast(u128::MAX, false, 8, 0) }.len(), 43);

        // ORACLE MATRIX against `FormatSpec`'s own 128-bit renderers, over
        // every spec shape the fast path can receive. This is the assertion
        // that actually pins run == build at this width; the Rust-reference
        // checks above only pin the digits.
        for raw in [
            "", "44", "1", "<44", ">44", "044", "x", "X", "o", "44x", "044o", "<48X", "020",
        ] {
            let fs = FormatSpec::parse(raw).unwrap();
            for v in [0u128, 1, u128::MAX, u128::MAX - 1, 1 << 127, 1 << 100] {
                assert_eq!(
                    unsafe { fast_fs(&fs, v, false) },
                    fs.apply_uint128(v),
                    "unsigned 128-bit spec {raw:?} value {v}"
                );
            }
            for v in [0i128, 1, -1, -7, i128::MAX, i128::MIN, 1 << 100] {
                // Codegen's rule: a NON-DECIMAL radix reinterprets at the
                // hole's own width. A 128-bit hole IS that width, so the raw
                // pattern crosses unchanged and `apply_int128` reads it the
                // same way.
                assert_eq!(
                    unsafe { fast_fs(&fs, v as u128, true) },
                    fs.apply_int128(v),
                    "signed 128-bit spec {raw:?} value {v}"
                );
            }
        }
    }

    /// ORACLE AGREEMENT for the allocation-free spec'd FLOAT path.
    ///
    /// `karac_runtime_f64_fmt` reproduces `FormatSpec::apply_float` from
    /// pre-decoded constants, and that pair is the interpreter's path, so the
    /// fast path is only correct insofar as it matches byte for byte. The
    /// failure mode is silent -- a wrong pad or a dropped sign still prints
    /// something plausible -- so this asserts a cross product.
    ///
    /// The float arm had NO codegen coverage at all before B-2026-09-07-39,
    /// which is how the `snprintf` over-read of B-2026-09-07-46 survived.
    ///
    /// Specs `needs_runtime_formatter()` diverts (center align, non-space fill)
    /// are excluded: codegen never routes them here.
    #[test]
    fn f64_fmt_matches_apply_float_over_a_matrix() {
        unsafe fn fast(fs: &FormatSpec, v: f64) -> String {
            unsafe {
                let mut buf = [0u8; 1024];
                let n = crate::karac_runtime_f64_fmt(
                    v,
                    fs.precision.map_or(-1i64, |p| p as i64),
                    fs.zero_pad as i32,
                    fs.width.unwrap_or(0) as i64,
                    fs.numeric_align_left() as i32,
                    buf.as_mut_ptr(),
                    buf.len() as i64,
                );
                String::from_utf8(buf[..n as usize].to_vec()).unwrap()
            }
        }
        let specs = [
            ".0", ".1", ".2", ".5", "8.2", "12.2", "<8.2", ">8.2", "08.2", "012.3", "1.2", "20.6",
        ];
        let vals: [f64; 14] = [
            0.0,
            1.5,
            -1.5,
            3.0,
            -3.0,
            1.23456,
            -1.23456,
            1234567.891,
            -0.0,
            0.5,
            -0.5,
            1e-7,
            f64::INFINITY,
            f64::NAN,
        ];
        for raw in specs {
            let fs = FormatSpec::parse(raw).unwrap();
            for v in vals {
                assert_eq!(
                    unsafe { fast(&fs, v) },
                    fs.apply_float(v),
                    "spec {raw:?} value {v}"
                );
            }
        }
        // The WIDE case, which is the one that used to read past the buffer:
        // `f64::MAX` at `.2` is 312 bytes. Codegen sizes the buffer from the
        // spec now, and this asserts the renderer fills it correctly rather
        // than reporting a length it did not write.
        let fs = FormatSpec::parse(".2").unwrap();
        let got = unsafe { fast(&fs, f64::MAX) };
        assert_eq!(got, fs.apply_float(f64::MAX));
        assert_eq!(got.len(), 312, "f64::MAX at .2 is 312 bytes");
        assert!(got.chars().all(|c| c.is_ascii_digit() || c == '.'));
    }

    /// The float fast path must TRUNCATE rather than write past a short buffer,
    /// AND must report the truncated length -- the exact contract C's
    /// `snprintf` does not honour, which is what made B-2026-09-07-46 an
    /// out-of-bounds read rather than merely a wrong string.
    #[test]
    fn f64_fmt_reports_the_truncated_length_not_the_would_be_length() {
        unsafe {
            let mut buf = [0xAAu8; 16];
            // `f64::MAX` at `.2` wants 312 bytes; only 8 are available.
            let n = crate::karac_runtime_f64_fmt(f64::MAX, 2, 0, 0, 0, buf.as_mut_ptr(), 8);
            assert_eq!(n, 8, "must report what it WROTE, not what it wanted");
            assert_eq!(&buf[..8], b"17976931");
            assert_eq!(&buf[8..], &[0xAA; 8], "must not write past buf_len");

            // null / non-positive cap write nothing.
            assert_eq!(
                crate::karac_runtime_f64_fmt(1.5, 2, 0, 0, 0, std::ptr::null_mut(), 8),
                0
            );
            assert_eq!(
                crate::karac_runtime_f64_fmt(1.5, 2, 0, 0, 0, buf.as_mut_ptr(), 0),
                0
            );
        }
    }

    /// The fast path must TRUNCATE rather than write past a short buffer.
    /// Codegen sizes the buffer as `max(64, width + 2)` so this is a guard, not
    /// an expected path — but it is the one bug in a hand-rolled renderer that
    /// corrupts memory instead of printing wrong.
    #[test]
    fn int_fmt_fast_path_truncates_into_a_short_buffer() {
        unsafe {
            let mut buf = [0xAAu8; 8];
            let n = crate::karac_runtime_int_fmt(
                1234567890123u64,
                0,
                1,
                10,
                0,
                0,
                0,
                buf.as_mut_ptr(),
                4,
            );
            assert_eq!(n, 4, "should stop at the cap");
            assert_eq!(&buf[..4], b"1234");
            assert_eq!(&buf[4..], &[0xAA; 4], "must not write past buf_len");

            // null / non-positive cap write nothing
            assert_eq!(
                crate::karac_runtime_int_fmt(1, 0, 1, 10, 0, 0, 0, std::ptr::null_mut(), 8),
                0
            );
            assert_eq!(
                crate::karac_runtime_int_fmt(1, 0, 1, 10, 0, 0, 0, buf.as_mut_ptr(), 0),
                0
            );
        }
    }
}
