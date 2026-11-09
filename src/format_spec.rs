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

#[path = "float_text.rs"]
pub mod float_text;

use float_text::{Count, Out};

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

/// Why a spec does not parse; byte offsets into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecError {
    Debug,
    NotUtf8,
    Sign(usize),
    Width(usize, usize),
    MissingPrecision,
    Precision(usize, usize),
    Type(usize),
    Trailing(usize),
    PrecisionOnInt,
}

impl SpecError {
    /// The diagnostic for `raw`, the spec that failed.
    pub fn message(&self, raw: &str) -> String {
        let span = |a: usize, b: usize| raw.get(a..b).unwrap_or_default();
        let char_at = |a: usize| span(a, raw.len()).chars().next().unwrap_or('?');
        match *self {
            SpecError::Debug => format!(
                "format spec `{raw}`: `?` is Debug formatting, which is reserved but not \
                 implemented yet; write `{{x}}` for the value's Display form"
            ),
            SpecError::NotUtf8 => "format spec is not UTF-8".to_string(),
            SpecError::Sign(i) => format!(
                "format spec `{raw}`: sign flag `{}` is not supported (only `+` is)",
                char_at(i)
            ),
            SpecError::Width(a, b) => {
                format!("format spec width `{}` is out of range", span(a, b))
            }
            SpecError::MissingPrecision => {
                format!("format spec `{raw}`: `.` must be followed by a precision (e.g. `.2`)")
            }
            SpecError::Precision(a, b) => {
                format!("format spec precision `{}` is out of range", span(a, b))
            }
            SpecError::Type(i) => format!(
                "format spec `{raw}`: unsupported type `{}` \
                 (expected one of x, X, o, b, d)",
                char_at(i)
            ),
            SpecError::Trailing(i) => format!(
                "format spec `{raw}`: unexpected trailing `{}`",
                span(i, raw.len())
            ),
            SpecError::PrecisionOnInt => {
                format!("format spec `{raw}`: precision is not valid with an integer type")
            }
        }
    }
}

impl FormatSpec {
    /// Parse a raw specifier string (the text after the hole's `:`). Returns a
    /// human-readable error for an unrecognized spec.
    pub fn parse(raw: &str) -> Result<FormatSpec, String> {
        Self::parse_bytes(raw.as_bytes()).map_err(|e| e.message(raw))
    }

