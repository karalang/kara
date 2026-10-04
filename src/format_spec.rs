//! Format specifiers for f-string interpolation holes — `f"{expr:spec}"`
//! (Phase 8 stdlib floor).
//!
//! A hole may carry a specifier after the first depth-0 `:` (the parser splits
//! it off in `src/parser/exprs.rs`). This module parses that specifier and
//! applies it, once, in Rust — the interpreter calls the `apply_*` helpers
//! directly, and codegen calls the SAME helpers on the already-rendered pieces
//! it can compute at compile time OR routes the runtime value through the same
//! logic via `karac_runtime_fmt_*`. Keeping one Rust implementation is what
//! guarantees `karac run` == `karac build` for formatted output.
//!
//! Grammar (a Rust/Python-like subset):
//!
//! ```text
//! spec   := [[fill] align] ['+'] ['0'] [width] ['.' precision] [type]
//! align  := '<' | '>' | '^'
//! width  := DIGIT+
//! prec   := DIGIT+
//! type   := 'x' | 'X' | 'o' | 'b' | 'd'
//! ```
//!
//! `fill` is any single char and requires an explicit `align` after it (so a
//! bare `0` stays the zero-pad flag, not a fill char). `+` prints a sign on a
//! non-negative number too, as in Rust and Python (B-2026-10-02-42); like
//! Rust, it also signs a non-decimal radix (`{255:+x}` is `+ff`), and a NaN
//! stays unsigned. Unrecognized specs are a
//! hard parse error surfaced at the interpolation site rather than silently
//! ignored — a silently-dropped specifier is the exact surprise this feature
//! removes.

/// Text alignment within the field `width`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
    Center,
}

/// Integer radix / rendering type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Radix {
    Dec,
    Hex,
    HexUpper,
    Oct,
    Bin,
}

/// A parsed `f"{expr:spec}"` specifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatSpec {
    pub fill: Option<char>,
    pub align: Option<Align>,
    /// `+` flag — print `+` before a non-negative number (B-2026-10-02-42).
    pub plus: bool,
    /// `0` flag — zero-pad numerics to `width` (right-aligned, after the sign).
    pub zero_pad: bool,
    pub width: Option<usize>,
    pub precision: Option<usize>,
    pub radix: Radix,
}

impl FormatSpec {
    /// Parse a raw specifier string (the text after the hole's `:`). Returns a
    /// human-readable error for an unrecognized spec.
    pub fn parse(raw: &str) -> Result<FormatSpec, String> {
        let mut spec = FormatSpec {
            fill: None,
            align: None,
            plus: false,
            zero_pad: false,
            width: None,
            precision: None,
            radix: Radix::Dec,
        };
        let chars: Vec<char> = raw.chars().collect();
        let mut i = 0;

        let align_of = |c: char| match c {
            '<' => Some(Align::Left),
            '>' => Some(Align::Right),
            '^' => Some(Align::Center),
            _ => None,
        };

        // [[fill] align] — a fill char is only recognized when an align char
        // follows it, so `<`/`>`/`^` alone is align-with-default-fill and a
        // leading `0` stays the zero-pad flag.
        if chars.len() >= 2 {
            if let Some(a) = align_of(chars[1]) {
                spec.fill = Some(chars[0]);
                spec.align = Some(a);
                i = 2;
            }
        }
        if spec.align.is_none() {
            if let Some(&c) = chars.first() {
                if let Some(a) = align_of(c) {
                    spec.align = Some(a);
                    i = 1;
                }
            }
        }

        // ['+'] sign flag. Rust also accepts `-` (and ignores it) and Python
        // a space; neither is supported, and both used to be reported as an
        // unsupported TYPE because the parser reached the `[type]` slot with
        // them (B-2026-10-02-42), so name them as sign flags instead.
        if i < chars.len() && chars[i] == '+' {
            spec.plus = true;
            i += 1;
        } else if i < chars.len() && (chars[i] == '-' || chars[i] == ' ') {
            return Err(format!(
                "format spec `{raw}`: sign flag `{}` is not supported (only `+` is)",
                chars[i]
            ));
        }

        // ['0'] zero-pad flag.
        if i < chars.len() && chars[i] == '0' {
            spec.zero_pad = true;
            i += 1;
        }

        // [width]
        let width_start = i;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        if i > width_start {
            let w: String = chars[width_start..i].iter().collect();
            spec.width = Some(
                w.parse()
                    .map_err(|_| format!("format spec width `{w}` is out of range"))?,
            );
        }

        // ['.' precision]
        if i < chars.len() && chars[i] == '.' {
            i += 1;
            let prec_start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i == prec_start {
                return Err(format!(
                    "format spec `{raw}`: `.` must be followed by a precision (e.g. `.2`)"
                ));
            }
            let p: String = chars[prec_start..i].iter().collect();
            spec.precision = Some(
                p.parse()
                    .map_err(|_| format!("format spec precision `{p}` is out of range"))?,
            );
        }

        // [type]
        if i < chars.len() {
            spec.radix = match chars[i] {
                'x' => Radix::Hex,
                'X' => Radix::HexUpper,
                'o' => Radix::Oct,
                'b' => Radix::Bin,
                'd' => Radix::Dec,
                other => {
                    return Err(format!(
                        "format spec `{raw}`: unsupported type `{other}` \
                         (expected one of x, X, o, b, d)"
                    ));
                }
            };
            i += 1;
        }

        if i != chars.len() {
            let rest: String = chars[i..].iter().collect();
            return Err(format!("format spec `{raw}`: unexpected trailing `{rest}`"));
        }
        if spec.radix != Radix::Dec && spec.precision.is_some() {
            return Err(format!(
                "format spec `{raw}`: precision is not valid with an integer type"
            ));
        }
        Ok(spec)
    }

