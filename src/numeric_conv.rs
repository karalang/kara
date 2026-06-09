//! Shared metadata + semantics for the float→int and int→float conversion
//! method families on numeric primitives — phase-8 § "Saturating float→int +
//! every-`as`-cast-pair fully defined", slice 2.
//!
//! Three backends consume this without coupling to each other:
//!   * the **typechecker** registers the method surface + return types
//!     (`typechecker/expr_method_call.rs`),
//!   * the **interpreter** computes the runtime result
//!     (`interpreter/method_call.rs`),
//!   * the **effectchecker** marks `trunc_to_*` as carrying `panics`
//!     (`effectchecker/inference.rs` + `seed_builtin_effects`).
//!
//! Codegen (the bit-exact `fptosi.sat` / `fptoui.sat` lowering + the `phi`/trap
//! shapes) is slice 4 and is intentionally not wired here. Until it lands, a
//! *compiled* program using one of these methods gets a clean "interpreter-only"
//! error from `codegen/method_call.rs` rather than an ICE or silent miscompile.
//!
//! `isize` is deliberately absent from the target set — it is not a Kāra type
//! (the language has `usize` but no signed-pointer-width integer). The phase-8
//! entry's mention of `isize` predates this and is corrected there.

/// The four float→int conversion families. The method name is
/// `<family>_to_<target>` — `saturating_to_i32`, `wrapping_to_u8`,
/// `checked_to_i64`, `trunc_to_i16`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FloatToIntFamily {
    /// Out-of-range clamps to the target's MIN/MAX; NaN → 0. (Same rule as the
    /// `as` cast.)
    Saturating,
    /// Raw modular truncation — the un-saturated `fptosi`/`fptoui` form, defined
    /// as wraparound modulo `2^bits`.
    Wrapping,
    /// `Option[T]`: `None` on NaN / out-of-range, else `Some(trunc-toward-zero)`.
    Checked,
    /// Traps (carries `panics`) on NaN / out-of-range, else `trunc-toward-zero`.
    Trunc,
}

/// Every integer type a float can convert to: `(name, bit_width, signed)`.
/// `usize` is modeled as 64-bit (the only supported pointer width).
pub const INT_TARGETS: &[(&str, u32, bool)] = &[
    ("i8", 8, true),
    ("i16", 16, true),
    ("i32", 32, true),
    ("i64", 64, true),
    ("i128", 128, true),
    ("u8", 8, false),
    ("u16", 16, false),
    ("u32", 32, false),
    ("u64", 64, false),
    ("u128", 128, false),
    ("usize", 64, false),
];

/// `true` iff `name` is one of the integer-conversion target type names.
pub fn is_int_target(name: &str) -> bool {
    INT_TARGETS.iter().any(|(n, _, _)| *n == name)
}

/// Parse a float→int conversion method name into its family + canonical target.
/// `"saturating_to_i32"` → `Some((Saturating, "i32", 32, true))`. Returns `None`
/// for any other method name (including a known prefix with an unknown suffix,
/// e.g. `trunc_to_isize`).
pub fn parse_float_to_int(method: &str) -> Option<(FloatToIntFamily, &'static str, u32, bool)> {
    use FloatToIntFamily::*;
    const PREFIXES: &[(&str, FloatToIntFamily)] = &[
        ("saturating_to_", Saturating),
        ("wrapping_to_", Wrapping),
        ("checked_to_", Checked),
        ("trunc_to_", Trunc),
    ];
    for &(prefix, family) in PREFIXES {
        if let Some(suffix) = method.strip_prefix(prefix) {
            if let Some(&(name, bits, signed)) = INT_TARGETS.iter().find(|(n, _, _)| *n == suffix) {
                return Some((family, name, bits, signed));
            }
        }
    }
    None
}

/// Result of a float→int conversion under the interpreter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConvOutcome {
    /// The converted integer, widened to `i128`. The interpreter's `Value::Int`
    /// is `i64`, so `u64`/`u128`/`i128` results beyond `i64` range are stored
    /// truncated by the caller — a pre-existing interpreter wide-int limitation,
    /// not specific to these methods. Codegen (slice 4) is bit-exact.
    Value(i128),
    /// `checked_to_*` on NaN / out-of-range.
    None,
    /// `trunc_to_*` on NaN / out-of-range — the caller raises a runtime panic.
    Panic,
}

