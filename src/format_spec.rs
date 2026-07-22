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
//! spec   := [[fill] align] ['0'] [width] ['.' precision] [type]
//! align  := '<' | '>' | '^'
//! width  := DIGIT+
//! prec   := DIGIT+
//! type   := 'x' | 'X' | 'o' | 'b' | 'd'
//! ```
//!
//! `fill` is any single char and requires an explicit `align` after it (so a
//! bare `0` stays the zero-pad flag, not a fill char). Unrecognized specs are a
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
    pub fn needs_runtime_formatter(&self) -> bool {
        self.align == Some(Align::Center)
            || self.radix == Radix::Bin
            || (self.fill.is_some() && self.fill != Some(' '))
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

    fn render_int_magnitude(&self, mag: u64) -> String {
        match self.radix {
            Radix::Dec => mag.to_string(),
            Radix::Hex => format!("{mag:x}"),
            Radix::HexUpper => format!("{mag:X}"),
            Radix::Oct => format!("{mag:o}"),
            Radix::Bin => format!("{mag:b}"),
        }
    }

    /// Format a signed integer. Zero-pad inserts zeros between the sign and the
    /// digits (`{-7:05}` -> `-0007`); otherwise the whole rendered number is
    /// padded per `align` (default right).
    pub fn apply_int(&self, v: i64) -> String {
        let neg = v < 0 && self.radix == Radix::Dec;
        let mag = if self.radix == Radix::Dec {
            (v as i128).unsigned_abs() as u64
        } else {
            v as u64
        };
        let digits = self.render_int_magnitude(mag);
        let sign = if neg { "-" } else { "" };
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

    /// Format an unsigned integer (same rules, never negative).
    pub fn apply_uint(&self, v: u64) -> String {
        let digits = self.render_int_magnitude(v);
        if self.zero_pad {
            if let Some(width) = self.width {
                let have = digits.chars().count();
                if have < width {
                    let zeros = "0".repeat(width - have);
                    return format!("{zeros}{digits}");
                }
            }
        }
        self.pad(&digits, false)
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
        if self.zero_pad {
            if let Some(width) = self.width {
                let neg = body.starts_with('-');
                let (sign, rest) = if neg {
                    ("-", &body[1..])
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
        // Matches printf `%4lld` / `%04lld` / `%-4lld`.
        assert_eq!(spec("4").apply_int(7), format_via_libc_int("%4lld", 7));
        assert_eq!(spec("04").apply_int(7), format_via_libc_int("%04lld", 7));
    }

    #[test]
    fn int_radix() {
        assert_eq!(spec("x").apply_int(255), "ff");
        assert_eq!(spec("X").apply_int(255), "FF");
        assert_eq!(spec("o").apply_int(8), "10");
        assert_eq!(spec("08x").apply_int(255), "000000ff");
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

    #[test]
    fn errors() {
        assert!(FormatSpec::parse("q").is_err());
        assert!(FormatSpec::parse(".").is_err());
        assert!(FormatSpec::parse(".2x").is_err());
        assert!(FormatSpec::parse("4z").is_err());
    }

    // Cross-check a couple of integer results against libc printf so the
    // `apply_*` helpers provably match the `snprintf` codegen path.
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