    /// True when this spec cannot be rendered by codegen's inline `snprintf`
    /// path and must route through the shared runtime formatter
    /// (`karac_runtime_fmt_*`) instead. printf has no binary conversion, no
    /// center alignment, and no custom (non-space) fill char — but the
    /// `apply_*` helpers on this struct handle all three, so codegen hands the
    /// value + raw spec to the runtime, which parses with THIS parser and calls
    /// the SAME `apply_*`. The interpreter always calls `apply_*` directly, so
    /// `karac run` == `karac build` for these specifiers by construction. Every
    /// other spec stays on the faster inline `to_printf` path.
    ///
    /// The `+` flag goes the same way: printf has one, but it is undefined on
    /// `%x`/`%o` and prints `+nan`, where `apply_*` follows Rust (`+ff`, `NaN`).
    pub fn needs_runtime_formatter(&self) -> bool {
        self.align == Some(Align::Center)
            || self.radix == Radix::Bin
            || (self.fill.is_some() && self.fill != Some(' '))
            || self.plus
    }

    /// Pad `body` to `width` honoring `align` (default: right for the numeric
    /// path, which passes `default_left = false`; left for strings). `fill` is
    /// the pad char (default space). Zero-pad is handled by the numeric callers
    /// before this (it inserts zeros after the sign), so this only does
    /// space/fill padding.
    fn pad(&self, body: &str, default_left: bool) -> String {
        let Some(width) = self.width else {
            return body.to_string();
        };
        let len = body.chars().count();
        if len >= width {
            return body.to_string();
        }
        let pad = width - len;
        let fill = self.fill.unwrap_or(' ');
        let align = self.align.unwrap_or(if default_left {
            Align::Left
        } else {
            Align::Right
        });
        let fills = |n: usize| String::from(fill).repeat(n);
        match align {
            Align::Left => format!("{body}{}", fills(pad)),
            Align::Right => format!("{}{body}", fills(pad)),
            Align::Center => {
                let left = pad / 2;
                format!("{}{body}{}", fills(left), fills(pad - left))
            }
        }
    }

    fn render_int_magnitude(&self, mag: u128) -> String {
        match self.radix {
            Radix::Dec => mag.to_string(),
            Radix::Hex => format!("{mag:x}"),
            Radix::HexUpper => format!("{mag:X}"),
            Radix::Oct => format!("{mag:o}"),
            Radix::Bin => format!("{mag:b}"),
        }
    }