/// Compute `f.<family>_to_<target>()`. Pure — the `Trunc` out-of-range case is
/// reported as [`ConvOutcome::Panic`] for the caller to surface as a runtime
/// error rather than panicking the compiler.
pub fn convert_float_to_int(
    f: f64,
    family: FloatToIntFamily,
    bits: u32,
    signed: bool,
) -> ConvOutcome {
    match family {
        FloatToIntFamily::Saturating => ConvOutcome::Value(saturating_cast(f, bits, signed)),
        FloatToIntFamily::Wrapping => ConvOutcome::Value(wrapping_cast(f, bits, signed)),
        FloatToIntFamily::Checked => {
            if in_range(f, bits, signed) {
                ConvOutcome::Value(saturating_cast(f, bits, signed))
            } else {
                ConvOutcome::None
            }
        }
        FloatToIntFamily::Trunc => {
            if in_range(f, bits, signed) {
                ConvOutcome::Value(saturating_cast(f, bits, signed))
            } else {
                ConvOutcome::Panic
            }
        }
    }
}

/// Native Rust saturating cast (`as` from float saturates and maps NaN → 0
/// since Rust 1.45), widened to `i128`. A `u128` result above `i128::MAX` wraps
/// on the final widening — only reachable for the `u128` target, whose values
/// the interpreter cannot represent anyway.
fn saturating_cast(f: f64, bits: u32, signed: bool) -> i128 {
    match (bits, signed) {
        (8, true) => (f as i8) as i128,
        (16, true) => (f as i16) as i128,
        (32, true) => (f as i32) as i128,
        (64, true) => (f as i64) as i128,
        (128, true) => f as i128,
        (8, false) => (f as u8) as i128,
        (16, false) => (f as u16) as i128,
        (32, false) => (f as u32) as i128,
        (64, false) => (f as u64) as i128,
        (128, false) => (f as u128) as i128,
        _ => 0,
    }
}

/// Modular (`wrapping`) truncation: truncate toward zero, then reduce modulo
/// `2^bits` into the target's signedness. Best-effort for `|f| ≥ 2^127` (the
/// `as i128` step saturates first); codegen is the bit-exact path.
fn wrapping_cast(f: f64, bits: u32, signed: bool) -> i128 {
    let t = f.trunc();
    if !t.is_finite() {
        return 0; // NaN / ±∞ → 0 (define the otherwise-UB raw cast)
    }
    let raw = t as i128; // saturates only for astronomically large |f|
    if bits >= 128 {
        return raw;
    }
    let modulus = 1i128 << bits;
    let mut m = raw & (modulus - 1); // keep the low `bits` bits
    if signed && (m & (1i128 << (bits - 1))) != 0 {
        m -= modulus; // sign-extend the high bit
    }
    m
}

/// Is `f` exactly representable in the target integer type after truncating
/// toward zero? Drives `checked_*` / `trunc_*`. The f64 bounds for 64/128-bit
/// targets round at the very top of the range — a known interpreter best-effort
/// edge; codegen uses an exact `fcmp` pair.
fn in_range(f: f64, bits: u32, signed: bool) -> bool {
    if !f.is_finite() {
        return false;
    }
    let t = f.trunc();
    let (min_f, max_f) = if signed {
        let half = 2f64.powi((bits - 1) as i32);
        (-half, half - 1.0)
    } else {
        (0.0, 2f64.powi(bits as i32) - 1.0)
    };
    t >= min_f && t <= max_f
}

#[cfg(test)]
mod tests {
    use super::*;
    use FloatToIntFamily::*;

    #[test]
    fn parses_every_family_and_target() {
        assert_eq!(
            parse_float_to_int("saturating_to_i32"),
            Some((Saturating, "i32", 32, true))
        );
        assert_eq!(
            parse_float_to_int("wrapping_to_u8"),
            Some((Wrapping, "u8", 8, false))
        );
        assert_eq!(
            parse_float_to_int("checked_to_i64"),
            Some((Checked, "i64", 64, true))
        );
        assert_eq!(
            parse_float_to_int("trunc_to_usize"),
            Some((Trunc, "usize", 64, false))
        );
        // every target name resolves under every family
        for &(name, bits, signed) in INT_TARGETS {
            for fam in ["saturating", "wrapping", "checked", "trunc"] {
                let m = format!("{fam}_to_{name}");
                assert_eq!(
                    parse_float_to_int(&m).map(|(_, n, b, s)| (n, b, s)),
                    Some((name, bits, signed))
                );
            }
        }
    }