    /// [`Self::parse`] over the spec's bytes, with nothing in it that can
    /// panic or allocate: the compiled runtime parses a hole's spec through
    /// this, and a Rust panic reachable from a runtime native keeps std's
    /// panic hook in every binary (B-2026-11-06-2).
    pub fn parse_bytes(raw: &[u8]) -> Result<FormatSpec, SpecError> {
        let mut spec = FormatSpec {
            fill: None,
            align: None,
            plus: false,
            zero_pad: false,
            width: None,
            precision: None,
            radix: Radix::Dec,
        };
        // `{x:?}` / `{x:#?}` (with or without width flags before the `?`) is
        // the Debug form design.md § Strings reserves and defers: name it as
        // such rather than letting the `?` reach the `[type]` slot and be
        // reported as an unknown radix (B-2026-10-05-48).
        if raw.last() == Some(&b'?') {
            return Err(SpecError::Debug);
        }
        let text = std::str::from_utf8(raw).map_err(|_| SpecError::NotUtf8)?;
        let align_of = |c: char| match c {
            '<' => Some(Align::Left),
            '>' => Some(Align::Right),
            '^' => Some(Align::Center),
            _ => None,
        };

        // [[fill] align] — a fill char is only recognized when an align char
        // follows it, so `<`/`>`/`^` alone is align-with-default-fill and a
        // leading `0` stays the zero-pad flag. Everything after them is
        // ASCII, so from here `i` counts bytes.
        let mut chars = text.chars();
        let (first, second) = (chars.next(), chars.next());
        let mut i = 0;
        if let (Some(f), Some(a)) = (first, second.and_then(align_of)) {
            spec.fill = Some(f);
            spec.align = Some(a);
            i = f.len_utf8() + 1;
        } else if let Some(a) = first.and_then(align_of) {
            spec.align = Some(a);
            i = 1;
        }
        let at = |i: usize| raw.get(i).copied();
        let digits_end = |mut i: usize| {
            while at(i).is_some_and(|c| c.is_ascii_digit()) {
                i += 1;
            }
            i
        };
        // A run of digits as a `usize`, None when it overflows.
        let number = |a: usize, b: usize| {
            raw.get(a..b)?.iter().try_fold(0usize, |n, c| {
                n.checked_mul(10)?.checked_add(usize::from(c - b'0'))
            })
        };

        // ['+'] sign flag. Rust also accepts `-` (and ignores it) and Python
        // a space; neither is supported, and both used to be reported as an
        // unsupported TYPE because the parser reached the `[type]` slot with
        // them (B-2026-10-02-42), so name them as sign flags instead.
        match at(i) {
            Some(b'+') => {
                spec.plus = true;
                i += 1;
            }
            Some(b'-' | b' ') => return Err(SpecError::Sign(i)),
            _ => {}
        }

        // ['0'] zero-pad flag.
        if at(i) == Some(b'0') {
            spec.zero_pad = true;
            i += 1;
        }

        // [width]
        let width_start = i;
        i = digits_end(i);
        if i > width_start {
            spec.width = Some(number(width_start, i).ok_or(SpecError::Width(width_start, i))?);
        }

        // ['.' precision]
        if at(i) == Some(b'.') {
            i += 1;
            let prec_start = i;
            i = digits_end(i);
            if i == prec_start {
                return Err(SpecError::MissingPrecision);
            }
            spec.precision =
                Some(number(prec_start, i).ok_or(SpecError::Precision(prec_start, i))?);
        }

        // [type]
        if let Some(c) = at(i) {
            spec.radix = match c {
                b'x' => Radix::Hex,
                b'X' => Radix::HexUpper,
                b'o' => Radix::Oct,
                b'b' => Radix::Bin,
                b'd' => Radix::Dec,
                _ => return Err(SpecError::Type(i)),
            };
            i += 1;
        }

        if i != raw.len() {
            return Err(SpecError::Trailing(i));
        }
        if spec.radix != Radix::Dec && spec.precision.is_some() {
            return Err(SpecError::PrecisionOnInt);
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
        let mut out = Vec::new();
        let (left, right) = self.pad_amounts(body.chars().count(), default_left);
        self.write_fill(&mut out, left);
        out.put(body.as_bytes());
        self.write_fill(&mut out, right);
        String::from_utf8(out).unwrap_or_default()
    }

    /// The fill before and after a body `chars` wide, per [`Self::pad`].
    fn pad_amounts(&self, chars: usize, default_left: bool) -> (usize, usize) {
        let pad = self.width.map_or(0, |w| w.saturating_sub(chars));
        let align = self.align.unwrap_or(if default_left {
            Align::Left
        } else {
            Align::Right
        });
        match align {
            Align::Left => (0, pad),
            Align::Right => (pad, 0),
            Align::Center => (pad / 2, pad - pad / 2),
        }
    }

    /// `n` fill chars.
    fn write_fill(&self, out: &mut impl Out, n: usize) {
        let mut fill = [0u8; 4];
        out.repeat(
            self.fill.unwrap_or(' ').encode_utf8(&mut fill).as_bytes(),
            n,
        );
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
        let mut out = Vec::new();
        self.write_magnitude(neg, mag, &mut out);
        String::from_utf8(out).unwrap_or_default()
    }

    /// [`Self::apply_magnitude`], appending to `out` with no allocation of
    /// its own: the compiled runtime formats `{x:b}` through this into one
    /// reused buffer (B-2026-10-29-33).
    pub fn write_magnitude(&self, neg: bool, mag: u128, out: &mut impl Out) {
        let (base, upper) = match self.radix {
            Radix::Dec => (10, false),
            Radix::Hex => (16, false),
            Radix::HexUpper => (16, true),
            Radix::Oct => (8, false),
            Radix::Bin => (2, false),
        };
        let digit = |d: u8| match d {
            0..=9 => b'0' + d,
            _ if upper => b'A' + d - 10,
            _ => b'a' + d - 10,
        };
        let mut buf = [0u8; 128];
        let mut i = buf.len();
        match u64::try_from(mag) {
            Ok(mut m) => {
                while i > 0 {
                    i -= 1;
                    if let Some(slot) = buf.get_mut(i) {
                        *slot = digit((m % base) as u8);
                    }
                    m /= base;
                    if m == 0 {
                        break;
                    }
                }
            }
            Err(_) => {
                // Divided 32 bits at a time, not as a `u128`: 128-bit
                // division is compiler_builtins' `__udivti3`, whose unwind
                // table names Rust's personality routine, and on Mach-O
                // that keeps std's panic machinery in every binary that
                // formats a hole with a spec (B-2026-11-06-2).
                let mut limbs = [
                    (mag >> 96) as u32,
                    (mag >> 64) as u32,
                    (mag >> 32) as u32,
                    mag as u32,
                ];
                while i > 0 {
                    let mut rem = 0u64;
                    for l in limbs.iter_mut() {
                        let cur = rem << 32 | u64::from(*l);
                        *l = (cur / base) as u32;
                        rem = cur % base;
                    }
                    i -= 1;
                    if let Some(slot) = buf.get_mut(i) {
                        *slot = digit(rem as u8);
                    }
                    if limbs.iter().all(|l| *l == 0) {
                        break;
                    }
                }
            }
        }
        let digits = buf.get(i..).unwrap_or_default();
        let sign: &[u8] = if neg {
            b"-"
        } else if self.plus {
            b"+"
        } else {
            b""
        };
        let have = sign.len() + digits.len();
        let pad = self.width.map_or(0, |w| w.saturating_sub(have));
        // Zero-pad inserts zeros between the sign and the digits.
        if self.zero_pad && pad > 0 {
            out.put(sign);
            out.repeat(b"0", pad);
            out.put(digits);
            return;
        }
        let (left, right) = self.pad_amounts(have, false);
        self.write_fill(out, left);
        out.put(sign);
        out.put(digits);
        self.write_fill(out, right);
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

    /// [`Self::apply_int`] (`signed`) or [`Self::apply_uint`] on the 64 bits
    /// `v`, appended to `out`.
    pub fn write_int64(&self, v: u64, signed: bool, out: &mut impl Out) {
        let neg = signed && (v as i64) < 0 && self.radix == Radix::Dec;
        let mag = if neg { (v as i64).unsigned_abs() } else { v };
        self.write_magnitude(neg, u128::from(mag), out);
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

    /// [`Self::apply_int128`] (`signed`) or [`Self::apply_uint128`] on the
    /// 128 bits `v`, appended to `out`.
    pub fn write_int128(&self, v: u128, signed: bool, out: &mut impl Out) {
        let neg = signed && (v as i128) < 0 && self.radix == Radix::Dec;
        let mag = if neg { (v as i128).unsigned_abs() } else { v };
        self.write_magnitude(neg, mag, out);
    }

    /// Format a float. `precision` fixes the fractional digit count (default: the
    /// value's natural rendering). Zero-pad and width apply to the whole number.
    pub fn apply_float(&self, v: f64) -> String {
        let mut out = Vec::new();
        self.write_float(v, &mut out);
        String::from_utf8(out).unwrap_or_default()
    }

    /// [`Self::apply_float`] on an `f32`: without a precision, its own
    /// shortest digits, as its `Display` shows them.
    pub fn apply_float32(&self, v: f32) -> String {
        let mut out = Vec::new();
        self.write_float32(v, &mut out);
        String::from_utf8(out).unwrap_or_default()
    }

    /// [`Self::apply_float32`], appended to `out`.
    pub fn write_float32(&self, v: f32, out: &mut impl Out) {
        self.write_float_as(f64::from(v), true, out)
    }

    /// [`Self::apply_float`], appended to `out`. The digits are Rust's
    /// `{v:.p}` with a precision, and the value's `Display` text without one
    /// (design.md §3: `{x:>8}` pads what `{x}` prints, B-2026-11-08-74);
    /// written by `float_text`, which cannot panic.
    pub fn write_float(&self, v: f64, out: &mut impl Out) {
        self.write_float_as(v, false, out)
    }

    /// [`Self::write_float`], the digits without a precision an `f32`'s
    /// when `single`.
    fn write_float_as(&self, v: f64, single: bool, out: &mut impl Out) {
        // `+` signs every non-negative value except a NaN, which Rust renders
        // unsigned (`format!("{:+}", f64::NAN)` is `NaN`); so does `-`.
        let sign: &[u8] = if v.is_nan() {
            b""
        } else if v.is_sign_negative() {
            b"-"
        } else if self.plus {
            b"+"
        } else {
            b""
        };
        let have = match self.width {
            Some(_) => {
                let mut count = Count(sign.len());
                Self::float_digits(self.precision, v, single, &mut count);
                count.0
            }
            None => 0,
        };
        if self.zero_pad {
            if let Some(width) = self.width {
                if have < width {
                    out.put(sign);
                    out.repeat(b"0", width - have);
                    return Self::float_digits(self.precision, v, single, out);
                }
            }
        }
        let (left, right) = self.pad_amounts(have, false);
        self.write_fill(out, left);
        out.put(sign);
        Self::float_digits(self.precision, v, single, out);
        self.write_fill(out, right);
    }

    /// `v`'s magnitude, as [`Self::write_float`] shows it.
    fn float_digits(precision: Option<usize>, v: f64, single: bool, out: &mut impl Out) {
        match precision {
            Some(p) if v.is_finite() => float_text::write_fixed(out, v, p),
            Some(_) => float_text::write_rust_plain(out, v),
            // The sign is written apart, so the magnitude's `Display`.
            None if single => float_text::write_display_f32(out, v.abs() as f32),
            None => float_text::write_display_f64(out, v.abs()),
        }
    }

    /// Format a string: width padding + align. Precision (truncation) on a
    /// string is a deferred follow-up (byte-vs-char parity with printf `%.*s`
    /// on multibyte UTF-8), rejected at format time, so this ignores it. Width
    /// default is right-align (matching printf `%Ns` and the numeric path — one
    /// consistent default across all value kinds).
    pub fn apply_str(&self, s: &str) -> String {
        self.pad(s, false)
    }

    /// [`Self::apply_str`] on the UTF-8 `s`, appended to `out`.
    pub fn write_str(&self, s: &[u8], out: &mut impl Out) {
        let chars = s.iter().filter(|b| **b & 0xc0 != 0x80).count();
        let (left, right) = self.pad_amounts(chars, false);
        self.write_fill(out, left);
        out.put(s);
        self.write_fill(out, right);
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

/// How `Display` shows a float (core-types.md § Printing): the shortest
/// digits that read back as the same value, always with a `.` or an
/// exponent, so a float never reads as an integer (`1.0`, not `1`). The
/// exponent form is used when the decimal exponent is below -4 or at least
/// 16, as Python's `repr` (`1e+16`, `1e-05`); the special values are `NaN`,
/// `inf` and `-inf`. The MIR interpreter and the compiled MIR backend's
/// runtime both print through this.
///
/// When two equally short decimals both read back and the value lies exactly
/// halfway between them, the one with the even last digit is shown, as
/// Python's `repr` does: -75455689800034.625 prints -75455689800034.62
/// (B-2026-10-23-4). Rust's shortest digits round that last digit up.
pub fn display_f64(v: f64) -> String {
    let mut out = Vec::new();
    float_text::write_display_f64(&mut out, v);
    String::from_utf8(out).unwrap_or_default()
}

/// [`display_f64`] with an `f32`'s own shortest digits.
pub fn display_f32(v: f32) -> String {
    let mut out = Vec::new();
    float_text::write_display_f32(&mut out, v);
    String::from_utf8(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    // The tie cases are exact doubles written out in full: the digits past
    // the shortest ones are what makes each a tie.
    #[allow(clippy::excessive_precision)]
    #[test]
    fn display_f64_follows_the_printing_rules() {
        let cases: [(f64, &str); 18] = [
            (1.0, "1.0"),
            (0.1, "0.1"),
            (2.5, "2.5"),
            (-3.0, "-3.0"),
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (-2.5e300, "-2.5e+300"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.25e-7, "1.25e-07"),
            (5e-324, "5e-324"),
            (123456.789, "123456.789"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
        ];
        for (v, want) in cases {
            assert_eq!(display_f64(v), want, "{v:?}");
        }
        // A tie between two equally short decimals keeps the even last
        // digit (B-2026-10-23-4).
        for (v, want) in [
            (-75455689800034.625, "-75455689800034.62"),
            (75455689800034.125, "75455689800034.12"),
            (75455689800034.375, "75455689800034.38"),
            (75455689800034.875, "75455689800034.88"),
            (0.5, "0.5"),
            (2.5e-5, "2.5e-05"),
        ] {
            assert_eq!(display_f64(v), want, "{v:?}");
        }
        assert_eq!(display_f32(0.1), "0.1");
        assert_eq!(display_f32(2.0), "2.0");
        assert_eq!(display_f32(1e20), "1e+20");
    }

    /// The previous `parse`, `display_f64`, `display_f32` and `apply_float`,
    /// built on core's float formatting (Grisu, Dragon, and `parse` for the
    /// even tie): the oracle `float_text` and `parse_bytes` are held to
    /// (B-2026-11-06-2).
    mod oracle {
        use super::super::{Align, FormatSpec, Radix};

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
            // `{x:?}` / `{x:#?}` (with or without width flags before the `?`) is
            // the Debug form design.md § Strings reserves and defers: name it as
            // such rather than letting the `?` reach the `[type]` slot and be
            // reported as an unknown radix (B-2026-10-05-48).
            if raw.ends_with('?') {
                return Err(format!(
                    "format spec `{raw}`: `?` is Debug formatting, which is reserved but not \
                     implemented yet; write `{{x}}` for the value's Display form"
                ));
            }
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

        pub fn display_f64(v: f64) -> String {
            let sci = format!("{v:e}");
            match even_tie(v, &sci, |t| t.parse::<f64>() == Ok(v)) {
                Some((sci, plain)) => shown_float(v, sci, plain),
                None => shown_float(v, sci, v.to_string()),
            }
        }

        /// [`display_f64`] with an `f32`'s own shortest digits.
        pub fn display_f32(v: f32) -> String {
            let sci = format!("{v:e}");
            match even_tie(f64::from(v), &sci, |t| t.parse::<f32>() == Ok(v)) {
                Some((sci, plain)) => shown_float(f64::from(v), sci, plain),
                None => shown_float(f64::from(v), sci, v.to_string()),
            }
        }

        /// The `{:e}` and plain forms of the even-ending neighbour of `sci`'s
        /// shortest digits, when `v` lies exactly halfway between the two and the
        /// neighbour also `reads_back` as `v`; None otherwise. Only an odd last
        /// digit can lose such a tie, and its neighbours differ in that digit
        /// alone (a 9's upper neighbour would carry to a shorter decimal, which
        /// the shortest digits would already be).
        fn even_tie(
            v: f64,
            sci: &str,
            reads_back: impl Fn(&str) -> bool,
        ) -> Option<(String, String)> {
            if !v.is_finite() || v == 0.0 {
                return None;
            }
            // A cheap necessary condition, which nearly every value fails: `v` is
            // M * 2^q with M odd, so when q < 0 it has exactly -q decimal places,
            // and the midpoint after the n shortest digits has n - exp of them. It
            // is first bounded from the bits alone (n is at most 17, and exp is at
            // least the binary exponent's decimal floor less one), so a typical
            // value is turned away before any text is read.
            let bits = v.abs().to_bits();
            let (m, q, e2) = match (bits >> 52) as i32 {
                0 => (bits, -1074, -1075),
                e => (bits & ((1 << 52) - 1) | 1 << 52, e - 1075, e - 1023),
            };
            let q = q + m.trailing_zeros() as i32;
            if q < 0 && -q > 17 - ((e2 * 30103).div_euclid(100000) - 1) {
                return None;
            }
            let (mantissa, exp) = sci.split_once('e')?;
            let exp: i32 = exp.parse().ok()?;
            let (neg, mantissa) = match mantissa.strip_prefix('-') {
                Some(m) => (true, m),
                None => (false, mantissa),
            };
            if mantissa
                .bytes()
                .last()
                .is_none_or(|b| (b - b'0').is_multiple_of(2))
            {
                return None;
            }
            let n = mantissa.bytes().filter(u8::is_ascii_digit).count() as i32;
            if q < 0 && -q != n - exp {
                return None;
            }
            let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
            let last = digits.bytes().last()? - b'0';
            let stem = &digits[..digits.len() - 1];
            let sign = if neg { "-" } else { "" };
            let sci_of = |d: &str| match d.split_at(1) {
                (lead, "") => format!("{sign}{lead}e{exp}"),
                (lead, rest) => format!("{sign}{lead}.{rest}e{exp}"),
            };
            // `v` is exactly halfway when its exact decimal expansion (finite for a
            // double; 1100 places hold the longest) is the midpoint. Reading the
            // midpoint back as `v` is checked first: it is cheap, and a value that
            // is nowhere near a tie never pays for the expansion.
            for (neighbour, midpoint) in [
                (last - 1, format!("{stem}{}5", last - 1)),
                (last + 1, format!("{stem}{last}5")),
            ] {
                let alt = format!("{stem}{neighbour}");
                if neighbour > 9
                    || sci_of(&midpoint).trim_start_matches('-').parse::<f64>() != Ok(v.abs())
                    || !reads_back(&sci_of(&alt))
                {
                    continue;
                }
                let exact = format!("{:.1100e}", v.abs());
                let (exact_digits, exact_exp) = exact.split_once('e')?;
                let exact_digits: String = exact_digits.chars().filter(|c| *c != '.').collect();
                let halfway = exact_exp.parse::<i32>() == Ok(exp)
                    && exact_digits
                        .strip_prefix(midpoint.as_str())
                        .is_some_and(|rest| rest.bytes().all(|b| b == b'0'));
                if halfway {
                    return Some((sci_of(&alt), plain_of(sign, &alt, exp)));
                }
            }
            None
        }

        /// `digits` (the first one before the point) times ten to `exp`, written
        /// out in decimal.
        fn plain_of(sign: &str, digits: &str, exp: i32) -> String {
            let point = exp + 1;
            if point <= 0 {
                let zeros = "0".repeat(point.unsigned_abs() as usize);
                return format!("{sign}0.{zeros}{digits}");
            }
            let point = point as usize;
            if digits.len() <= point {
                return format!("{sign}{digits}{}", "0".repeat(point - digits.len()));
            }
            format!("{sign}{}.{}", &digits[..point], &digits[point..])
        }

        /// `v` shown from its shortest digits in Rust's `{:e}` (`sci`: `1e16`,
        /// `-1.5e-5`) and `Display` (`plain`: always decimal, `-0` for -0.0) forms.
        fn shown_float(v: f64, sci: String, plain: String) -> String {
            if v.is_nan() {
                return "NaN".to_string();
            }
            if v.is_infinite() {
                return if v > 0.0 { "inf" } else { "-inf" }.to_string();
            }
            if v != 0.0 {
                if let Some((mantissa, exp)) = sci.split_once('e') {
                    let exp: i32 = exp.parse().unwrap_or(0);
                    if !(-4..16).contains(&exp) {
                        let sign = if exp < 0 { '-' } else { '+' };
                        return format!("{mantissa}e{sign}{:02}", exp.unsigned_abs());
                    }
                }
            }
            if plain.contains('.') {
                plain
            } else {
                plain + ".0"
            }
        }

        pub fn apply_float(fs: &FormatSpec, v: f64) -> String {
            let body = match fs.precision {
                Some(p) => format!("{v:.*}", p),
                // The value's `Display` text (design.md §3, B-2026-11-08-74).
                None => display_f64(v),
            };
            // `+` signs every non-negative value except a NaN, which Rust renders
            // unsigned (`format!("{:+}", f64::NAN)` is `NaN`).
            let body = if fs.plus && !body.starts_with('-') && !v.is_nan() {
                format!("+{body}")
            } else {
                body
            };
            if fs.zero_pad {
                if let Some(width) = fs.width {
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
            fs.pad(&body, false)
        }
    }

    /// A xorshift stream, for reproducible values.
    fn stream(mut x: u64) -> impl Iterator<Item = u64> {
        std::iter::repeat_with(move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        })
    }

    /// The floats the oracle tests run: the edges of every kind, short
    /// decimals (where the ties and trailing zeros are), and random bits.
    #[allow(clippy::excessive_precision)]
    fn float_cases() -> Vec<f64> {
        let mut v = vec![
            0.0,
            -0.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MIN_POSITIVE,
            f64::MAX,
            f64::MIN,
            f64::EPSILON,
            5e-324,
            1e-323,
            0.5,
            1.5,
            2.5,
            0.125,
            0.375,
            9.995,
            -75455689800034.625,
            75455689800034.125,
            1e15,
            1e16,
            1e21,
            1e22,
            1e23,
            9007199254740993.0,
            4503599627370495.5,
        ];
        for e in -325..=310 {
            for m in ["1", "5", "123456789", "9.999999999999999"] {
                v.push(format!("{m}e{e}").parse().unwrap());
            }
            v.push(2f64.powi(e));
        }
        let mut r = stream(0x9e3779b97f4a7c15);
        for _ in 0..20000 {
            let n = r.next().unwrap() % 100_000_000;
            let e = (r.next().unwrap() % 40) as i32 - 20;
            v.push(n as f64 * 10f64.powi(e));
            v.push(n as f64 / 1024.0);
            v.push((n % 100_000) as f64 / 8.0 + 0.5);
        }
        for _ in 0..100_000 {
            v.push(f64::from_bits(r.next().unwrap()));
        }
        v
    }

    #[test]
    fn display_matches_the_core_formatting_oracle() {
        for v in float_cases() {
            assert_eq!(
                display_f64(v),
                oracle::display_f64(v),
                "{v:?} ({:#x})",
                v.to_bits()
            );
            let f = v as f32;
            assert_eq!(
                display_f32(f),
                oracle::display_f32(f),
                "{f:?} ({:#x})",
                f.to_bits()
            );
        }
        let mut r = stream(77);
        for _ in 0..100_000 {
            let f = f32::from_bits(r.next().unwrap() as u32);
            assert_eq!(
                display_f32(f),
                oracle::display_f32(f),
                "{f:?} ({:#x})",
                f.to_bits()
            );
        }
    }

    #[test]
    fn apply_float_matches_the_core_formatting_oracle() {
        let specs = [
            "", ".0", ".1", ".2", ".3", ".5", ".9", ".17", ".20", ".40", "10", "10.2", "+08.3",
            "^12", "*<9", "+", "08", "+.0", "é>14.4", "030.1",
        ];
        let specs: Vec<FormatSpec> = specs.iter().map(|s| spec(s)).collect();
        for (n, v) in float_cases().into_iter().enumerate() {
            for fs in &specs {
                assert_eq!(
                    fs.apply_float(v),
                    oracle::apply_float(fs, v),
                    "{fs:?} {v:?}"
                );
            }
            // The long expansions, on a sample.
            if n % 97 == 0 {
                for p in [330, 1074, 1100] {
                    let fs = FormatSpec {
                        precision: Some(p),
                        ..spec("")
                    };
                    assert_eq!(fs.apply_float(v), oracle::apply_float(&fs, v), ".{p} {v:?}");
                }
            }
        }
    }

    /// `parse` (now over `parse_bytes`) accepts what it did, and refuses
    /// what it refused with the same message.
    #[test]
    fn parse_matches_the_previous_parser() {
        let mut raws: Vec<String> = [
            "",
            "?",
            ">5?",
            "#?",
            "-5",
            " 5",
            "-",
            "5.",
            ".",
            "5.x",
            "q",
            "4z",
            "5q",
            "5xx",
            "x.2",
            ".2x",
            "99999999999999999999999",
            ".99999999999999999999999",
            "18446744073709551615",
            "18446744073709551616",
            "é",
            "é<",
            "é<5",
            "<<",
            "<",
            "0<5",
            "00",
            "0",
            "+0",
            "+08.3",
            "*^+012.4",
            "😀>9x",
            "😀",
            ">+5",
            "<-5",
            "5.2d",
            "5.2b",
            "x",
            "X",
            "o",
            "b",
            "d",
            "08b",
            "+x",
            "+05",
            ">8",
            "<8",
            "^9",
            "*>7x",
            "_<6",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let alphabet = [
            '<', '>', '^', '+', '-', '0', '1', '9', '.', 'x', 'b', 'd', '*', 'é', '?',
        ];
        let mut r = stream(5);
        for _ in 0..20000 {
            let len = (r.next().unwrap() % 7) as usize;
            raws.push(
                (0..len)
                    .map(|_| alphabet[(r.next().unwrap() % alphabet.len() as u64) as usize])
                    .collect(),
            );
        }
        for raw in raws {
            assert_eq!(FormatSpec::parse(&raw), oracle::parse(&raw), "{raw:?}");
        }
    }

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
        // B-2026-10-05-48: the reserved Debug forms are named as Debug
        // formatting, not as an unsupported radix.
        for raw in ["?", "#?", ">8?"] {
            let e = FormatSpec::parse(raw).unwrap_err();
            assert!(e.contains("Debug formatting"), "{raw:?}: {e}");
            assert!(!e.contains("unsupported type"), "{raw:?}: {e}");
        }
        // B-2026-10-02-42: an unsupported sign flag is named as one.
        for raw in ["-", "-5", " 5"] {
            let e = FormatSpec::parse(raw).unwrap_err();
            assert!(e.contains("sign flag"), "{raw:?}: {e}");
        }
    }

    /// The allocation-free integer writer (B-2026-10-29-33) renders exactly
    /// what the `format!`-based code it replaced did, which this oracle
    /// keeps: digits by Rust's own radix formatting, zeros after the sign,
    /// otherwise the whole number padded per `align` (default right).
    #[test]
    fn integer_writer_matches_the_format_based_rendering() {
        fn oracle(fs: &FormatSpec, neg: bool, mag: u128) -> String {
            let digits = match fs.radix {
                Radix::Dec => mag.to_string(),
                Radix::Hex => format!("{mag:x}"),
                Radix::HexUpper => format!("{mag:X}"),
                Radix::Oct => format!("{mag:o}"),
                Radix::Bin => format!("{mag:b}"),
            };
            let sign = if neg {
                "-"
            } else if fs.plus {
                "+"
            } else {
                ""
            };
            if fs.zero_pad {
                if let Some(width) = fs.width {
                    let have = sign.len() + digits.chars().count();
                    if have < width {
                        return format!("{sign}{}{digits}", "0".repeat(width - have));
                    }
                }
            }
            fs.pad(&format!("{sign}{digits}"), false)
        }
        let specs = [
            "b", "x", "X", "o", "d", "5", "05", "08b", "+", "+x", "+05", ">8", "<8", "^9", "*>7x",
            "_<6", "é^7b", "+010X", "3", "040b",
        ];
        let values: [i64; 9] = [0, 1, -1, 7, -7, 255, 400, i64::MIN, i64::MAX];
        for raw in specs {
            let fs = FormatSpec::parse(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
            for v in values {
                let dec = fs.radix == Radix::Dec;
                let mag = if dec {
                    u128::from(v.unsigned_abs())
                } else {
                    u128::from(v as u64)
                };
                let want = oracle(&fs, v < 0 && dec, mag);
                assert_eq!(fs.apply_int(v), want, "{raw} on {v}");
                let mut out = Vec::new();
                fs.write_int64(v as u64, true, &mut out);
                assert_eq!(String::from_utf8(out).unwrap(), want, "{raw} on {v}");
                let mut out = Vec::new();
                fs.write_int64(v as u64, false, &mut out);
                assert_eq!(
                    String::from_utf8(out).unwrap(),
                    oracle(&fs, false, u128::from(v as u64)),
                    "{raw} on {v} unsigned"
                );
            }
            for v in [u128::MAX, 1 << 100, 12345] {
                assert_eq!(fs.apply_uint128(v), oracle(&fs, false, v), "{raw} on {v}");
            }
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