    /// Render a sign plus an ALREADY-REINTERPRETED magnitude. Zero-pad inserts
    /// zeros between the sign and the digits (`{-7:05}` -> `-0007`); otherwise
    /// the whole rendered number is padded per `align` (default right).
    ///
    /// The magnitude is 128 bits because that is the widest integer the
    /// language has — but the width-DEPENDENT decision, whether a negative
    /// value reinterprets as 64 or 128 bits under a non-decimal radix, belongs
    /// to the caller, which is the only party that knows the hole's own width.
    /// `{-1:x}` is `ffffffffffffffff` on an `i64` hole and thirty-two f's on an
    /// `i128` one; both are correct, and folding the two together here is
    /// exactly the mistake that would silently make one of them wrong.
    fn apply_magnitude(&self, neg: bool, mag: u128) -> String {
        let digits = self.render_int_magnitude(mag);
        let sign = if neg {
            "-"
        } else if self.plus {
            "+"
        } else {
            ""
        };
        if self.zero_pad {
            if let Some(width) = self.width {
                let have = sign.len() + digits.chars().count();
                if have < width {
                    let zeros = "0".repeat(width - have);
                    return format!("{sign}{zeros}{digits}");
                }
            }
        }
        self.pad(&format!("{sign}{digits}"), false)
    }

    /// Format a signed integer AT 64-BIT WIDTH. Only decimal takes the sign; a
    /// non-decimal radix reinterprets the value as `u64`, so `{-1:x}` renders
    /// `ffffffffffffffff` rather than `-1`.
    pub fn apply_int(&self, v: i64) -> String {
        let dec = self.radix == Radix::Dec;
        let mag = if dec {
            u128::from(v.unsigned_abs())
        } else {
            u128::from(v as u64)
        };
        self.apply_magnitude(v < 0 && dec, mag)
    }

    /// Format an unsigned integer at 64-bit width (same rules, never negative).
    pub fn apply_uint(&self, v: u64) -> String {
        self.apply_magnitude(false, u128::from(v))
    }

    /// Format a signed integer AT 128-BIT WIDTH — the `i128` twin of
    /// [`Self::apply_int`], reinterpreting as `u128` under a non-decimal radix
    /// so `{-1:x}` is thirty-two f's rather than sixteen.
    ///
    /// B-2026-09-07-35: until this existed there was nothing for the
    /// interpreter's spec'd f-string arm to call with a 128-bit value, so it
    /// went through `narrow_to_i64` — which PANICS by design rather than
    /// truncate silently — and `f"{big:44}"` on a `u128` ABORTED the
    /// interpreter while both compiled backends rendered it correctly.
    ///
    /// This pair is also the oracle `karac_runtime_int_fmt`'s 128-bit arm was
    /// missing: that test had to reach for Rust's own `{}`/`{:x}` because
    /// `FormatSpec` stopped at 64 bits, which is a weaker check than the
    /// interpreter-vs-runtime agreement every other width gets.
    pub fn apply_int128(&self, v: i128) -> String {
        let dec = self.radix == Radix::Dec;
        let mag = if dec { v.unsigned_abs() } else { v as u128 };
        self.apply_magnitude(v < 0 && dec, mag)
    }

    /// Format an unsigned integer at 128-bit width — the `u128` twin of
    /// [`Self::apply_uint`]. See [`Self::apply_int128`].
    pub fn apply_uint128(&self, v: u128) -> String {
        self.apply_magnitude(false, v)
    }