    #[test]
    fn rejects_unknown_and_isize() {
        assert_eq!(parse_float_to_int("trunc_to_isize"), None); // isize is not a Kāra type
        assert_eq!(parse_float_to_int("to_i32"), None);
        assert_eq!(parse_float_to_int("saturating_to_"), None);
        assert_eq!(parse_float_to_int("abs"), None);
    }

    #[test]
    fn saturating_clamps_and_zeroes_nan() {
        assert_eq!(
            convert_float_to_int(f64::NAN, Saturating, 32, true),
            ConvOutcome::Value(0)
        );
        assert_eq!(
            convert_float_to_int(f64::INFINITY, Saturating, 32, true),
            ConvOutcome::Value(i32::MAX as i128)
        );
        assert_eq!(
            convert_float_to_int(f64::NEG_INFINITY, Saturating, 32, true),
            ConvOutcome::Value(i32::MIN as i128)
        );
        assert_eq!(
            convert_float_to_int(1e30, Saturating, 32, true),
            ConvOutcome::Value(i32::MAX as i128)
        );
        assert_eq!(
            convert_float_to_int(-1e30, Saturating, 32, true),
            ConvOutcome::Value(i32::MIN as i128)
        );
        assert_eq!(
            convert_float_to_int(1e30, Saturating, 8, false),
            ConvOutcome::Value(255)
        );
        assert_eq!(
            convert_float_to_int(-1.0, Saturating, 8, false),
            ConvOutcome::Value(0)
        );
        assert_eq!(
            convert_float_to_int(3.7, Saturating, 32, true),
            ConvOutcome::Value(3)
        );
        assert_eq!(
            convert_float_to_int(-3.7, Saturating, 32, true),
            ConvOutcome::Value(-3)
        );
    }

    #[test]
    fn checked_none_on_nan_and_out_of_range() {
        assert_eq!(
            convert_float_to_int(f64::NAN, Checked, 32, true),
            ConvOutcome::None
        );
        assert_eq!(
            convert_float_to_int(1e30, Checked, 32, true),
            ConvOutcome::None
        );
        assert_eq!(
            convert_float_to_int(1.5, Checked, 32, true),
            ConvOutcome::Value(1)
        );
        assert_eq!(
            convert_float_to_int(-1.5, Checked, 8, false),
            ConvOutcome::None
        );
        assert_eq!(
            convert_float_to_int(255.0, Checked, 8, false),
            ConvOutcome::Value(255)
        );
        assert_eq!(
            convert_float_to_int(256.0, Checked, 8, false),
            ConvOutcome::None
        );
    }

    #[test]
    fn trunc_panics_on_nan_and_out_of_range() {
        assert_eq!(
            convert_float_to_int(f64::NAN, Trunc, 32, true),
            ConvOutcome::Panic
        );
        assert_eq!(
            convert_float_to_int(1e30, Trunc, 32, true),
            ConvOutcome::Panic
        );
        assert_eq!(
            convert_float_to_int(42.9, Trunc, 32, true),
            ConvOutcome::Value(42)
        );
    }

    #[test]
    fn wrapping_is_modular() {
        // 300 wraps into i8 as 300 - 256 = 44
        assert_eq!(
            convert_float_to_int(300.0, Wrapping, 8, true),
            ConvOutcome::Value(44)
        );
        // 256 wraps to 0 in u8
        assert_eq!(
            convert_float_to_int(256.0, Wrapping, 8, false),
            ConvOutcome::Value(0)
        );
        // 257.9 truncates to 257, wraps to 1 in u8
        assert_eq!(
            convert_float_to_int(257.9, Wrapping, 8, false),
            ConvOutcome::Value(1)
        );
        // NaN → 0
        assert_eq!(
            convert_float_to_int(f64::NAN, Wrapping, 8, false),
            ConvOutcome::Value(0)
        );
        // in-range values are unchanged (truncated toward zero)
        assert_eq!(
            convert_float_to_int(-3.7, Wrapping, 32, true),
            ConvOutcome::Value(-3)
        );
    }
}