    /// Format a float. `precision` fixes the fractional digit count (default: the
    /// value's natural rendering). Zero-pad and width apply to the whole number.
    pub fn apply_float(&self, v: f64) -> String {
        let body = match self.precision {
            Some(p) => format!("{v:.*}", p),
            None => {
                // Match the interpreter/codegen bare-float rendering: an integral
                // value still shows one fractional digit.
                if v.fract() == 0.0 && v.is_finite() {
                    format!("{v:.1}")
                } else {
                    format!("{v}")
                }
            }
        };
        // `+` signs every non-negative value except a NaN, which Rust renders
        // unsigned (`format!("{:+}", f64::NAN)` is `NaN`).
        let body = if self.plus && !body.starts_with('-') && !v.is_nan() {
            format!("+{body}")
        } else {
            body
        };
        if self.zero_pad {
            if let Some(width) = self.width {
                let signed = body.starts_with('-') || body.starts_with('+');
                let (sign, rest) = if signed {
                    body.split_at(1)
                } else {
                    ("", body.as_str())
                };
                let have = sign.len() + rest.chars().count();
                if have < width {
                    let zeros = "0".repeat(width - have);
                    return format!("{sign}{zeros}{rest}");
                }
            }
        }
        self.pad(&body, false)
    }

    /// Format a string: width padding + align. Precision (truncation) on a
    /// string is a deferred follow-up (byte-vs-char parity with printf `%.*s`
    /// on multibyte UTF-8), rejected at format time, so this ignores it. Width
    /// default is right-align (matching printf `%Ns` and the numeric path — one
    /// consistent default across all value kinds).
    pub fn apply_str(&self, s: &str) -> String {
        self.pad(s, false)
    }

    /// The printf conversion char for the integer radix (`d`/`x`/`X`/`o`).
    /// Binary routes through the runtime formatter (`needs_runtime_formatter`),
    /// never the `snprintf` path that calls this, so its arm is unreachable
    /// here (kept as a defined mapping rather than a panic).
    pub fn int_conv(&self) -> char {
        match self.radix {
            Radix::Dec => 'd',
            Radix::Hex => 'x',
            Radix::HexUpper => 'X',
            Radix::Oct => 'o',
            Radix::Bin => 'd',
        }
    }

    /// Radix operand for the runtime's allocation-free integer formatter,
    /// `karac_runtime_i64_fmt`: `10`, `8`, `16` (lower) or `-16` (upper).
    ///
    /// This encoding lives HERE, beside [`Self::apply_int`], because two
    /// separate places consume it — codegen stamps it as an LLVM constant, and
    /// the runtime's oracle-agreement test decodes the same spec — and a
    /// mapping duplicated at both ends is one that drifts. `Bin` never reaches
    /// the fast path ([`Self::needs_runtime_formatter`] diverts it), so its arm
    /// is a defined value rather than a panic, matching [`Self::int_conv`].
    pub fn fast_radix_code(&self) -> i32 {
        match self.radix {
            Radix::Dec => 10,
            Radix::Oct => 8,
            Radix::Hex => 16,
            Radix::HexUpper => -16,
            Radix::Bin => 2,
        }
    }

    /// Whether the numeric fast path left-aligns. Numerics default to RIGHT —
    /// see [`Self::pad`]'s `default_left = false` — so only an explicit `<`
    /// selects Left.
    pub fn numeric_align_left(&self) -> bool {
        self.align == Some(Align::Left)
    }

    /// Build the printf conversion string codegen feeds to `snprintf` — e.g.
    /// `%04lld`, `%-8.2f`, `%6s`. `length_mod` is the C length modifier (`"ll"`
    /// for i64, `""` for double / string), `conv` the conversion char, and
    /// `numeric` gates the `0` zero-pad flag (printf ignores `0` for `s`). Only
    /// reached for specs where `needs_runtime_formatter()` is false — the
    /// printf-expressible subset (no center / custom-fill / binary; no string
    /// precision), exactly what printf renders identically to the `apply_*`
    /// helpers above, so `karac run` == `karac build`. Center / custom-fill /
    /// binary take the runtime-formatter path instead.
    pub fn to_printf(&self, length_mod: &str, conv: char, numeric: bool) -> String {
        let mut s = String::from("%");
        if self.align == Some(Align::Left) {
            s.push('-');
        }
        if numeric && self.zero_pad {
            s.push('0');
        }
        if let Some(w) = self.width {
            s.push_str(&w.to_string());
        }
        if let Some(p) = self.precision {
            s.push('.');
            s.push_str(&p.to_string());
        }
        s.push_str(length_mod);
        s.push(conv);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(s: &str) -> FormatSpec {
        FormatSpec::parse(s).unwrap()
    }

    #[test]
    fn int_width_and_zero_pad() {
        assert_eq!(spec("4").apply_int(7), "   7");
        assert_eq!(spec("04").apply_int(7), "0007");
        assert_eq!(spec("05").apply_int(-7), "-0007");
        assert_eq!(spec("<4").apply_int(7), "7   ");
        // Matches printf `%4lld` / `%04lld` / `%-4lld`. The libc cross-check
        // is skipped on Windows: MSVC's UCRT exposes `snprintf` only as an
        // inline in <stdio.h>, so a bare `extern "C" { fn snprintf }` fails to
        // link (LNK2019). The exact-value asserts above cover the same logic.
        #[cfg(not(windows))]
        assert_eq!(spec("4").apply_int(7), format_via_libc_int("%4lld", 7));
        #[cfg(not(windows))]
        assert_eq!(spec("04").apply_int(7), format_via_libc_int("%04lld", 7));
    }

    #[test]
    fn int_radix() {
        assert_eq!(spec("x").apply_int(255), "ff");
        assert_eq!(spec("X").apply_int(255), "FF");
        assert_eq!(spec("o").apply_int(8), "10");
        assert_eq!(spec("08x").apply_int(255), "000000ff");
    }

    /// B-2026-09-07-35 — the 128-bit renderers. `FormatSpec` stopped at 64
    /// bits, which left the interpreter's spec'd f-string arm nothing to call
    /// for an `i128`/`u128` hole.
    ///
    /// The load-bearing assertion is the LAST group: a non-decimal radix
    /// reinterprets at the value's OWN width, so `{-1:x}` is sixteen f's
    /// through `apply_int` and thirty-two through `apply_int128`. Both are
    /// correct; a single shared renderer would have to get one of them wrong.
    #[test]
    fn int_128_bit_width_and_radix() {
        // Magnitudes no 64-bit reading can represent.
        assert_eq!(
            spec("").apply_uint128(u128::MAX),
            "340282366920938463463374607431768211455"
        );
        assert_eq!(
            spec("44").apply_uint128(u128::MAX),
            "     340282366920938463463374607431768211455"
        );
        assert_eq!(spec("").apply_int128(i128::MAX), format!("{}", i128::MAX));
        assert_eq!(spec("").apply_int128(i128::MIN), format!("{}", i128::MIN));
        // 2^100 — its LOW WORD IS ZERO, which is exactly what a 64-bit
        // truncation renders as "0".
        assert_eq!(
            spec("").apply_int128(1 << 100),
            "1267650600228229401496703205376"
        );

        // Sign and zero-pad behave as at 64 bits.
        assert_eq!(spec("06").apply_int128(-7), "-00007");
        assert_eq!(spec("<8").apply_int128(-7), "-7      ");
        assert_eq!(spec("08").apply_uint128(42), "00000042");

        // Non-decimal reinterprets at the value's own width.
        assert_eq!(spec("x").apply_int(-1), "f".repeat(16));
        assert_eq!(spec("x").apply_int128(-1), "f".repeat(32));
        assert_eq!(spec("X").apply_int128(-1), "F".repeat(32));
        assert_eq!(spec("o").apply_int128(-1), format!("{:o}", u128::MAX));
        assert_eq!(spec("b").apply_int128(-1), "1".repeat(128));
        // Decimal is the one that keeps the sign at either width.
        assert_eq!(spec("").apply_int(-1), "-1");
        assert_eq!(spec("").apply_int128(-1), "-1");
    }

    /// The 64-bit helpers must be UNCHANGED by the widening — they now share
    /// `apply_magnitude` with the 128-bit pair, and the way that refactor goes
    /// wrong is by silently promoting a 64-bit hole to a 128-bit
    /// reinterpretation.
    ///
    /// Checked against INDEPENDENT references rather than against the helper
    /// under test: a non-decimal radix is defined as the `u64` reinterpretation
    /// (so `apply_int(v)` must equal `apply_uint(v as u64)`, and Rust's own
    /// `{:x}` of that `u64`), and decimal is defined as sign + magnitude.
    #[test]
    fn int_64_bit_readings_are_unchanged_by_the_128_bit_widening() {
        for v in [0i64, 1, -1, 7, -7, i64::MAX, i64::MIN, -255, 255] {
            let u = v as u64;
            // Non-decimal: the `u64` reinterpretation, at 64 bits.
            assert_eq!(spec("x").apply_int(v), format!("{u:x}"), "hex of {v}");
            assert_eq!(spec("X").apply_int(v), format!("{u:X}"), "HEX of {v}");
            assert_eq!(spec("o").apply_int(v), format!("{u:o}"), "oct of {v}");
            assert_eq!(spec("b").apply_int(v), format!("{u:b}"), "bin of {v}");
            for raw in ["x", "X", "o", "b", "08x", "<12x", ">12o"] {
                assert_eq!(
                    spec(raw).apply_int(v),
                    spec(raw).apply_uint(u),
                    "spec {raw:?}: a signed 64-bit hole reinterprets as u64, value {v}"
                );
            }
            // Decimal: sign + magnitude, unpadded.
            assert_eq!(spec("").apply_int(v), format!("{v}"), "dec of {v}");
        }
        // Widening a u64 to u128 changes nothing, at every spec shape.
        for v in [0u64, 1, 255, u64::MAX, u64::MAX - 1, 1 << 63] {
            for raw in ["", "8", "08", "<8", ">8", "x", "X", "o", "b", "020"] {
                assert_eq!(
                    spec(raw).apply_uint(v),
                    spec(raw).apply_uint128(u128::from(v)),
                    "spec {raw:?} value {v}"
                );
            }
        }
        // The same BIT PATTERN read at three (width, signedness) combinations.
        // The first two agree and the third must not join them.
        assert_eq!(spec("x").apply_uint(u64::MAX), "f".repeat(16));
        assert_eq!(spec("x").apply_int(-1), "f".repeat(16));
        assert_eq!(spec("x").apply_int128(-1), "f".repeat(32));
    }

    #[test]
    fn float_precision() {
        assert_eq!(spec(".2").apply_float(1.23456), "1.23");
        assert_eq!(spec(".0").apply_float(3.9), "4");
        assert_eq!(spec("8.2").apply_float(1.23456), "    1.23");
        assert_eq!(spec("08.2").apply_float(1.23456), "00001.23");
    }

    #[test]
    fn string_width_and_align() {
        assert_eq!(spec("5").apply_str("hi"), "   hi");
        assert_eq!(spec(">5").apply_str("hi"), "   hi");
        assert_eq!(spec("<5").apply_str("hi"), "hi   ");
    }

    #[test]
    fn to_printf_maps() {
        assert_eq!(spec("04").to_printf("ll", 'd', true), "%04lld");
        assert_eq!(spec("8.2").to_printf("", 'f', true), "%8.2f");
        assert_eq!(spec("<8.2").to_printf("", 'f', true), "%-8.2f");
        assert_eq!(spec("08x").to_printf("ll", 'x', true), "%08llx");
        assert_eq!(spec("5").to_printf("", 's', false), "%5s");
        assert_eq!(spec("<5").to_printf("", 's', false), "%-5s");
    }

    #[test]
    fn binary_center_and_fill_now_parse_and_render() {
        // Formerly-deferred specs — now parse and render via the `apply_*`
        // helpers (codegen routes them through the shared runtime formatter,
        // the interpreter calls these directly). `needs_runtime_formatter()`
        // is what tells codegen to take that path.
        // Binary radix.
        let b = spec("b");
        assert!(b.needs_runtime_formatter());
        assert_eq!(b.apply_int(5), "101");
        assert_eq!(spec("08b").apply_int(5), "00000101");
        assert_eq!(spec("b").apply_uint(255), "11111111");
        // Center align (default space fill).
        let c = spec("^7");
        assert!(c.needs_runtime_formatter());
        assert_eq!(c.apply_str("hi"), "  hi   ");
        assert_eq!(spec("^6").apply_int(42), "  42  ");
        // Custom fill char + align.
        let f = spec("*^7");
        assert!(f.needs_runtime_formatter());
        assert_eq!(f.apply_str("hi"), "**hi***");
        assert_eq!(spec("*<7").apply_str("hi"), "hi*****");
        assert_eq!(spec("*>7").apply_int(42), "*****42");
        // Custom fill center on a float with precision.
        assert_eq!(spec("*^8.2").apply_float(1.5), "**1.50**");
        // A plain space-fill / left / right / hex spec does NOT need the
        // runtime path (stays on the faster snprintf route).
        assert!(!spec("<5").needs_runtime_formatter());
        assert!(!spec("08x").needs_runtime_formatter());
        assert!(!spec(".2").needs_runtime_formatter());
    }

    /// B-2026-10-02-42 — the `+` sign flag, checked against Rust's own `{:+}`.
    #[test]
    fn plus_sign_flag() {
        for v in [0i64, 5, -3, i64::MAX, i64::MIN] {
            assert_eq!(spec("+").apply_int(v), format!("{v:+}"), "{v}");
            assert_eq!(spec("+06").apply_int(v), format!("{v:+06}"), "{v}");
            assert_eq!(spec("<+6").apply_int(v), format!("{v:<+6}"), "{v}");
            assert_eq!(spec("*^+7").apply_int(v), format!("{v:*^+7}"), "{v}");
            assert_eq!(spec("+x").apply_int(v), format!("{:+x}", v as u64), "{v}");
        }
        assert_eq!(spec("+").apply_uint(7), "+7");
        assert_eq!(
            spec("+").apply_int128(i128::MAX),
            format!("{:+}", i128::MAX)
        );
        assert_eq!(
            spec("+").apply_uint128(u128::MAX),
            format!("{:+}", u128::MAX)
        );
        for v in [
            1.5f64,
            0.0,
            -0.0,
            -0.25,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
        ] {
            assert_eq!(spec("+.2").apply_float(v), format!("{v:+.2}"), "{v}");
            // Rust pads a non-finite value with spaces under `0`; this module
            // zero-pads it, with or without `+`, so compare finite values only.
            if v.is_finite() {
                assert_eq!(spec("+08.2").apply_float(v), format!("{v:+08.2}"), "{v}");
            }
        }
        assert!(spec("+").needs_runtime_formatter());
        assert!(!spec("").plus);
    }

    #[test]
    fn errors() {
        assert!(FormatSpec::parse("q").is_err());
        assert!(FormatSpec::parse(".").is_err());
        assert!(FormatSpec::parse(".2x").is_err());
        assert!(FormatSpec::parse("4z").is_err());
        // B-2026-10-02-42: an unsupported sign flag is named as one.
        for raw in ["-", "-5", " 5"] {
            let e = FormatSpec::parse(raw).unwrap_err();
            assert!(e.contains("sign flag"), "{raw:?}: {e}");
        }
    }

    // Cross-check a couple of integer results against libc printf so the
    // `apply_*` helpers provably match the `snprintf` codegen path. Not built
    // on Windows — bare `snprintf` is unlinkable against MSVC's UCRT (see the
    // call sites in `int_width_and_zero_pad`).
    #[cfg(not(windows))]
    fn format_via_libc_int(fmt: &str, v: i64) -> String {
        use std::ffi::CString;
        extern "C" {
            fn snprintf(buf: *mut u8, size: usize, fmt: *const i8, ...) -> i32;
        }
        let cfmt = CString::new(fmt).unwrap();
        let mut buf = vec![0u8; 64];
        let n = unsafe { snprintf(buf.as_mut_ptr(), 64, cfmt.as_ptr(), v) };
        String::from_utf8_lossy(&buf[..n as usize]).into_owned()
    }
}
