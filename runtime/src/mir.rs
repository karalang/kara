//! Natives for the MIR backend (`src/codegen/mir/`): printing, process
//! exit, and the string and vector operations its compiled bodies call.
//!
//! The values are laid out as `src/mir/layout.rs` says: a `String` or
//! `Vec[T]` is `{ptr, len, cap}` over a `malloc` buffer (`cap == 0` owns no
//! buffer, which is how a literal's bytes are borrowed), a `str` is
//! `{ptr, len}`. The interpreter's natives (`src/mir/interp.rs`) are the
//! reference semantics for each entry point here.

use std::cell::RefCell;
use std::io::Write;

use crate::alloc::karac_realloc_or_panic;

/// A `String` or `Vec[T]` as compiled code holds it.
#[repr(C)]
pub struct RawVec {
    pub ptr: *mut u8,
    pub len: usize,
    pub cap: usize,
}

thread_local! {
    /// The text being formatted: a `println` or a `format` pushes where
    /// its text starts, appends its parts, and pops it off the end, so
    /// nested formats share one buffer and a format allocates nothing but
    /// its result.
    static FMT: RefCell<FmtBuf> = const {
        RefCell::new(FmtBuf {
            buf: Vec::new(),
            starts: Vec::new(),
        })
    };
    /// Standard output, flushed at exit, at an abort, and before anything
    /// is written to standard error so the two stay in order on a terminal.
    static OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// The message `panic(msg)` set for the abort that follows it.
    static PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

const OUT_LIMIT: usize = 1 << 16;

struct FmtBuf {
    buf: Vec<u8>,
    starts: Vec<usize>,
}

/// Pops the innermost format's text, handing it to `f`.
fn fmt_pop<T>(f: impl FnOnce(&[u8]) -> T) -> T {
    FMT.with(|b| {
        let mut b = b.borrow_mut();
        let start = b.starts.pop().unwrap_or(b.buf.len());
        let out = f(&b.buf[start..]);
        b.buf.truncate(start);
        out
    })
}

pub(crate) fn out_flush() {
    OUT.with(|o| {
        let mut o = o.borrow_mut();
        if !o.is_empty() {
            let mut so = std::io::stdout().lock();
            let _ = so.write_all(&o);
            let _ = so.flush();
            o.clear();
        }
    });
}

fn write(stderr: bool, bytes: &[u8]) {
    if stderr {
        out_flush();
        let _ = std::io::stderr().write_all(bytes);
    } else {
        OUT.with(|o| {
            let mut o = o.borrow_mut();
            o.extend_from_slice(bytes);
            // A terminal sees each line as it ends (docs/library/io.md §
            // Buffering); a pipe or file gets the block.
            if o.len() >= OUT_LIMIT || (bytes.contains(&b'\n') && stdout_is_terminal()) {
                drop(o);
                out_flush();
            }
        });
    }
}

/// Whether standard output is a terminal, asked once.
fn stdout_is_terminal() -> bool {
    use std::io::IsTerminal;
    static TERMINAL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TERMINAL.get_or_init(|| std::io::stdout().is_terminal())
}

/// # Safety
/// `ptr` is readable for `len` bytes (or `len` is 0).
unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 {
        &[]
    } else {
        // SAFETY: the caller's contract.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

fn append(s: &str) {
    append_bytes(s.as_bytes());
}

fn append_bytes(s: &[u8]) {
    FMT.with(|f| {
        let mut f = f.borrow_mut();
        if f.starts.is_empty() {
            drop(f);
            write(false, s);
        } else {
            f.buf.extend_from_slice(s);
        }
    });
}

/// The decimal digits of `v`, at the end of `out`.
fn digits(mut v: u64, out: &mut [u8; 20]) -> usize {
    let mut i = out.len();
    loop {
        i -= 1;
        out[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            return i;
        }
    }
}

/// Writes what is buffered for standard output.
#[no_mangle]
pub extern "C" fn karac_mir_flush() {
    out_flush();
}

/// Starts formatting a `println` / `print` / `format`.
#[no_mangle]
pub extern "C" fn karac_mir_fmt_begin() {
    FMT.with(|f| {
        let mut f = f.borrow_mut();
        let at = f.buf.len();
        f.starts.push(at);
    });
}

/// # Safety
/// `ptr` is readable for `len` bytes of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_fmt_str(ptr: *const u8, len: usize) {
    // SAFETY: the caller's contract; a `str` holds UTF-8.
    append_bytes(unsafe { bytes(ptr, len) });
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_i64(v: i64) {
    let mut d = [0u8; 20];
    let i = digits(v.unsigned_abs(), &mut d);
    if v < 0 {
        d[i - 1] = b'-';
        append_bytes(&d[i - 1..]);
    } else {
        append_bytes(&d[i..]);
    }
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_u64(v: u64) {
    let mut d = [0u8; 20];
    let i = digits(v, &mut d);
    append_bytes(&d[i..]);
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_i128(lo: u64, hi: u64, signed: bool) {
    let bits = (u128::from(hi) << 64) | u128::from(lo);
    append(&if signed {
        (bits as i128).to_string()
    } else {
        bits.to_string()
    });
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_f64(v: f64) {
    append(&crate::fmt::format_spec::display_f64(v));
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_f32(v: f32) {
    append(&crate::fmt::format_spec::display_f32(v));
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_bool(v: u8) {
    append(if v != 0 { "true" } else { "false" });
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_char(c: u32) {
    let mut b = [0u8; 4];
    append(char::from_u32(c).unwrap_or('\u{fffd}').encode_utf8(&mut b));
}

/// Ends a `println` (bit 0 of `flags`: add a newline; bit 1: to standard
/// error).
#[no_mangle]
pub extern "C" fn karac_mir_print_end(flags: u32) {
    if flags & 1 != 0 {
        FMT.with(|f| {
            let mut f = f.borrow_mut();
            if f.starts.is_empty() {
                let at = f.buf.len();
                f.starts.push(at);
            }
            f.buf.push(b'\n');
        });
    }
    fmt_pop(|text| write(flags & 2 != 0, text));
}

/// Ends a `format` into the `String` at `out`.
///
/// # Safety
/// `out` is writable for a `String`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_fmt_end_string(out: *mut RawVec) {
    // SAFETY: the caller's contract.
    fmt_pop(|text| unsafe { string_of(out, text) });
}

/// One f-string hole with a spec (`{x:.3}`, `{n:04}`, `{s:>10}`): the
/// value, rendered by `kind` (0 a signed integer `lo`, 1 an unsigned one,
/// 2 a 128-bit signed `hi:lo`, 3 an unsigned one, 4 the float `f`, 5 the
/// text the innermost format buffer holds, which is popped) through the
/// interpreter's own `FormatSpec`, as a `String` at `out`.
///
/// # Safety
/// `spec` is readable for `spec_len` bytes; `out` is writable for a
/// `String`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_fmt_spec(
    out: *mut RawVec,
    spec: *const u8,
    spec_len: usize,
    kind: i32,
    lo: u64,
    hi: u64,
    f: f64,
) {
    use crate::fmt::format_spec::FormatSpec;
    // SAFETY: the caller's contract.
    let spec_bytes = unsafe { bytes(spec, spec_len) };
    if kind == 0 || kind == 1 {
        if let Some(text) = padded_int(spec_bytes, lo, kind == 0) {
            // SAFETY: the caller's contract.
            unsafe { string_of(out, &text) };
            return;
        }
    }
    let raw = String::from_utf8_lossy(spec_bytes).into_owned();
    let wide = (u128::from(hi) << 64) | u128::from(lo);
    let text = match FormatSpec::parse(&raw) {
        Err(e) => format!("<bad format spec: {e}>"),
        Ok(fs) => match kind {
            0 => fs.apply_int(lo as i64),
            1 => fs.apply_uint(lo),
            2 => fs.apply_int128(wide as i128),
            3 => fs.apply_uint128(wide),
            4 => fs.apply_float(f),
            _ => {
                let shown = fmt_pop(|t| String::from_utf8_lossy(t).into_owned());
                fs.apply_str(&shown)
            }
        },
    };
    // SAFETY: the caller's contract.
    unsafe { string_of(out, text.as_bytes()) };
}

/// `{v:N}` and `{v:0N}` for a 64-bit integer (`signed` says how to
/// read `v`), without parsing a `FormatSpec`: right-aligned in `N`
/// columns, padded with spaces, or with zeros after the sign. `None`
/// for any other spec, which takes the general path.
fn padded_int(spec: &[u8], v: u64, signed: bool) -> Option<Vec<u8>> {
    if spec.is_empty() || spec.len() > 3 || !spec.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let zero = spec.len() > 1 && spec[0] == b'0';
    let width = spec
        .iter()
        .fold(0usize, |w, c| w * 10 + usize::from(c - b'0'));
    let neg = signed && (v as i64) < 0;
    let mag = if signed { (v as i64).unsigned_abs() } else { v };
    let mut d = [0u8; 20];
    let i = digits(mag, &mut d);
    let body = &d[i..];
    let len = body.len() + usize::from(neg);
    let pad = width.saturating_sub(len);
    let mut t = Vec::with_capacity(len + pad);
    if zero {
        if neg {
            t.push(b'-');
        }
        t.resize(t.len() + pad, b'0');
    } else {
        t.resize(pad, b' ');
        if neg {
            t.push(b'-');
        }
    }
    t.extend_from_slice(body);
    Some(t)
}

/// Writes a fresh `String` holding `b` at `out`.
///
/// # Safety
/// `out` is writable for a `String`.
unsafe fn string_of(out: *mut RawVec, b: &[u8]) {
    let ptr = if b.is_empty() {
        std::ptr::null_mut()
    } else {
        let p = karac_realloc_or_panic(std::ptr::null_mut(), b.len());
        // SAFETY: `p` has room for `b.len()` bytes.
        unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), p, b.len()) };
        p
    };
    // SAFETY: the caller's contract.
    unsafe {
        out.write(RawVec {
            ptr,
            len: b.len(),
            cap: b.len(),
        })
    };
}

/// `f"{v}"` for a signed integer `v`, as a `String` at `out`.
///
/// # Safety
/// `out` is writable for a `String`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_string_of_i64(out: *mut RawVec, v: i64) {
    let mut d = [0u8; 20];
    let mut i = digits(v.unsigned_abs(), &mut d);
    if v < 0 {
        i -= 1;
        d[i] = b'-';
    }
    // SAFETY: the caller's contract.
    unsafe { string_of(out, &d[i..]) };
}

/// `f"{v}"` for an unsigned integer `v`, as a `String` at `out`.
///
/// # Safety
/// `out` is writable for a `String`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_string_of_u64(out: *mut RawVec, v: u64) {
    let mut d = [0u8; 20];
    let i = digits(v, &mut d);
    // SAFETY: the caller's contract.
    unsafe { string_of(out, &d[i..]) };
}

/// `String.from(s)`: a copy of the `str` `ptr[..len]`.
///
/// # Safety
/// `out` is writable for a `String`; `ptr` readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_string_from(out: *mut RawVec, ptr: *const u8, len: usize) {
    // SAFETY: the caller's contract.
    unsafe { string_of(out, bytes(ptr, len)) };
}

/// Makes room for `more` elements of `size` bytes in the vector at `v`.
///
/// # Safety
/// `v` is a live `String` or `Vec` of `size`-byte elements.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_vec_reserve(v: *mut RawVec, more: usize, size: usize) {
    // SAFETY: the caller's contract.
    let v = unsafe { &mut *v };
    let want = v.len.checked_add(more).expect("capacity overflow");
    if want <= v.cap || size == 0 {
        if size == 0 {
            v.cap = usize::MAX;
        }
        return;
    }
    // At least 8 bytes' worth of elements up front, as Rust's own `Vec`
    // does for bytes, so a short `String` grows once rather than twice.
    let cap = want
        .max(v.cap.saturating_mul(2))
        .max(if size == 1 { 8 } else { 4 });
    let old = if v.cap == 0 && v.len > 0 {
        // A borrowed literal: copy it out before growing.
        let p = karac_realloc_or_panic(std::ptr::null_mut(), cap * size);
        // SAFETY: the literal holds `len` elements; `p` has room.
        unsafe { std::ptr::copy_nonoverlapping(v.ptr, p, v.len * size) };
        v.ptr = p;
        v.cap = cap;
        return;
    } else if v.cap == 0 {
        std::ptr::null_mut()
    } else {
        v.ptr
    };
    v.ptr = karac_realloc_or_panic(old, cap * size);
    v.cap = cap;
}

/// Writes a `Vec` of `n` uninitialized `size`-byte elements at `out`
/// (`len == cap == n`), for the caller to fill.
///
/// # Safety
/// `out` is writable for a `Vec`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_vec_alloc(out: *mut RawVec, n: usize, size: usize) {
    let bytes = n.checked_mul(size).expect("capacity overflow");
    let ptr = if bytes == 0 {
        std::ptr::null_mut()
    } else {
        karac_realloc_or_panic(std::ptr::null_mut(), bytes)
    };
    let cap = if bytes == 0 { 0 } else { n };
    // SAFETY: the caller's contract.
    unsafe { out.write(RawVec { ptr, len: n, cap }) };
}

/// Whether `a[..an]` and `b[..bn]` hold the same bytes.
///
/// # Safety
/// Each pointer is readable for its length.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_bytes_eq(
    a: *const u8,
    an: usize,
    b: *const u8,
    bn: usize,
) -> u8 {
    // SAFETY: the caller's contract.
    u8::from(unsafe { bytes(a, an) == bytes(b, bn) })
}

/// The order of `a[..an]` and `b[..bn]` as byte strings: -1, 0 or 1.
///
/// # Safety
/// Each pointer is readable for its length.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_bytes_cmp(
    a: *const u8,
    an: usize,
    b: *const u8,
    bn: usize,
) -> i32 {
    // SAFETY: the caller's contract.
    match unsafe { bytes(a, an).cmp(bytes(b, bn)) } {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// Appends the `size` bytes at `elem` to the vector at `v`.
///
/// # Safety
/// As [`karac_mir_vec_reserve`]; `elem` readable for `size` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_vec_push(v: *mut RawVec, elem: *const u8, size: usize) {
    // SAFETY: the caller's contract.
    unsafe {
        karac_mir_vec_reserve(v, 1, size);
        let v = &mut *v;
        if size != 0 {
            std::ptr::copy_nonoverlapping(elem, v.ptr.add(v.len * size), size);
        }
        v.len += 1;
    }
}

/// Appends the bytes `ptr[..len]` to the `String` at `s`.
///
/// # Safety
/// `s` is a live `String`; `ptr` readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_string_push_str(s: *mut RawVec, ptr: *const u8, len: usize) {
    // SAFETY: the caller's contract.
    unsafe {
        karac_mir_vec_reserve(s, len, 1);
        let v = &mut *s;
        if len != 0 {
            std::ptr::copy_nonoverlapping(ptr, v.ptr.add(v.len), len);
        }
        v.len += len;
    }
}

/// Appends the UTF-8 of the char `c` to the `String` at `s`.
///
/// # Safety
/// `s` is a live `String`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_string_push_char(s: *mut RawVec, c: u32) {
    let mut buf = [0u8; 4];
    let t = char::from_u32(c)
        .unwrap_or('\u{fffd}')
        .encode_utf8(&mut buf);
    // SAFETY: the caller's contract.
    unsafe { karac_mir_string_push_str(s, t.as_ptr(), t.len()) };
}

/// # Safety
/// `ptr` is readable for `len` bytes of UTF-8.
unsafe fn text<'a>(ptr: *const u8, len: usize) -> &'a str {
    // SAFETY: the caller's contract; a `str` or `String` holds UTF-8.
    unsafe { std::str::from_utf8_unchecked(bytes(ptr, len)) }
}

/// A test of `a` against `b`: 0 `contains`, 1 `starts_with`, 2
/// `ends_with`, 3 `a < b`.
///
/// # Safety
/// Each pointer is readable for its length, of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_test(
    op: i32,
    a: *const u8,
    an: usize,
    b: *const u8,
    bn: usize,
) -> u8 {
    // SAFETY: the caller's contract.
    let (a, b) = unsafe { (text(a, an), text(b, bn)) };
    u8::from(match op {
        0 => a.contains(b),
        1 => a.starts_with(b),
        2 => a.ends_with(b),
        _ => a < b,
    })
}

/// A new `String` at `out` from `s`: 0 `trim`, 1 `to_uppercase`, 2
/// `to_lowercase`, 3 `trim_start`, 4 `trim_end`, 5 its chars sorted.
///
/// # Safety
/// `out` is writable for a `String`; `s` readable for `n` bytes of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_map(out: *mut RawVec, op: i32, s: *const u8, n: usize) {
    // SAFETY: the caller's contract.
    let s = unsafe { text(s, n) };
    // The trims and an ASCII sort build the result in place, with no
    // string in between.
    let view = match op {
        0 => Some(s.trim()),
        3 => Some(s.trim_start()),
        4 => Some(s.trim_end()),
        5 if s.is_ascii() => Some(s),
        _ => None,
    };
    if let Some(v) = view {
        // SAFETY: as above.
        unsafe {
            string_of(out, v.as_bytes());
            if op == 5 && !(*out).ptr.is_null() {
                std::slice::from_raw_parts_mut((*out).ptr, (*out).len).sort_unstable();
            }
        }
        return;
    }
    let t = match op {
        1 => s.to_uppercase(),
        2 => s.to_lowercase(),
        _ => {
            let mut cs: Vec<char> = s.chars().collect();
            cs.sort_unstable();
            cs.into_iter().collect()
        }
    };
    // SAFETY: as above.
    unsafe { string_of(out, t.as_bytes()) };
}

/// The bytes `lo..hi` of `s` that mode `mode` picks, or `None` for a
/// panic. Mode 0 is `substring`, which saturates (an end past the text is
/// its end, an empty or inverted range is empty); mode 1 is `s[lo..hi]`,
/// which panics when out of range or inverted; both panic off a char
/// boundary. Mode 2 is `String.slice`: a start out of range is empty, the
/// end clamps, and a cut inside a char is lossy, so its text may be new.
/// The message of a string cut [`str_range`] refuses, as the interpreter
/// words it, for the abort that follows.
fn str_range_panic(s: &str, lo: i64, hi: i64) {
    let len = s.len() as i64;
    let msg = if !(0 <= lo && lo <= hi && hi <= len) {
        format!("string slice bounds {lo}..{hi} out of range (len {len})")
    } else {
        format!(
            "E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY: byte range {lo}..{hi} does not fall on \
             UTF-8 char boundaries of {s:?}"
        )
    };
    PANIC.with(|p| *p.borrow_mut() = Some(msg));
}

fn str_range(s: &str, lo: i64, hi: i64, mode: i32) -> Option<std::borrow::Cow<'_, str>> {
    let len = s.len() as i64;
    if mode == 2 {
        if lo < 0 || lo > len {
            return Some("".into());
        }
        let end = hi.clamp(lo, len);
        return Some(String::from_utf8_lossy(
            &s.as_bytes()[lo as usize..end as usize],
        ));
    }
    let (lo, hi) = if mode == 0 {
        let hi = hi.min(len);
        if lo < 0 || lo >= hi {
            return Some("".into());
        }
        (lo, hi)
    } else {
        if !(0 <= lo && lo <= hi && hi <= len) {
            return None;
        }
        (lo, hi)
    };
    s.get(lo as usize..hi as usize).map(Into::into)
}

/// The text [`str_range`] picks as a new `String` at `out`. Returns 1 for
/// a panic, with nothing written.
///
/// # Safety
/// `out` is writable for a `String`; `s` readable for `n` bytes of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_slice(
    out: *mut RawVec,
    s: *const u8,
    n: usize,
    lo: i64,
    hi: i64,
    mode: i32,
) -> u8 {
    // SAFETY: the caller's contract.
    let s = unsafe { text(s, n) };
    match str_range(s, lo, hi, mode) {
        // SAFETY: as above.
        Some(t) => unsafe { string_of(out, t.as_bytes()) },
        None => {
            str_range_panic(s, lo, hi);
            return 1;
        }
    }
    0
}

/// The text [`str_range`] picks as a `str` view `{ptr, len}` at `out`:
/// a view into `s` itself, so nothing to drop. Text that is new (a
/// `String.slice` cut inside a char, made lossy) is leaked, as a view
/// cannot own it. Returns 1 for a panic, with nothing written.
///
/// # Safety
/// `out` is writable for two words; `s` readable for `n` bytes of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_view(
    out: *mut usize,
    s: *const u8,
    n: usize,
    lo: i64,
    hi: i64,
    mode: i32,
) -> u8 {
    // SAFETY: the caller's contract.
    let s = unsafe { text(s, n) };
    let (p, len) = match str_range(s, lo, hi, mode) {
        Some(std::borrow::Cow::Borrowed(t)) => (t.as_ptr(), t.len()),
        Some(std::borrow::Cow::Owned(t)) => {
            let t = t.leak();
            (t.as_ptr(), t.len())
        }
        None => {
            str_range_panic(s, lo, hi);
            return 1;
        }
    };
    // SAFETY: as above.
    unsafe {
        out.write(p as usize);
        out.add(1).write(len);
    }
    0
}

/// The char starting at byte `i` of `s`, or -1 (a panic) when `i` is out
/// of range or not on a char boundary.
///
/// # Safety
/// `s` is readable for `n` bytes of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_char_at_byte(s: *const u8, n: usize, i: i64) -> i64 {
    // SAFETY: the caller's contract.
    let s = unsafe { text(s, n) };
    if i < 0 || i as usize >= s.len() {
        return -1;
    }
    match s.get(i as usize..).and_then(|t| t.chars().next()) {
        Some(c) => i64::from(u32::from(c)),
        None => -1,
    }
}

/// The char ending at byte `i` of `s` (the one whose last byte is `i - 1`),
/// or -1 (a panic) when `i` is out of range or not on a char boundary. The
/// back step of `chars()` (`StrChars.next_back`).
///
/// # Safety
/// `s` is readable for `n` bytes of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_char_before_byte(s: *const u8, n: usize, i: i64) -> i64 {
    // SAFETY: the caller's contract.
    let s = unsafe { text(s, n) };
    if i <= 0 || i as usize > s.len() {
        return -1;
    }
    match s.get(..i as usize).and_then(|t| t.chars().next_back()) {
        Some(c) => i64::from(u32::from(c)),
        None => -1,
    }
}

/// `String.from_utf8`: the bytes as a new `String` at `out` and 0, or 1
/// when the input ends inside a sequence and 2 for an invalid byte, with
/// nothing written.
///
/// # Safety
/// `out` is writable for a `String`; `p` readable for `n` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_from_utf8(out: *mut RawVec, p: *const u8, n: usize) -> i32 {
    // SAFETY: the caller's contract.
    let b = unsafe { bytes(p, n) };
    match std::str::from_utf8(b) {
        Ok(_) => {
            // SAFETY: as above.
            unsafe { string_of(out, b) };
            0
        }
        Err(e) if e.error_len().is_none() => 1,
        Err(_) => 2,
    }
}

/// `s.replace(a, b)` as a new `String` at `out`.
///
/// # Safety
/// `out` is writable for a `String`; each pointer readable for its length.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_replace(
    out: *mut RawVec,
    s: *const u8,
    sn: usize,
    a: *const u8,
    an: usize,
    b: *const u8,
    bn: usize,
) {
    // SAFETY: the caller's contract.
    let t = unsafe { text(s, sn).replace(text(a, an), text(b, bn)) };
    // SAFETY: as above.
    unsafe { string_of(out, t.as_bytes()) };
}

/// The byte offset of the first `needle` in `s`, or -1. A non-negative
/// `c` is a char needle and `needle` is unread.
///
/// # Safety
/// Each pointer is readable for its length, of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_find(
    s: *const u8,
    sn: usize,
    needle: *const u8,
    nn: usize,
    c: i64,
) -> i64 {
    // SAFETY: the caller's contract.
    let s = unsafe { text(s, sn) };
    let at = if c >= 0 {
        char::from_u32(c as u32).and_then(|c| s.find(c))
    } else {
        // SAFETY: as above.
        s.find(unsafe { text(needle, nn) })
    };
    at.map_or(-1, |b| b as i64)
}

/// The `i`-th char of `s` (op 0), or -1 past the end or below zero; or
/// the count of its chars (op 1).
///
/// # Safety
/// `s` is readable for `n` bytes of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_chars(op: i32, s: *const u8, n: usize, i: i64) -> i64 {
    // SAFETY: the caller's contract.
    let s = unsafe { text(s, n) };
    if op == 1 {
        return s.chars().count() as i64;
    }
    usize::try_from(i)
        .ok()
        .and_then(|i| s.chars().nth(i))
        .map_or(-1, |c| i64::from(u32::from(c)))
}

/// `s.strip_prefix(p)` (op 0) or `strip_suffix` (op 1): 1 with the rest
/// as a new `String` at `out`, or 0 with nothing written.
///
/// # Safety
/// `out` is writable for a `String`; each pointer readable for its length.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_strip(
    out: *mut RawVec,
    op: i32,
    s: *const u8,
    sn: usize,
    p: *const u8,
    pn: usize,
) -> u8 {
    // SAFETY: the caller's contract.
    let (s, p) = unsafe { (text(s, sn), text(p, pn)) };
    let rest = if op == 0 {
        s.strip_prefix(p)
    } else {
        s.strip_suffix(p)
    };
    match rest {
        Some(t) => {
            // SAFETY: as above.
            unsafe { string_of(out, t.as_bytes()) };
            1
        }
        None => 0,
    }
}

/// `s.replacen(a, b, n)` (a negative count is 0) as a new `String` at
/// `out`.
///
/// # Safety
/// `out` is writable for a `String`; each pointer readable for its length.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn karac_mir_str_replacen(
    out: *mut RawVec,
    s: *const u8,
    sn: usize,
    a: *const u8,
    an: usize,
    b: *const u8,
    bn: usize,
    n: i64,
) {
    // SAFETY: the caller's contract.
    let t = unsafe { text(s, sn).replacen(text(a, an), text(b, bn), n.max(0) as usize) };
    // SAFETY: as above.
    unsafe { string_of(out, t.as_bytes()) };
}

/// Runs a program's `main` body on a 1 GiB stack (reserved, committed as
/// it is touched) and returns its exit code, so recursion reaches the depth
/// the interpreter's does.
///
/// Where it can, it switches the main thread onto that stack rather than
/// starting a thread: once a process has a second thread, glibc's `malloc`
/// and `free` lock their arena on every call, which made an allocating
/// program (a linked-list merge) 1.7x slower than on the process's own
/// thread. The stack-overflow guard covers the new stack.
#[no_mangle]
pub extern "C" fn karac_mir_run_main(body: extern "C" fn() -> i32) -> i32 {
    #[cfg(all(
        unix,
        feature = "net",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    if !big_stack::under_sanitizer() {
        if let Some(code) = big_stack::run(body) {
            return code;
        }
    }
    match std::thread::Builder::new()
        .name("main".into())
        .stack_size(1 << 30)
        .spawn(move || body())
    {
        Ok(h) => h.join().unwrap_or(101),
        // No thread to be had: run it here, on the process's own stack.
        Err(_) => body(),
    }
}

#[cfg(all(
    unix,
    feature = "net",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod big_stack {
    const SIZE: usize = 1 << 30;
    /// The inaccessible low end, which an overflow faults on.
    const GUARD: usize = 1 << 16;

    /// Whether AddressSanitizer is linked in, which `__mir-llvm-asan` does.
    /// LeakSanitizer reports nothing allocated while the stack pointer is on
    /// a stack it was not told about, so a lane run on this stack could not
    /// see a leak (B-2026-10-18-31); there the thread path runs `main`
    /// instead, whose stack the sanitizer knows.
    pub(super) fn under_sanitizer() -> bool {
        // SAFETY: a lookup of a NUL-terminated name in the loaded objects.
        !unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"__asan_init".as_ptr()) }.is_null()
    }

    /// Runs `body` on a fresh stack; `None` when none could be mapped.
    pub(super) fn run(body: extern "C" fn() -> i32) -> Option<i32> {
        // SAFETY: an anonymous private mapping, reserved, not committed.
        let lo = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if lo == libc::MAP_FAILED {
            return None;
        }
        // SAFETY: the low pages of the mapping just made.
        unsafe { libc::mprotect(lo, GUARD, libc::PROT_NONE) };
        let lo = lo as usize;
        let top = lo + SIZE;
        crate::stack_guard::imp::install_for(Some((lo + GUARD, top)));
        // SAFETY: `top` is the 16-byte-aligned end of a mapping that
        // nothing else uses; `body` runs on it with the C ABI and returns,
        // and the callee-saved register holding the old stack pointer
        // brings it back. The mapping is left in place: the process exits
        // after `main` returns.
        let code: i32;
        #[cfg(target_arch = "x86_64")]
        unsafe {
            std::arch::asm!(
                "mov r12, rsp",
                "mov rsp, {top}",
                "call {f}",
                "mov rsp, r12",
                top = in(reg) top,
                f = in(reg) body,
                out("r12") _,
                lateout("eax") code,
                clobber_abi("C"),
            );
        }
        #[cfg(target_arch = "aarch64")]
        unsafe {
            std::arch::asm!(
                "mov x20, sp",
                "mov sp, {top}",
                "blr {f}",
                "mov sp, x20",
                top = in(reg) top,
                f = in(reg) body,
                out("x20") _,
                lateout("w0") code,
                clobber_abi("C"),
            );
        }
        Some(code)
    }
}

/// `binary_search`: the index std's search over the `n` elements of
/// `size` bytes at `buf` finds for the value at `key` under `cmp`, or -1.
///
/// # Safety
/// `buf` holds `n` elements; `cmp` is sound on an element and the key.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_bsearch(
    buf: *const u8,
    n: usize,
    size: usize,
    cmp: unsafe extern "C" fn(*const u8, *const u8) -> i32,
    key: *const u8,
) -> i64 {
    let at = |i: usize| {
        // SAFETY: the caller's contract.
        let o = unsafe { cmp(buf.add(i * size), key) };
        o.cmp(&0)
    };
    if n == 0 {
        // An empty Vec's buffer may be null.
        return -1;
    }
    if size == 0 {
        // Every element is the same zero-sized value.
        let idx = vec![(); n];
        return idx.binary_search_by(|_| at(0)).map_or(-1, |i| i as i64);
    }
    // std's search, driven over the buffer's first `n` bytes so each probe
    // names its index by its address, with nothing allocated per call
    // (B-2026-10-18-9): `n * size >= n` bytes are readable.
    // SAFETY: as above; the bytes are only addressed, never read.
    let bytes = unsafe { std::slice::from_raw_parts(buf, n) };
    bytes
        .binary_search_by(|b| at((b as *const u8 as usize) - (buf as usize)))
        .map_or(-1, |i| i as i64)
}

/// A float to an integer of `bits` bits, as `numeric_conv` converts it:
/// family 0 saturating, 1 wrapping, 2 checked, 3 trunc. Writes the value
/// (as its i64 bits) at `out` and returns 0, or 1 when the value is out of
/// range for a checked or trunc conversion, with nothing written.
///
/// # Safety
/// `out` is writable for an `i64`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_f2i(
    f: f64,
    family: i32,
    bits: i32,
    signed: u8,
    out: *mut i64,
) -> u8 {
    let signed = signed != 0;
    let saturating = |f: f64| -> i64 {
        match (bits, signed) {
            (8, true) => i64::from(f as i8),
            (16, true) => i64::from(f as i16),
            (32, true) => i64::from(f as i32),
            (8, false) => i64::from(f as u8),
            (16, false) => i64::from(f as u16),
            (32, false) => i64::from(f as u32),
            (64, false) => f as u64 as i64,
            _ => f as i64,
        }
    };
    let v = match family {
        0 => saturating(f),
        1 => {
            let t = f.trunc();
            if !t.is_finite() {
                0
            } else {
                let raw = t as i128;
                let modulus = 1i128 << bits;
                let mut m = raw & (modulus - 1);
                if signed && (m & (1i128 << (bits - 1))) != 0 {
                    m -= modulus;
                }
                m as i64
            }
        }
        _ => {
            if !f.is_finite() {
                return 1;
            }
            let t = f.trunc();
            let (lo, hi) = if signed {
                let half = 2f64.powi(bits - 1);
                (-half, half - 1.0)
            } else {
                (0.0, 2f64.powi(bits) - 1.0)
            };
            if !(t >= lo && t <= hi) {
                return 1;
            }
            saturating(f)
        }
    };
    // SAFETY: the caller's contract.
    unsafe { out.write(v) };
    0
}

thread_local! {
    /// The ids of the spans `with_span` made active, innermost last.
    static SPANS: RefCell<Vec<i64>> = const { RefCell::new(Vec::new()) };
}

/// A copy of the thread's active span stack, for a task being spawned.
pub(crate) fn current_spans() -> Vec<i64> {
    SPANS.with(|s| s.borrow().clone())
}

/// Installs `spans` as the thread's active span stack, returning the old one.
pub(crate) fn swap_spans(spans: Vec<i64>) -> Vec<i64> {
    SPANS.with(|s| std::mem::replace(&mut *s.borrow_mut(), spans))
}

/// Makes the span `id` active.
#[no_mangle]
pub extern "C" fn karac_mir_span_push(id: i64) {
    SPANS.with(|s| s.borrow_mut().push(id));
}

/// Ends the innermost active span.
#[no_mangle]
pub extern "C" fn karac_mir_span_pop() {
    SPANS.with(|s| s.borrow_mut().pop());
}

/// The innermost active span's id, or 0.
#[no_mangle]
pub extern "C" fn karac_mir_span_top() -> i64 {
    SPANS.with(|s| s.borrow().last().copied().unwrap_or(0))
}

thread_local! {
    static RAND: std::cell::Cell<u64> = std::cell::Cell::new(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64)
            | 1,
    );
}

/// `RandomSource.next_u64`: xorshift64 from a clock seed, as the
/// interpreter's.
#[no_mangle]
pub extern "C" fn karac_mir_rand_next() -> u64 {
    RAND.with(|r| {
        let mut x = r.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        r.set(x);
        x
    })
}

/// `Env.set(name, value)`.
///
/// # Safety
/// `p` and `v` are readable for `n` and `vn` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_env_set(p: *const u8, n: usize, v: *const u8, vn: usize) {
    // SAFETY: the caller's contract.
    let (name, val) = unsafe { (bytes(p, n), bytes(v, vn)) };
    if let (Ok(name), Ok(val)) = (std::str::from_utf8(name), std::str::from_utf8(val)) {
        // SAFETY: the program runs its environment edits on one thread,
        // as the interpreter does.
        unsafe { std::env::set_var(name, val) };
    }
}

/// `f64.total_cmp` (`f32` when `narrow`): -1, 0 or 1. Every NaN orders
/// as the positive quiet one, as the interpreter canonicalizes it.
#[no_mangle]
pub extern "C" fn karac_mir_total_cmp(a: f64, b: f64, narrow: u8) -> i32 {
    let canon = |f: f64| if f.is_nan() { f64::NAN } else { f };
    let o = if narrow != 0 {
        (canon(a) as f32).total_cmp(&(canon(b) as f32))
    } else {
        canon(a).total_cmp(&canon(b))
    };
    o as i32
}

/// The length of the NUL-terminated bytes at `p` (0 for null): a `CStr`
/// or `CString` is a pointer to them.
///
/// # Safety
/// `p` is null or points at NUL-terminated bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_cstr_len(p: *const u8) -> i64 {
    if p.is_null() {
        return 0;
    }
    // SAFETY: the caller's contract.
    unsafe { std::ffi::CStr::from_ptr(p.cast()) }
        .to_bytes()
        .len() as i64
}

/// `String.to_cstring`: writes a malloc'd NUL-terminated copy of the `n`
/// bytes at `p` to `*out` and returns 1, or returns 0 (writing nothing)
/// when they hold a NUL, which C would read as the end.
///
/// # Safety
/// `p` is readable for `n` bytes; `out` is writable for a pointer.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_to_cstring(out: *mut *mut u8, p: *const u8, n: usize) -> u8 {
    // SAFETY: the caller's contract.
    let b = unsafe { bytes(p, n) };
    if b.contains(&0) {
        return 0;
    }
    let buf = crate::alloc::karac_realloc_or_panic(std::ptr::null_mut(), n + 1);
    // SAFETY: `buf` holds `n + 1` bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(b.as_ptr(), buf, n);
        *buf.add(n) = 0;
        *out = buf;
    }
    1
}

/// Frees a `CString`'s buffer.
///
/// # Safety
/// `p` came from [`karac_mir_to_cstring`] and is not used after.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_cstring_free(p: *mut u8) {
    // SAFETY: the caller's contract; the buffer came from the C allocator.
    unsafe { free(p.cast()) };
}

/// `CStr.to_string` (`view` 0) or `to_string_slice` (1): validates the
/// NUL-terminated bytes at `p` as UTF-8; on success writes a `String`
/// copy (or, for a view, the bytes' `{ptr, len}`) at `out` and returns
/// 0, else returns 1 for a cut-off final sequence or 2 for any other bad
/// byte.
///
/// # Safety
/// `p` points at NUL-terminated bytes; `out` is writable for a `String`
/// (three words) or a view (two).
#[no_mangle]
pub unsafe extern "C" fn karac_mir_cstr_to_string(out: *mut RawVec, p: *const u8, view: u8) -> i32 {
    // SAFETY: the caller's contract.
    let b = unsafe { std::ffi::CStr::from_ptr(p.cast()) }.to_bytes();
    match std::str::from_utf8(b) {
        Ok(_) if view != 0 => {
            let w = out.cast::<usize>();
            // SAFETY: as above.
            unsafe {
                *w = p as usize;
                *w.add(1) = b.len();
            }
            0
        }
        Ok(_) => {
            // SAFETY: as above.
            unsafe { string_of(out, b) };
            0
        }
        Err(e) if e.error_len().is_none() => 1,
        Err(_) => 2,
    }
}

thread_local! {
    /// Each `Semaphore`'s `(available, initial)` permits, by id - 1.
    static SEMAPHORES: std::cell::RefCell<Vec<(i64, i64)>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Each `RateLimiter`'s rate, capacity and per-key `(tokens, last)`.
    static LIMITERS: std::cell::RefCell<Vec<Limiter>> = const { std::cell::RefCell::new(Vec::new()) };
}

struct Limiter {
    rate: f64,
    capacity: f64,
    buckets: std::collections::HashMap<Vec<u8>, (f64, std::time::Instant)>,
}

/// `Semaphore.new(n)`: its id. One task runs at a time, as in the
/// interpreter, so an `acquire` with no permit left times out at once.
#[no_mangle]
pub extern "C" fn karac_mir_sem_new(n: i64) -> i64 {
    let n = n.max(0);
    SEMAPHORES.with(|s| {
        let mut s = s.borrow_mut();
        s.push((n, n));
        s.len() as i64
    })
}

/// `Semaphore.acquire`: 1 when a permit was taken, 0 (a timeout) when
/// none is left or the id names no semaphore.
#[no_mangle]
pub extern "C" fn karac_mir_sem_acquire(id: i64) -> u8 {
    SEMAPHORES.with(|s| {
        let mut s = s.borrow_mut();
        match usize::try_from(id - 1).ok().and_then(|i| s.get_mut(i)) {
            Some((avail, _)) if *avail > 0 => {
                *avail -= 1;
                1
            }
            _ => 0,
        }
    })
}

/// `Semaphore.release`: a permit back, never above the initial count.
#[no_mangle]
pub extern "C" fn karac_mir_sem_release(id: i64) {
    SEMAPHORES.with(|s| {
        let mut s = s.borrow_mut();
        if let Some((avail, max)) = usize::try_from(id - 1).ok().and_then(|i| s.get_mut(i)) {
            if *avail < *max {
                *avail += 1;
            }
        }
    });
}

/// `RateLimiter.new_token_bucket(rate, capacity)`: its id.
#[no_mangle]
pub extern "C" fn karac_mir_limiter_new(rate: i64, cap: i64) -> i64 {
    LIMITERS.with(|l| {
        let mut l = l.borrow_mut();
        l.push(Limiter {
            rate: rate.max(0) as f64,
            capacity: cap.max(0) as f64,
            buckets: Default::default(),
        });
        l.len() as i64
    })
}

/// `RateLimiter.try_acquire(key)`: a fresh key's bucket starts full and
/// refills against the monotonic clock; 1 when a token was taken. An id
/// that names no limiter fails closed.
///
/// # Safety
/// `p` is readable for `n` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_limiter_try(id: i64, p: *const u8, n: usize) -> u8 {
    // SAFETY: the caller's contract.
    let key = unsafe { bytes(p, n) }.to_vec();
    LIMITERS.with(|l| {
        let mut l = l.borrow_mut();
        let Some(l) = usize::try_from(id - 1).ok().and_then(|i| l.get_mut(i)) else {
            return 0;
        };
        let now = std::time::Instant::now();
        let (rate, cap) = (l.rate, l.capacity);
        let (tokens, last) = l.buckets.entry(key).or_insert((cap, now));
        let elapsed = now.saturating_duration_since(*last).as_secs_f64();
        *tokens = (*tokens + elapsed * rate).min(cap);
        *last = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            1
        } else {
            0
        }
    })
}

/// `std.time::sleep_ms`: a real pause; a count of zero or less is a
/// no-op.
#[no_mangle]
pub extern "C" fn karac_mir_sleep_ms(ms: i64) {
    // Under the task executor, the readiness loop before it already waited
    // the pause out.
    if crate::mir_task::take_slept() {
        return;
    }
    if ms > 0 {
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::sleep(std::time::Duration::from_millis(ms as u64));
    }
}

/// `s.repeat(n)` (a negative count is 0) as a new `String` at `out`.
///
/// # Safety
/// `out` is writable for a `String`; `s` readable for `sn` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_repeat(out: *mut RawVec, s: *const u8, sn: usize, n: i64) {
    // SAFETY: the caller's contract.
    let t = unsafe { text(s, sn).repeat(n.max(0) as usize) };
    // SAFETY: as above.
    unsafe { string_of(out, t.as_bytes()) };
}

/// A new `Vec[String]` at `out` of the parts of `s`: 0 `split(sep)`, 1
/// `split_whitespace`, 2 `lines`.
///
/// # Safety
/// `out` is writable for a `Vec`; each pointer readable for its length.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_split(
    out: *mut RawVec,
    op: i32,
    s: *const u8,
    sn: usize,
    sep: *const u8,
    sepn: usize,
) {
    // SAFETY: the caller's contract.
    let (s, sep) = unsafe { (text(s, sn), text(sep, sepn)) };
    let parts: Vec<&str> = match op {
        0 => s.split(sep).collect(),
        1 => s.split_whitespace().collect(),
        _ => s.lines().collect(),
    };
    let size = std::mem::size_of::<RawVec>();
    // SAFETY: as above; the new buffer holds `parts.len()` Strings.
    unsafe {
        karac_mir_vec_alloc(out, parts.len(), size);
        let buf = (*out).ptr as *mut RawVec;
        for (i, p) in parts.iter().enumerate() {
            string_of(buf.add(i), p.as_bytes());
        }
    }
}

/// `v.join(sep)` over `n` texts laid out `stride` bytes apart from `elems`
/// (each starting with its `{ptr, len}`), as a new `String` at `out`.
///
/// # Safety
/// `out` is writable for a `String`; `elems` holds `n` texts; `sep` is
/// readable for `sepn` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_join(
    out: *mut RawVec,
    elems: *const u8,
    n: usize,
    stride: usize,
    sep: *const u8,
    sepn: usize,
) {
    let mut t = String::new();
    for i in 0..n {
        // SAFETY: the caller's contract.
        unsafe {
            if i > 0 {
                t.push_str(text(sep, sepn));
            }
            let e = elems.add(i * stride) as *const RawVec;
            t.push_str(text((*e).ptr, (*e).len));
        }
    }
    // SAFETY: as above.
    unsafe { string_of(out, t.as_bytes()) };
}

extern "C" {
    fn free(ptr: *mut core::ffi::c_void);
}

/// The `fs` functions, by `op`: `write(path, text)` (0),
/// `read_to_string(path)` (1), `read_lines(path)` (2), `exists(path)` (3),
/// `metadata(path)` (4), `read_dir(path)` (5), `create_dir` (6),
/// `create_dir_all` (7), `remove_file` (8), `remove_dir` (9),
/// `remove_dir_all` (10), `rename(path, text)` (11), `copy(path, text)`
/// (12), `read_bytes(path)` (13), `write_bytes(path, text)` (14),
/// `symlink(path, text)` (15, the target and the link), `read_link(path)`
/// (16), `symlink_metadata(path)` (17), `set_permissions(path, value[0])`
/// (18) and `set_modified(path, value[0])` (19), on the real file system
/// (docs/library/io.md § Files). Returns -1
/// on success, with the text read as a `String` (or the lines or entry
/// names as a `Vec[String]`) at `out`, `copy`'s byte count at `value[0]`,
/// or `metadata`'s size, is-file, is-dir, modification time (ms since
/// the Unix epoch), is-symlink and mode at `value[0..6]`; `exists` returns
/// 1 or 0. Otherwise the
/// code of the failure's `IoError` variant (`NotFound`, `PermissionDenied`,
/// `AlreadyExists`, `UnexpectedEof`, `InvalidUtf8`, `Interrupted`, `Other`,
/// `InvalidInput`, `Unsupported`: codegen's `IO_ERRORS`), with `Other`'s
/// message as a `String` at `out`. The interpreter's twin is src/mir/fs.rs.
///
/// # Safety
/// Each pointer is readable for its length; `out` is writable for a
/// `String` or `Vec`, and `value` for six `i64`s.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_fs(
    op: i32,
    path: *const u8,
    path_len: usize,
    text: *const u8,
    text_len: usize,
    out: *mut RawVec,
    value: *mut i64,
) -> i32 {
    use std::io::ErrorKind as K;
    // SAFETY: the caller's contract.
    let path = String::from_utf8_lossy(unsafe { bytes(path, path_len) }).into_owned();
    // SAFETY: as above.
    let text = unsafe { bytes(text, text_len) };
    let to = || String::from_utf8_lossy(text).into_owned();
    let lines_out = |lines: &[&str]| {
        // SAFETY: `out` is writable for a `Vec` of `String`s.
        unsafe {
            karac_mir_vec_alloc(out, lines.len(), std::mem::size_of::<RawVec>());
            let buf = (*out).ptr.cast::<RawVec>();
            for (i, l) in lines.iter().enumerate() {
                string_of(buf.add(i), l.as_bytes());
            }
        }
    };
    let res: std::io::Result<()> = match op {
        0 => std::fs::write(&path, text),
        1 | 2 => std::fs::read_to_string(&path).map(|t| {
            if op == 2 {
                lines_out(&t.lines().collect::<Vec<_>>());
            } else {
                // SAFETY: `out` is writable for a `String`.
                unsafe { string_of(out, t.as_bytes()) };
            }
        }),
        3 => return std::fs::metadata(&path).is_ok() as i32,
        4 | 17 => fs_metadata(&path, op == 4).map(|m| {
            for (i, w) in m.iter().enumerate() {
                // SAFETY: `value` is writable for six `i64`s.
                unsafe { *value.add(i) = *w };
            }
        }),
        5 => fs_read_dir(&path).map(|names| {
            lines_out(&names.iter().map(String::as_str).collect::<Vec<_>>());
        }),
        6 => std::fs::create_dir(&path),
        7 => std::fs::create_dir_all(&path),
        8 => std::fs::remove_file(&path),
        9 => std::fs::remove_dir(&path),
        10 => std::fs::remove_dir_all(&path),
        11 => std::fs::rename(&path, to()),
        // `read_bytes` / `write_bytes` (B-2026-10-15-28): the bytes as a
        // `Vec[u8]`, which has a `String`'s layout.
        // SAFETY: `out` is writable for a `Vec[u8]`.
        13 => std::fs::read(&path).map(|b| unsafe { string_of(out, &b) }),
        14 => std::fs::write(&path, text),
        // Links, modes and times (B-2026-10-15-59).
        15 => fs_symlink(&path, &to()),
        // SAFETY: `out` is writable for a `String`.
        16 => fs_read_link(&path).map(|t| unsafe { string_of(out, t.as_bytes()) }),
        // SAFETY: `value[0]` holds the mode or the time.
        18 => fs_set_permissions(&path, unsafe { *value }),
        // SAFETY: as above.
        19 => fs_set_modified(&path, unsafe { *value }),
        // SAFETY: `value` is writable for an `i64`.
        _ => std::fs::copy(&path, to()).map(|n| unsafe { *value = n as i64 }),
    };
    let e = match res {
        Ok(()) => return -1,
        Err(e) => e,
    };
    match e.kind() {
        K::NotFound => 0,
        K::PermissionDenied => 1,
        K::AlreadyExists => 2,
        K::UnexpectedEof => 3,
        K::InvalidData => 4,
        K::Interrupted => 5,
        K::InvalidInput => 7,
        K::Unsupported => 8,
        _ => {
            // SAFETY: `out` is writable for a `String`.
            unsafe { string_of(out, e.to_string().as_bytes()) };
            6
        }
    }
}

/// `fs.metadata` (`follow`) or `fs.symlink_metadata`: size, is-file,
/// is-dir, modification time in ms since the Unix epoch, is-symlink and
/// the mode's permission bits. The interpreter's twin is src/mir/fs.rs.
fn fs_metadata(path: &str, follow: bool) -> std::io::Result<[i64; 6]> {
    let m = if follow {
        std::fs::metadata(path)?
    } else {
        std::fs::symlink_metadata(path)?
    };
    let ms = match m.modified()?.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    };
    Ok([
        m.len() as i64,
        m.is_file() as i64,
        m.is_dir() as i64,
        ms,
        m.file_type().is_symlink() as i64,
        fs_mode(&m),
    ])
}

/// The permission bits (`0o7777`): the Unix mode, or one made from the
/// read-only flag elsewhere (`0o666` / `0o444`, `0o777` / `0o555` for a
/// directory).
fn fs_mode(m: &std::fs::Metadata) -> i64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        i64::from(m.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let all = if m.is_dir() { 0o777 } else { 0o666 };
        if m.permissions().readonly() {
            all & !0o222
        } else {
            all
        }
    }
}

/// `fs.set_permissions`: a mode outside `0..=0o7777` is `InvalidInput`;
/// without Unix modes only the owner's write bit counts.
fn fs_set_permissions(path: &str, mode: i64) -> std::io::Result<()> {
    if !(0..=0o7777).contains(&mode) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a mode is 0 to 0o7777",
        ));
    }
    #[cfg(unix)]
    let p = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::Permissions::from_mode(mode as u32)
    };
    #[cfg(not(unix))]
    let p = {
        let mut p = std::fs::metadata(path)?.permissions();
        p.set_readonly(mode & 0o200 == 0);
        p
    };
    std::fs::set_permissions(path, p)
}

/// `fs.set_modified`: the modification time to `ms` since the Unix epoch,
/// leaving the access time; opens the file to set it.
fn fs_set_modified(path: &str, ms: i64) -> std::io::Result<()> {
    let d = std::time::Duration::from_millis(ms.unsigned_abs());
    let t = if ms >= 0 {
        std::time::UNIX_EPOCH.checked_add(d)
    } else {
        std::time::UNIX_EPOCH.checked_sub(d)
    }
    .ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "the time is out of range")
    })?;
    #[cfg(windows)]
    let f = {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_WRITE_ATTRIBUTES, and FILE_FLAG_BACKUP_SEMANTICS so a
        // directory opens too.
        std::fs::OpenOptions::new()
            .access_mode(0x100)
            .custom_flags(0x0200_0000)
            .open(path)?
    };
    #[cfg(not(windows))]
    let f = std::fs::File::open(path)?;
    f.set_modified(t)
}

/// `fs.symlink(target, link)`; a directory link on Windows when the target
/// is a directory, `Unsupported` where there are no links.
fn fs_symlink(target: &str, link: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        let at = std::path::Path::new(link)
            .parent()
            .unwrap_or(std::path::Path::new(""))
            .join(target);
        if at.is_dir() {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "symbolic links are not supported here",
        ))
    }
}

/// `fs.read_link`: the stored target; one that is not UTF-8 is an
/// `InvalidData` error.
fn fs_read_link(path: &str) -> std::io::Result<String> {
    std::fs::read_link(path)?
        .into_os_string()
        .into_string()
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "a link target is not UTF-8",
            )
        })
}

/// `fs.read_dir`: the entry names sorted byte-wise; a name that is not
/// UTF-8 is an `InvalidData` error.
fn fs_read_dir(path: &str) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let name = entry?.file_name().into_string().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "a file name is not UTF-8")
        })?;
        names.push(name);
    }
    names.sort();
    Ok(names)
}

/// A `VecDeque`: a ring of `cap` slots from which `len` elements, starting
/// at slot `head`, are live (zero-sized elements keep `cap` and `head` 0).
#[repr(C)]
pub struct RawDeque {
    pub ptr: *mut u8,
    pub head: usize,
    pub len: usize,
    pub cap: usize,
}

impl RawDeque {
    fn slot(&self, i: usize, size: usize) -> *mut u8 {
        if size == 0 {
            return self.ptr;
        }
        // SAFETY: a slot index below `cap`, inside the buffer.
        unsafe { self.ptr.add((self.head + i) % self.cap * size) }
    }

    /// Makes room for one more element, unwrapping into a new buffer.
    fn grow(&mut self, size: usize) {
        if size == 0 || self.len < self.cap {
            return;
        }
        let cap = (self.cap * 2).max(4);
        let buf = karac_realloc_or_panic(std::ptr::null_mut(), cap * size);
        for i in 0..self.len {
            // SAFETY: element `i` is live; slot `i` of the new buffer is free.
            unsafe { std::ptr::copy_nonoverlapping(self.slot(i, size), buf.add(i * size), size) };
        }
        if self.cap != 0 {
            // SAFETY: the old buffer came from `karac_realloc_or_panic`.
            unsafe { free(self.ptr.cast()) };
        }
        self.ptr = buf;
        self.head = 0;
        self.cap = cap;
    }
}

/// Writes a deque of `n` uninitialized `size`-byte elements at `out`
/// (`head == 0`, `len == cap == n`), for the caller to fill.
///
/// # Safety
/// `out` is writable for a deque.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_deque_alloc(out: *mut RawDeque, n: usize, size: usize) {
    let mut v = RawVec {
        ptr: std::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: `v` is a writable vector.
    unsafe { karac_mir_vec_alloc(&mut v, n, size) };
    // SAFETY: the caller's contract.
    unsafe {
        out.write(RawDeque {
            ptr: v.ptr,
            head: 0,
            len: n,
            cap: v.cap,
        })
    };
}

/// The element `i` of the deque at `q` (in bounds).
///
/// # Safety
/// `q` is a live deque of `size`-byte elements with more than `i` of them.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_deque_slot(
    q: *const RawDeque,
    i: usize,
    size: usize,
) -> *mut u8 {
    // SAFETY: the caller's contract.
    unsafe { (*q).slot(i, size) }
}

/// Adds the `size` bytes at `elem` at the back, or the front when `front`.
///
/// # Safety
/// `q` is a live deque of `size`-byte elements; `elem` is readable.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_deque_push(
    q: *mut RawDeque,
    elem: *const u8,
    size: usize,
    front: u8,
) {
    // SAFETY: the caller's contract.
    let q = unsafe { &mut *q };
    q.grow(size);
    if front != 0 && size != 0 {
        q.head = (q.head + q.cap - 1) % q.cap;
    }
    let at = if front != 0 { 0 } else { q.len };
    q.len += 1;
    // SAFETY: `grow` made room; slot `at` is free.
    unsafe { std::ptr::copy_nonoverlapping(elem, q.slot(at, size), size) };
}

/// Inserts the `size` bytes at `elem` as element `i`, moving the ones
/// after it back one, and returns 1; returns 0, inserting nothing, when
/// `i` is past the end.
///
/// # Safety
/// `q` is a live deque of `size`-byte elements; `elem` is readable.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_deque_insert(
    q: *mut RawDeque,
    i: usize,
    elem: *const u8,
    size: usize,
) -> u8 {
    // SAFETY: the caller's contract.
    let q = unsafe { &mut *q };
    if i > q.len {
        return 0;
    }
    q.grow(size);
    for j in (i..q.len).rev() {
        // SAFETY: element `j` is live and slot `j + 1` is free or already
        // moved from; `grow` left room for one more.
        unsafe { std::ptr::copy_nonoverlapping(q.slot(j, size), q.slot(j + 1, size), size) };
    }
    q.len += 1;
    // SAFETY: slot `i` was vacated above.
    unsafe { std::ptr::copy_nonoverlapping(elem, q.slot(i, size), size) };
    1
}

/// Moves the back element, or the front one when `front`, to `out` and
/// returns 1, or returns 0 when the deque is empty.
///
/// # Safety
/// `q` is a live deque of `size`-byte elements; `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_deque_pop(
    q: *mut RawDeque,
    out: *mut u8,
    size: usize,
    front: u8,
) -> u8 {
    // SAFETY: the caller's contract.
    let q = unsafe { &mut *q };
    if q.len == 0 {
        return 0;
    }
    let at = if front != 0 { 0 } else { q.len - 1 };
    // SAFETY: element `at` is live.
    unsafe { std::ptr::copy_nonoverlapping(q.slot(at, size), out, size) };
    if front != 0 && size != 0 {
        q.head = (q.head + 1) % q.cap;
    }
    q.len -= 1;
    1
}

/// A stable sort of `n` elements of `size` bytes at `buf` by `cmp`, which
/// returns -1, 0 or 1 (the interpreter's bottom-up merge, so equal
/// elements keep their order).
///
/// # Safety
/// `buf` holds `n` elements; `cmp` is sound on any two of them.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_sort(
    buf: *mut u8,
    n: usize,
    size: usize,
    cmp: unsafe extern "C" fn(*const u8, *const u8) -> i32,
) {
    // SAFETY: the caller's contract.
    unsafe { merge_sort(buf, n, size, &mut |a, b| cmp(a, b)) };
}

/// Sorts `n` integers at `buf` in their numeric order: `kind` is the
/// width in bits (8, 16, 32 or 64), plus 256 when signed. Equal integers
/// cannot be told apart, so this agrees with the stable sort.
///
/// # Safety
/// `buf` holds `n` integers of that width, aligned to it.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_sort_int(buf: *mut u8, n: usize, kind: i64) {
    if n < 2 {
        return;
    }
    // SAFETY (each arm): the caller's contract.
    unsafe {
        match kind {
            8 => std::slice::from_raw_parts_mut(buf, n).sort_unstable(),
            16 => std::slice::from_raw_parts_mut(buf as *mut u16, n).sort_unstable(),
            32 => std::slice::from_raw_parts_mut(buf as *mut u32, n).sort_unstable(),
            64 => std::slice::from_raw_parts_mut(buf as *mut u64, n).sort_unstable(),
            264 => std::slice::from_raw_parts_mut(buf as *mut i8, n).sort_unstable(),
            272 => std::slice::from_raw_parts_mut(buf as *mut i16, n).sort_unstable(),
            288 => std::slice::from_raw_parts_mut(buf as *mut i32, n).sort_unstable(),
            320 => std::slice::from_raw_parts_mut(buf as *mut i64, n).sort_unstable(),
            _ => panic!("karac_mir_sort_int: no integer kind {kind}"),
        }
    }
}

/// [`karac_mir_sort`] with a comparator that also takes `ctx` (a
/// `sort_by` closure's environment).
///
/// # Safety
/// As [`karac_mir_sort`]; `cmp` is sound with `ctx`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_sort_ctx(
    buf: *mut u8,
    n: usize,
    size: usize,
    cmp: unsafe extern "C" fn(*const u8, *const u8, *const u8) -> i32,
    ctx: *const u8,
) {
    // SAFETY: the caller's contract.
    unsafe { merge_sort(buf, n, size, &mut |a, b| cmp(ctx, a, b)) };
}

/// # Safety
/// `buf` holds `n` elements of `size` bytes; `cmp` is sound on any two.
unsafe fn merge_sort(
    buf: *mut u8,
    n: usize,
    size: usize,
    cmp: &mut dyn FnMut(*const u8, *const u8) -> i32,
) {
    if n < 2 || size == 0 {
        return;
    }
    // A fixed element size lets each move be a plain load and store.
    // SAFETY (each arm): the caller's contract.
    unsafe {
        match size {
            1 => merge_passes::<1>(buf, n, size, cmp),
            2 => merge_passes::<2>(buf, n, size, cmp),
            4 => merge_passes::<4>(buf, n, size, cmp),
            8 => merge_passes::<8>(buf, n, size, cmp),
            16 => merge_passes::<16>(buf, n, size, cmp),
            24 => merge_passes::<24>(buf, n, size, cmp),
            32 => merge_passes::<32>(buf, n, size, cmp),
            _ => merge_passes::<0>(buf, n, size, cmp),
        }
    }
}

/// The block a merge sort finishes its narrow widths in before the wide
/// ones. The MIR interpreter's `merge_order` (src/mir/interp.rs
/// `MERGE_BLOCK`) and the sort codegen emits use the same value and
/// schedule, so a comparator sees the same calls in the same order on
/// every backend (B-2026-10-13-18).
const MERGE_BLOCK: usize = 1024;

/// The runs [`merge_widths`] insertion-sorts when it starts at width 1,
/// as the interpreter's `INSERTION_RUN` and the emitted sort do.
const INSERTION_RUN: usize = 16;

/// The bottom-up merge of [`merge_sort`], between `buf` and one scratch
/// buffer: widths 1, 2, 4, ..., the right element first only when
/// strictly less. Past [`MERGE_BLOCK`] elements, every width below it runs
/// one block at a time (in cache) before the wider passes over the whole
/// buffer; the merges are the ones full passes would make. `N` is the
/// element size, or 0 for "`size`, copied".
///
/// # Safety
/// As [`merge_sort`].
unsafe fn merge_passes<const N: usize>(
    buf: *mut u8,
    n: usize,
    size: usize,
    cmp: &mut dyn FnMut(*const u8, *const u8) -> i32,
) {
    let mut scratch = vec![0u8; n * size];
    let tmp = scratch.as_mut_ptr();
    if n <= MERGE_BLOCK {
        // SAFETY: the caller's contract; `tmp` holds `n` elements.
        unsafe { merge_widths::<N>(buf, tmp, n, size, 1, n, cmp) };
        return;
    }
    for b in (0..n).step_by(MERGE_BLOCK) {
        let len = MERGE_BLOCK.min(n - b);
        // SAFETY: block `b` of both buffers, `len` elements in bounds.
        unsafe {
            merge_widths::<N>(
                buf.add(b * size),
                tmp.add(b * size),
                len,
                size,
                1,
                MERGE_BLOCK,
                cmp,
            )
        };
    }
    // SAFETY: as above.
    unsafe { merge_widths::<N>(buf, tmp, n, size, MERGE_BLOCK, n, cmp) };
}

/// The passes of widths `width`, `2 * width`, ... below `limit` over the
/// `n` elements at `buf`, using `tmp` (room for `n`), ending in `buf`. A
/// start at width 1 insertion-sorts runs of [`INSERTION_RUN`] in place
/// instead of the narrowest passes (an element moves left past each it is
/// strictly less than).
///
/// # Safety
/// `buf` and `tmp` hold `n` elements of `size` bytes and do not overlap.
unsafe fn merge_widths<const N: usize>(
    buf: *mut u8,
    tmp: *mut u8,
    n: usize,
    size: usize,
    mut width: usize,
    limit: usize,
    cmp: &mut dyn FnMut(*const u8, *const u8) -> i32,
) {
    let (mut src, mut dst) = (buf, tmp);
    // SAFETY: every index passed is below `n`, in a buffer of `n`.
    let at = |b: *mut u8, i: usize| unsafe { b.add(i * size) };
    let put = |d: *mut u8, s: *const u8| {
        // SAFETY: both point at whole elements of distinct buffers.
        unsafe {
            if N == 0 {
                std::ptr::copy_nonoverlapping(s, d, size);
            } else {
                (d as *mut [u8; N]).write_unaligned((s as *const [u8; N]).read_unaligned());
            }
        }
    };
    if width == 1 {
        let mut x = vec![0u8; size];
        for lo in (0..n).step_by(INSERTION_RUN) {
            let hi = (lo + INSERTION_RUN).min(n);
            for k in lo + 1..hi {
                // SAFETY: indices below `n`; `x` holds one element.
                unsafe {
                    std::ptr::copy_nonoverlapping(at(buf, k), x.as_mut_ptr(), size);
                    let mut j = k;
                    while j > lo && cmp(x.as_ptr(), at(buf, j - 1)) < 0 {
                        std::ptr::copy_nonoverlapping(at(buf, j - 1), at(buf, j), size);
                        j -= 1;
                    }
                    std::ptr::copy_nonoverlapping(x.as_ptr(), at(buf, j), size);
                }
            }
        }
        width = INSERTION_RUN;
    }
    while width < limit {
        let mut k = 0;
        for lo in (0..n).step_by(2 * width) {
            let mid = (lo + width).min(n);
            let hi = (lo + 2 * width).min(n);
            let (mut i, mut j) = (lo, mid);
            while i < mid && j < hi {
                if cmp(at(src, j), at(src, i)) < 0 {
                    put(at(dst, k), at(src, j));
                    j += 1;
                } else {
                    put(at(dst, k), at(src, i));
                    i += 1;
                }
                k += 1;
            }
            // SAFETY: the rest of each run, whole elements in bounds.
            unsafe {
                std::ptr::copy_nonoverlapping(at(src, i), at(dst, k), (mid - i) * size);
                k += mid - i;
                std::ptr::copy_nonoverlapping(at(src, j), at(dst, k), (hi - j) * size);
                k += hi - j;
            }
        }
        std::mem::swap(&mut src, &mut dst);
        width *= 2;
    }
    if src != buf {
        // SAFETY: the sorted elements are in the scratch buffer.
        unsafe { std::ptr::copy_nonoverlapping(src, buf, n * size) };
    }
}

/// Ends a `panic_message` format: its text is the next abort's message.
#[no_mangle]
pub extern "C" fn karac_mir_panic_set() {
    let text = fmt_pop(|t| String::from_utf8_lossy(t).into_owned());
    PANIC.with(|p| *p.borrow_mut() = Some(text));
}

/// Ends the process with `code`, after writing what is buffered.
#[no_mangle]
pub extern "C" fn karac_mir_exit(code: i32) -> ! {
    out_flush();
    #[cfg(all(unix, feature = "net"))]
    if code == 101 && linked_with_asan() {
        // A panic runs no Drop (core semantics §10.2), so what it leaves
        // allocated is not a leak; under `__mir-llvm-asan` leave without
        // LeakSanitizer's exit check, which would report it (B-2026-10-20-38).
        let _ = std::io::stdout().flush();
        // SAFETY: ends the process; nothing runs after it.
        unsafe { libc::_exit(code) }
    }
    std::process::exit(code)
}

/// Whether AddressSanitizer is linked in, which `__mir-llvm-asan` does.
#[cfg(all(unix, feature = "net"))]
fn linked_with_asan() -> bool {
    // SAFETY: a lookup of a NUL-terminated name in the loaded objects.
    !unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"__asan_init".as_ptr()) }.is_null()
}

/// The program's `#[panic_handler]` thunk (`__karac_panic_thunk`), which
/// `main` registers before it runs: a unit function of no arguments, so it
/// takes only the slot for its result.
static PANIC_HANDLER: std::sync::atomic::AtomicPtr<u8> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Set once a panic has begun running the handler; a panic after that,
/// in the handler or in another task, ends the process at once.
static PANICKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The message and location of the panic the handler runs for.
static PANIC_INFO: std::sync::Mutex<(String, String)> =
    std::sync::Mutex::new((String::new(), String::new()));

/// Registers the program's panic-handler thunk (core semantics §10.2).
#[no_mangle]
pub extern "C" fn karac_mir_set_panic_handler(thunk: *mut u8) {
    PANIC_HANDLER.store(thunk, std::sync::atomic::Ordering::SeqCst);
}

/// Whether a panic may report itself: false once one has begun, whose
/// handler is running or has run. A panic for which this is false exits
/// with code 101 without a second report.
pub(crate) fn panic_begins() -> bool {
    !PANICKING.swap(true, std::sync::atomic::Ordering::SeqCst)
}

/// Runs the program's panic handler, if it has one, for the panic whose
/// record was just written: once, then the caller exits with code 101.
pub(crate) fn run_panic_handler(message: &str, location: &str) {
    let thunk = PANIC_HANDLER.load(std::sync::atomic::Ordering::SeqCst);
    if thunk.is_null() {
        return;
    }
    if let Ok(mut info) = PANIC_INFO.lock() {
        *info = (message.to_string(), location.to_string());
    }
    // SAFETY: `karac_mir_set_panic_handler` was given the thunk, a unit
    // function whose only parameter is the slot for its result.
    let f: extern "C" fn(*mut u8) = unsafe { std::mem::transmute(thunk) };
    let mut slot = [0u64; 2];
    f(slot.as_mut_ptr().cast());
}

/// `__karac_panic_message` (`which` 0) or `__karac_panic_location` (1):
/// that part of the panic being handled, as a `String` at `out`.
///
/// # Safety
/// `out` is writable for a `String`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_panic_info(out: *mut RawVec, which: i32) {
    let text = PANIC_INFO
        .lock()
        .map(|i| if which == 0 { i.0.clone() } else { i.1.clone() })
        .unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe { string_of(out, text.as_bytes()) };
}

/// A deadlock, which is a panic (core semantics §10.1): `error: deadlock:
/// {what}` on standard error, the panic handler, then exit 101.
pub(crate) fn deadlock(what: &str) -> ! {
    if panic_begins() {
        karac_mir_flush();
        eprintln!("error: deadlock: {what}");
        run_panic_handler(&format!("deadlock: {what}"), "");
    }
    karac_mir_exit(101)
}

/// A runtime error: the message on standard error, then exit 101.
///
/// # Safety
/// `ptr` is readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_abort(ptr: *const u8, len: usize) -> ! {
    if !panic_begins() {
        karac_mir_exit(101)
    }
    // SAFETY: the caller's contract.
    let msg = PANIC
        .with(|p| p.borrow_mut().take())
        .unwrap_or_else(|| String::from_utf8_lossy(unsafe { bytes(ptr, len) }).into_owned());
    write(true, format!("runtime error: {msg}\n").as_bytes());
    run_panic_handler(&msg, "");
    karac_mir_exit(101)
}

/// A runtime error at a place in the source: the message, then `at`
/// (`file:line:col`), as the MIR interpreter reports one; then exit 101.
/// An empty `at` reports the message alone.
///
/// # Safety
/// `ptr` is readable for `len` bytes and `at` for `at_len`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_abort_at(
    ptr: *const u8,
    len: usize,
    at: *const u8,
    at_len: usize,
) -> ! {
    if !panic_begins() {
        karac_mir_exit(101)
    }
    // SAFETY: the caller's contract.
    let msg = PANIC
        .with(|p| p.borrow_mut().take())
        .unwrap_or_else(|| String::from_utf8_lossy(unsafe { bytes(ptr, len) }).into_owned());
    // SAFETY: the caller's contract.
    let at = String::from_utf8_lossy(unsafe { bytes(at, at_len) });
    let report = if at.is_empty() {
        format!("runtime error: {msg}\n")
    } else {
        format!("runtime error: {msg}\n  at {at}\nError return trace:\n  {at}\n")
    };
    write(true, report.as_bytes());
    run_panic_handler(&msg, &at);
    karac_mir_exit(101)
}

/// A string in Rust's `Debug` form (`"a\n"`), for `dbg`.
///
/// # Safety
/// `ptr` is readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_fmt_str_debug(ptr: *const u8, len: usize) {
    // SAFETY: the caller's contract.
    let b = unsafe { bytes(ptr, len) };
    append(&format!("{:?}", String::from_utf8_lossy(b)));
}

/// A char in Rust's `Debug` form (`'a'`), for `dbg`.
#[no_mangle]
pub extern "C" fn karac_mir_fmt_char_debug(c: u32) {
    append(&format!("{:?}", char::from_u32(c).unwrap_or('\u{fffd}')));
}

/// The char method `op` (the index into the list below) of `c`, with
/// `radix` for `is_digit` and `to_digit`: a predicate is 0 or 1, a case
/// mapping is the mapped char (itself when the mapping expands), and
/// `to_digit` is the digit or -1. Bit 8 of `op` reads `c` as a byte (an
/// integer receiver), which is in no class when it is above 255.
#[no_mangle]
pub extern "C" fn karac_mir_char(op: u32, c: i64, radix: i64) -> i64 {
    let ch = if op & 0x100 != 0 {
        u8::try_from(c).ok().map(char::from)
    } else {
        u32::try_from(c).ok().and_then(char::from_u32)
    };
    let Some(ch) = ch else {
        return 0;
    };
    let b = i64::from;
    let one = |it: &mut dyn Iterator<Item = char>| {
        let first = it.next();
        match (first, it.next()) {
            (Some(m), None) => m,
            _ => ch,
        }
    };
    let r = u32::try_from(radix).unwrap_or(0);
    match CHAR_OPS.get((op & 0xff) as usize).copied().unwrap_or("") {
        "is_alphabetic" => b(ch.is_alphabetic()),
        "is_numeric" => b(ch.is_numeric()),
        "is_alphanumeric" => b(ch.is_alphanumeric()),
        "is_uppercase" => b(ch.is_uppercase()),
        "is_lowercase" => b(ch.is_lowercase()),
        "is_whitespace" => b(ch.is_whitespace()),
        "is_ascii" => b(ch.is_ascii()),
        "is_ascii_digit" => b(ch.is_ascii_digit()),
        "is_ascii_alphabetic" => b(ch.is_ascii_alphabetic()),
        "is_ascii_alphanumeric" => b(ch.is_ascii_alphanumeric()),
        "is_ascii_uppercase" => b(ch.is_ascii_uppercase()),
        "is_ascii_lowercase" => b(ch.is_ascii_lowercase()),
        "is_ascii_punctuation" => b(ch.is_ascii_punctuation()),
        "is_ascii_whitespace" => b(ch.is_ascii_whitespace()),
        "is_ascii_hexdigit" => b(ch.is_ascii_hexdigit()),
        "to_lowercase" => i64::from(u32::from(one(&mut ch.to_lowercase()))),
        "to_uppercase" => i64::from(u32::from(one(&mut ch.to_uppercase()))),
        "to_ascii_lowercase" => i64::from(u32::from(ch.to_ascii_lowercase())),
        "to_ascii_uppercase" => i64::from(u32::from(ch.to_ascii_uppercase())),
        "is_digit" => b(ch.is_digit(r)),
        "to_digit" => ch.to_digit(r).map_or(-1, i64::from),
        _ => 0,
    }
}

/// The char methods [`karac_mir_char`] answers, by index; codegen keeps
/// the same list (`src/codegen/mir/num.rs`).
const CHAR_OPS: &[&str] = &[
    "is_alphabetic",
    "is_numeric",
    "is_alphanumeric",
    "is_uppercase",
    "is_lowercase",
    "is_whitespace",
    "is_ascii",
    "is_ascii_digit",
    "is_ascii_alphabetic",
    "is_ascii_alphanumeric",
    "is_ascii_uppercase",
    "is_ascii_lowercase",
    "is_ascii_punctuation",
    "is_ascii_whitespace",
    "is_ascii_hexdigit",
    "to_lowercase",
    "to_uppercase",
    "to_ascii_lowercase",
    "to_ascii_uppercase",
    "is_digit",
    "to_digit",
];

/// `i128.parse(s)` / `u128.parse(s)` and their `from_str_radix`: 1 with
/// the number's 16 bytes at `out` when the trimmed text is one that fits,
/// else 0 (B-2026-10-18-11).
///
/// # Safety
/// `ptr` is readable for `len` bytes; `out` is writable for 16 bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_parse_int128(
    ptr: *const u8,
    len: usize,
    radix: i64,
    signed: u8,
    out: *mut u8,
) -> u8 {
    // SAFETY: the caller's contract.
    let Ok(text) = std::str::from_utf8(unsafe { bytes(ptr, len) }) else {
        return 0;
    };
    let text = text.trim();
    if !(2..=36).contains(&radix) {
        return 0;
    }
    let got = if signed != 0 {
        i128::from_str_radix(text, radix as u32).map(|n| n.to_ne_bytes())
    } else {
        u128::from_str_radix(text, radix as u32).map(|n| n.to_ne_bytes())
    };
    let Ok(b) = got else {
        return 0;
    };
    // SAFETY: the caller's contract.
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), out, 16) };
    1
}

/// A radix-10 integer of at most 18 digits, with an optional sign and
/// ASCII whitespace around it, read in place: it cannot overflow an `i64`,
/// and the text is all ASCII, so `str::trim` would have cut the same
/// bytes. `None` sends every other text to the `from_str_radix` path,
/// which gives the verdict on it (B-2026-10-13-34).
fn short_decimal(raw: &[u8], radix: i64) -> Option<(bool, i128)> {
    if radix != 10 {
        return None;
    }
    let mut b = raw;
    while let [first, rest @ ..] = b {
        if !first.is_ascii_whitespace() {
            break;
        }
        b = rest;
    }
    while let [rest @ .., last] = b {
        if !last.is_ascii_whitespace() {
            break;
        }
        b = rest;
    }
    let (neg, digits) = match b.first() {
        Some(b'-') => (true, &b[1..]),
        Some(b'+') => (false, &b[1..]),
        _ => (false, b),
    };
    if digits.is_empty() || digits.len() > 18 {
        return None;
    }
    let mut n: i64 = 0;
    for &d in digits {
        if !d.is_ascii_digit() {
            return None;
        }
        n = n * 10 + (d - b'0') as i64;
    }
    Some((neg, if neg { -(n as i128) } else { n as i128 }))
}

/// `T.parse(s)` / `T.from_str_radix(s, radix)` for an integer of `bits`
/// bits (at most 64): 1 with the number at `out` when the trimmed text is
/// one that fits, else 0.
///
/// # Safety
/// `ptr` is readable for `len` bytes; `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_parse_int(
    ptr: *const u8,
    len: usize,
    radix: i64,
    bits: u32,
    signed: u8,
    out: *mut i64,
) -> u8 {
    // SAFETY: the caller's contract.
    let raw = unsafe { bytes(ptr, len) };
    if !(2..=36).contains(&radix) {
        return 0;
    }
    let n = match short_decimal(raw, radix) {
        Some((true, _)) if signed == 0 => return 0,
        Some((_, n)) => n,
        None => {
            // Invalid UTF-8 cannot parse, so it fails here rather than
            // through a lossy, allocated copy (B-2026-10-13-34).
            let Ok(text) = std::str::from_utf8(raw) else {
                return 0;
            };
            let text = text.trim();
            if signed == 0 && text.starts_with('-') {
                return 0;
            }
            match i128::from_str_radix(text, radix as u32) {
                Ok(n) => n,
                Err(_) => return 0,
            }
        }
    };
    let fits = match (bits, signed != 0) {
        // The common widths, without the variable 128-bit shifts.
        (64, true) => i64::try_from(n).is_ok(),
        (64, false) => u64::try_from(n).is_ok(),
        (_, true) => {
            let half = 1i128 << (bits - 1);
            (-half..half).contains(&n)
        }
        (_, false) => n >= 0 && n < (1i128 << bits),
    };
    if !fits {
        return 0;
    }
    // SAFETY: the caller's contract.
    unsafe { *out = n as i64 };
    1
}

/// `f64.parse(s)`: 1 with the number at `out`, else 0.
///
/// # Safety
/// `ptr` is readable for `len` bytes; `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_parse_f64(ptr: *const u8, len: usize, out: *mut f64) -> u8 {
    // SAFETY: the caller's contract.
    let Ok(text) = std::str::from_utf8(unsafe { bytes(ptr, len) }) else {
        return 0;
    };
    match text.trim().parse::<f64>() {
        Ok(v) => {
            // SAFETY: the caller's contract.
            unsafe { *out = v };
            1
        }
        Err(_) => 0,
    }
}

/// `a.pow(e)` for an integer of `bits` bits (at most 64): 0 with the
/// power at `out`, 1 when it overflows the type, 2 for an exponent out of
/// `u32`. An unsigned `a` arrives as its bits.
///
/// # Safety
/// `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_ipow(
    a: i64,
    e: i64,
    bits: u32,
    signed: u8,
    out: *mut i64,
) -> u8 {
    let Ok(e) = u32::try_from(e) else {
        return 2;
    };
    let a = if signed != 0 {
        i128::from(a)
    } else {
        i128::from(a as u64)
    };
    let (lo, hi) = if signed != 0 {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    };
    match a.checked_pow(e) {
        Some(v) if (lo..=hi).contains(&v) => {
            // SAFETY: the caller's contract.
            unsafe { *out = v as i64 };
            0
        }
        _ => 1,
    }
}

/// An `Arena[T]`'s items: a vector of them and the arena's id, which its
/// `ArenaRef` handles carry.
#[repr(C)]
pub struct RawArena {
    pub items: RawVec,
    pub id: i64,
}

static ARENA_IDS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// `Arena.new()`: an empty arena with the next id (from 1, as the
/// interpreter numbers them).
#[no_mangle]
pub extern "C" fn karac_mir_arena_new() -> *mut RawArena {
    let id = ARENA_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    Box::into_raw(Box::new(RawArena {
        items: RawVec {
            ptr: std::ptr::null_mut(),
            len: 0,
            cap: 0,
        },
        id,
    }))
}

/// Frees an arena whose items compiled code has dropped.
///
/// # Safety
/// `a` came from [`karac_mir_arena_new`] and is not used again.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_arena_free(a: *mut RawArena) {
    // SAFETY: the caller's contract.
    let a = unsafe { Box::from_raw(a) };
    if a.items.cap != 0 && a.items.cap != usize::MAX {
        // SAFETY: the buffer came from `karac_realloc_or_panic`.
        unsafe { free(a.items.ptr.cast()) };
    }
}

/// An `Interner`'s strings, each a `String` that stays where it is so
/// `resolve` can lend it, and their ids. Its drop frees them all.
pub struct MirInterner {
    // Boxed so a lent `String` stays put when the vector grows.
    #[allow(clippy::vec_box)]
    strings: Vec<Box<RawVec>>,
    ids: std::collections::HashMap<Vec<u8>, usize>,
}

/// `Interner.new()`.
#[no_mangle]
pub extern "C" fn karac_mir_interner_new() -> *mut MirInterner {
    Box::into_raw(Box::new(MirInterner {
        strings: Vec::new(),
        ids: std::collections::HashMap::new(),
    }))
}

/// `Interner.intern(s)`: the id of `ptr[..len]`, new or not.
///
/// # Safety
/// `h` came from [`karac_mir_interner_new`]; `ptr` is readable for `len`
/// bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_interner_intern(
    h: *mut MirInterner,
    ptr: *const u8,
    len: usize,
) -> i64 {
    // SAFETY: the caller's contract.
    let (t, b) = unsafe { (&mut *h, bytes(ptr, len)) };
    if let Some(&id) = t.ids.get(b) {
        return id as i64;
    }
    let mut s = Box::new(RawVec {
        ptr: std::ptr::null_mut(),
        len: 0,
        cap: 0,
    });
    // SAFETY: `s` is a writable `String`.
    unsafe { string_of(&mut *s, b) };
    t.strings.push(s);
    t.ids.insert(b.to_vec(), t.strings.len() - 1);
    t.strings.len() as i64 - 1
}

/// The empty `String` a foreign symbol resolves to.
struct EmptyString(RawVec);
// SAFETY: it is never written; its null buffer is never read.
unsafe impl Sync for EmptyString {}
static EMPTY_STRING: EmptyString = EmptyString(RawVec {
    ptr: std::ptr::null_mut(),
    len: 0,
    cap: 0,
});

/// `Interner.resolve(sym)`: the interned `String`, or an empty one for a
/// symbol out of range.
///
/// # Safety
/// `h` came from [`karac_mir_interner_new`].
#[no_mangle]
pub unsafe extern "C" fn karac_mir_interner_resolve(
    h: *const MirInterner,
    id: i64,
) -> *const RawVec {
    // SAFETY: the caller's contract.
    let t = unsafe { &*h };
    match usize::try_from(id).ok().and_then(|i| t.strings.get(i)) {
        Some(s) => &**s,
        None => &EMPTY_STRING.0,
    }
}

/// Frees an interner and its strings; nothing it lent is still read.
///
/// # Safety
/// `h` came from [`karac_mir_interner_new`] and is not used again.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_interner_free(h: *mut MirInterner) {
    // SAFETY: the caller's contract; the box came from `Box::into_raw`.
    let t = unsafe { Box::from_raw(h) };
    for s in &t.strings {
        if s.cap != 0 {
            // SAFETY: the buffer came from `string_of`'s allocator.
            unsafe { free(s.ptr.cast()) };
        }
    }
}

/// `Interner.len()`.
///
/// # Safety
/// `h` came from [`karac_mir_interner_new`].
#[no_mangle]
pub unsafe extern "C" fn karac_mir_interner_len(h: *const MirInterner) -> i64 {
    // SAFETY: the caller's contract.
    unsafe { (*h).strings.len() as i64 }
}

/// `f16` and `bf16` are held as their bits and computed through `f32`;
/// these round an `f32` to the narrow type, as the interpreter's
/// `FloatTy::round` (`src/mir/ty.rs`), whose `f16` code this copies.
#[no_mangle]
pub extern "C" fn karac_mir_f16_from(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xFF) as i32;
    let frac = b & 0x007F_FFFF;
    if exp == 0xFF {
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
        return sign | 0x7C00;
    }
    if e >= -14 {
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
            ee += 1;
            mm = 0;
        }
        if ee >= 31 {
            return sign | 0x7C00;
        }
        return sign | ((ee as u16) << 10) | (mm as u16);
    }
    if e < -25 {
        return sign;
    }
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

/// The `f32` an `f16`'s bits stand for (exact).
#[no_mangle]
pub extern "C" fn karac_mir_f16_to(h: u16) -> f32 {
    let neg = h & 0x8000 != 0;
    let exp = ((h >> 10) & 0x1F) as i32;
    let mant = f64::from(h & 0x3FF);
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
    (if neg { -v } else { v }) as f32
}

/// An `f32` rounded to `bf16` bits: to nearest even, a NaN kept quiet.
#[no_mangle]
pub extern "C" fn karac_mir_bf16_from(f: f32) -> u16 {
    let bits = f.to_bits();
    if f.is_nan() {
        ((bits | 0x0040_0000) >> 16) as u16
    } else {
        let lsb = (bits >> 16) & 1;
        (bits.wrapping_add(0x7FFF + lsb) >> 16) as u16
    }
}

/// A channel of the MIR backend, as the interpreter's `Chan`: a queue of
/// `size`-byte values, with a capacity (0 is unbounded for a plain
/// channel, holds nothing for a `BoundedChannel`) and the count of live
/// ends of each kind. A `recv` never blocks: it finds a value or none,
/// as the interpreter's does. Queued values drop through `drop` once both
/// kinds of end are gone.
pub struct MirChan {
    queue: std::sync::Mutex<MirChanState>,
    size: usize,
    cap: usize,
    bounded: bool,
    drop: Option<unsafe extern "C" fn(*mut u8)>,
}

struct MirChanState {
    queue: std::collections::VecDeque<Box<[u8]>>,
    senders: usize,
    receivers: usize,
}

/// A new channel with one end of each kind.
#[no_mangle]
pub extern "C" fn karac_mir_chan_new(
    size: usize,
    cap: i64,
    bounded: u8,
    drop: Option<unsafe extern "C" fn(*mut u8)>,
) -> *mut MirChan {
    Box::into_raw(Box::new(MirChan {
        queue: std::sync::Mutex::new(MirChanState {
            queue: std::collections::VecDeque::new(),
            senders: 1,
            receivers: 1,
        }),
        size,
        cap: cap.max(0) as usize,
        bounded: bounded != 0,
        drop,
    }))
}

/// Queues the `size` bytes at `v`: 0 when queued, 1 when the channel is
/// full and 2 when no receiver is left (`v` is then left to the caller).
///
/// # Safety
/// `h` is a live channel; `v` is readable for its `size` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_chan_send(h: *mut MirChan, v: *const u8) -> i32 {
    // SAFETY: the caller's contract.
    let c = unsafe { &*h };
    let mut s = c.queue.lock().unwrap_or_else(|p| p.into_inner());
    if !c.bounded && s.receivers == 0 {
        return 2;
    }
    if (c.bounded || c.cap > 0) && s.queue.len() >= c.cap {
        return 1;
    }
    // SAFETY: as above.
    s.queue.push_back(unsafe { bytes(v, c.size) }.into());
    0
}

/// Whether a `recv` (`send` 0) or a `send` on `h` can complete now: a
/// value is queued or no sender is left, or there is room or no receiver
/// is left (a `BoundedChannel` refuses at once when full, so its send is
/// always ready).
///
/// # Safety
/// `h` is a live channel.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_chan_ready(h: *mut MirChan, send: u8) -> u8 {
    // SAFETY: the caller's contract.
    let c = unsafe { &*h };
    let s = c.queue.lock().unwrap_or_else(|p| p.into_inner());
    u8::from(if send == 0 {
        !s.queue.is_empty() || s.senders == 0
    } else {
        c.bounded || c.cap == 0 || s.queue.len() < c.cap || s.receivers == 0
    })
}

/// Moves the oldest value to `out`: 1 when there was one, else 0.
///
/// # Safety
/// `h` is a live channel; `out` is writable for its `size` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_chan_recv(h: *mut MirChan, out: *mut u8) -> u8 {
    // SAFETY: the caller's contract.
    let c = unsafe { &*h };
    let mut s = c.queue.lock().unwrap_or_else(|p| p.into_inner());
    match s.queue.pop_front() {
        Some(b) => {
            // SAFETY: as above.
            unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), out, c.size) };
            1
        }
        None => 0,
    }
}

/// Another end of the kind `sender` names.
///
/// # Safety
/// `h` is a live channel.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_chan_clone(h: *mut MirChan, sender: u8) -> *mut MirChan {
    // SAFETY: the caller's contract.
    let c = unsafe { &*h };
    let mut s = c.queue.lock().unwrap_or_else(|p| p.into_inner());
    if sender != 0 {
        s.senders += 1;
    } else {
        s.receivers += 1;
    }
    h
}

/// One end of the kind `sender` names is gone; with every end gone, the
/// queued values drop and the channel is freed.
///
/// # Safety
/// `h` is null or a live channel, and the end is not used again.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_chan_end(h: *mut MirChan, sender: u8) {
    if h.is_null() {
        return;
    }
    let last = {
        // SAFETY: the caller's contract.
        let c = unsafe { &*h };
        let mut s = c.queue.lock().unwrap_or_else(|p| p.into_inner());
        let n = if sender != 0 {
            &mut s.senders
        } else {
            &mut s.receivers
        };
        *n = n.saturating_sub(1);
        s.senders == 0 && s.receivers == 0
    };
    if !last {
        return;
    }
    // SAFETY: the last end; the channel came from `Box::into_raw`.
    let c = unsafe { Box::from_raw(h) };
    let s = c.queue.into_inner().unwrap_or_else(|p| p.into_inner());
    if let Some(d) = c.drop {
        for mut b in s.queue {
            // SAFETY: the bytes are a value the glue drops.
            unsafe { d(b.as_mut_ptr()) };
        }
    }
}

/// A dropped `Sender`.
///
/// # Safety
/// As [`karac_mir_chan_end`].
#[no_mangle]
pub unsafe extern "C" fn karac_mir_chan_drop_sender(h: *mut MirChan) {
    // SAFETY: the caller's contract.
    unsafe { karac_mir_chan_end(h, 1) }
}

/// A dropped `Receiver`.
///
/// # Safety
/// As [`karac_mir_chan_end`].
#[no_mangle]
pub unsafe extern "C" fn karac_mir_chan_drop_receiver(h: *mut MirChan) {
    // SAFETY: the caller's contract.
    unsafe { karac_mir_chan_end(h, 0) }
}

/// Writes the chars of `s[..n]` to `out` (room for `n` chars) and returns
/// how many there are.
///
/// # Safety
/// `s` is readable for `n` bytes of UTF-8; `out` is writable for `n`
/// `u32`s.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_str_decode(out: *mut u32, s: *const u8, n: usize) -> usize {
    // SAFETY: the caller's contract.
    let text = unsafe { std::str::from_utf8_unchecked(bytes(s, n)) };
    let mut k = 0;
    for c in text.chars() {
        // SAFETY: a string of `n` bytes has at most `n` chars.
        unsafe { out.add(k).write(u32::from(c)) };
        k += 1;
    }
    k
}

/// The chars `c[..k]` as a new `String` at `out`.
///
/// # Safety
/// `c` holds `k` valid chars; `out` is writable for a `String`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_string_from_chars(out: *mut RawVec, c: *const u32, k: usize) {
    let mut text = String::with_capacity(k);
    for i in 0..k {
        // SAFETY: the caller's contract.
        let v = unsafe { c.add(i).read() };
        text.push(char::from_u32(v).unwrap_or(char::REPLACEMENT_CHARACTER));
    }
    // SAFETY: the caller's contract.
    unsafe { string_of(out, text.as_bytes()) };
}

/// An erased function value of the MIR backend: `{code, env, drop}`.
type MirFnBytes = [usize; 3];

/// Drops an erased function value: its captures through their glue, then
/// their block (a null environment owns nothing).
///
/// # Safety
/// `f` is a live erased function value, not used again.
unsafe fn drop_mir_fn(f: &MirFnBytes) {
    let env = f[1] as *mut u8;
    if env.is_null() {
        return;
    }
    if f[2] != 0 {
        // SAFETY: `drop` is the glue of the captures at `env`.
        let d: unsafe extern "C" fn(*mut u8) = unsafe { std::mem::transmute(f[2]) };
        unsafe { d(env) };
    }
    // SAFETY: the captures' block came from `malloc`.
    unsafe { free(env.cast()) };
}

/// A `Pool[T]` of the MIR backend, as the interpreter's `PoolEntry`: the
/// factory and health check (erased function values), the idle values
/// (`size` bytes each), the count minted and not evicted, its bound and the
/// checked-out connection ids. Every `Pool` value and checked-out
/// connection holds a reference; the last one gone drops the idle values
/// and the functions and frees it.
pub struct MirPool {
    refs: usize,
    size: usize,
    drop: Option<unsafe extern "C" fn(*mut u8)>,
    create: MirFnBytes,
    check: Option<MirFnBytes>,
    slots: Vec<Box<[u8]>>,
    active: i64,
    max: i64,
    out: std::collections::HashSet<i64>,
    next_conn: i64,
}

/// A new pool of `size`-byte values minted by the erased function at
/// `create` (moved in), at most `max` at once.
///
/// # Safety
/// `create` is a live erased function value, which the pool now owns.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_new(
    size: usize,
    max: i64,
    create: *const MirFnBytes,
    drop: Option<unsafe extern "C" fn(*mut u8)>,
) -> *mut MirPool {
    Box::into_raw(Box::new(MirPool {
        refs: 1,
        size,
        drop,
        // SAFETY: the caller's contract.
        create: unsafe { create.read_unaligned() },
        check: None,
        slots: Vec::new(),
        active: 0,
        max,
        out: Default::default(),
        next_conn: 1,
    }))
}

/// Another `Pool` value for `h`; `check` (when not null, moved in)
/// replaces its health check.
///
/// # Safety
/// `h` is a live pool; `check` is null or a live erased function value.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_share(
    h: *mut MirPool,
    check: *const MirFnBytes,
) -> *mut MirPool {
    // SAFETY: the caller's contract.
    let p = unsafe { &mut *h };
    p.refs += 1;
    if !check.is_null() {
        // SAFETY: as above.
        if let Some(old) = p.check.replace(unsafe { check.read_unaligned() }) {
            unsafe { drop_mir_fn(&old) };
        }
    }
    h
}

/// Where the pool's factory (`which` 0) or health check (1, null when
/// none) is held.
///
/// # Safety
/// `h` is a live pool.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_fn(h: *mut MirPool, which: u8) -> *const MirFnBytes {
    // SAFETY: the caller's contract.
    let p = unsafe { &*h };
    match (which, &p.check) {
        (0, _) => &p.create,
        (_, Some(c)) => c,
        _ => std::ptr::null(),
    }
}

/// Moves the newest idle value to `out`: 1 when there was one, else 0.
///
/// # Safety
/// `h` is a live pool; `out` is writable for its value size.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_pop(h: *mut MirPool, out: *mut u8) -> u8 {
    // SAFETY: the caller's contract.
    let p = unsafe { &mut *h };
    match p.slots.pop() {
        Some(b) => {
            // SAFETY: as above.
            unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), out, p.size) };
            1
        }
        None => 0,
    }
}

/// Counts the pool's live values: `delta` -1 for an evicted one, +1 for
/// one minted; 0 answers whether another may be minted (1) or not (0).
///
/// # Safety
/// `h` is a live pool.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_count(h: *mut MirPool, delta: i64) -> u8 {
    // SAFETY: the caller's contract.
    let p = unsafe { &mut *h };
    if delta == 0 {
        return u8::from(p.active < p.max);
    }
    p.active = (p.active + delta).max(0);
    1
}

/// A new connection id, recorded as checked out; the connection holds a
/// reference on the pool.
///
/// # Safety
/// `h` is a live pool.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_check_out(h: *mut MirPool) -> i64 {
    // SAFETY: the caller's contract.
    let p = unsafe { &mut *h };
    let c = p.next_conn;
    p.next_conn += 1;
    p.out.insert(c);
    p.refs += 1;
    c
}

/// Hands back the value at `val` of connection `conn` from pool `h`, to
/// the pool `into` (null for `h`): 1 when the pool took it (it was
/// checked out there), else 0 and the caller drops it. The connection's
/// reference on `h` is gone either way.
///
/// # Safety
/// `h` is a live pool the connection holds a reference on; `val` is
/// readable for its value size.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_return(
    h: *mut MirPool,
    into: *mut MirPool,
    conn: i64,
    val: *const u8,
) -> u8 {
    let took = {
        // SAFETY: the caller's contract.
        let p = unsafe { &mut *h };
        if (into.is_null() || into == h) && p.out.remove(&conn) {
            // SAFETY: as above.
            p.slots.push(unsafe { bytes(val, p.size) }.into());
            1
        } else {
            0
        }
    };
    // SAFETY: the connection's reference.
    unsafe { karac_mir_pool_drop(h) };
    took
}

/// A `Pool` value (or a connection's reference) is gone; with the last
/// one, the idle values and the functions drop and the pool is freed.
///
/// # Safety
/// `h` is null or a live pool, and this reference is not used again.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pool_drop(h: *mut MirPool) {
    if h.is_null() {
        return;
    }
    {
        // SAFETY: the caller's contract.
        let p = unsafe { &mut *h };
        p.refs -= 1;
        if p.refs > 0 {
            return;
        }
    }
    // SAFETY: the last reference; the pool came from `Box::into_raw`.
    let p = unsafe { Box::from_raw(h) };
    if let Some(d) = p.drop {
        for mut b in p.slots {
            // SAFETY: the bytes are a value the glue drops.
            unsafe { d(b.as_mut_ptr()) };
        }
    }
    // SAFETY: the pool owned both functions.
    unsafe { drop_mir_fn(&p.create) };
    if let Some(c) = &p.check {
        unsafe { drop_mir_fn(c) };
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn padded_int_agrees_with_format_spec() {
        use crate::fmt::format_spec::FormatSpec;
        for spec in ["0", "1", "2", "02", "5", "05", "010", "20", "021"] {
            for v in [0i64, 7, -7, 42, -42, 12345, -12345, i64::MIN, i64::MAX] {
                let fast = super::padded_int(spec.as_bytes(), v as u64, true).unwrap();
                let slow = FormatSpec::parse(spec).unwrap().apply_int(v);
                assert_eq!(String::from_utf8(fast).unwrap(), slow, "{{{v}:{spec}}}");
            }
            for v in [0u64, 9, u64::MAX] {
                let fast = super::padded_int(spec.as_bytes(), v, false).unwrap();
                let slow = FormatSpec::parse(spec).unwrap().apply_uint(v);
                assert_eq!(String::from_utf8(fast).unwrap(), slow, "{{{v}:{spec}}}");
            }
        }
        for spec in ["", "x", "<5", "+5", "0005", ">05", ".2"] {
            assert!(
                super::padded_int(spec.as_bytes(), 1, true).is_none(),
                "{spec}"
            );
        }
    }

    use super::{
        karac_mir_fmt_begin, karac_mir_fmt_char, karac_mir_fmt_end_string, karac_mir_fmt_i64,
        karac_mir_fmt_str, karac_mir_fmt_u64, karac_mir_parse_int, merge_sort, RawVec,
    };

    /// `karac_mir_parse_int` on `text`, as an i64 of `bits` bits.
    fn parse(text: &[u8], radix: i64, bits: u32, signed: bool) -> Option<i64> {
        let mut out = 0i64;
        // SAFETY: `text` is readable and `out` writable.
        let ok = unsafe {
            karac_mir_parse_int(
                text.as_ptr(),
                text.len(),
                radix,
                bits,
                signed as u8,
                &mut out,
            )
        };
        (ok != 0).then_some(out)
    }

    // The in-place radix-10 path gives the verdict the i128 path gave
    // (B-2026-10-13-34): signs, trimming, junk, 18 and 19 digits, bounds.
    #[test]
    fn parse_int_short_decimal_agrees_with_from_str_radix() {
        let cases: &[&[u8]] = &[
            b"0",
            b"12",
            b"-12",
            b"+7",
            b" 12",
            b"12\n",
            b"",
            b"-",
            b"+",
            b"1_0",
            b"12a",
            b"--1",
            b"+-1",
            b"999999999999999999",
            b"-999999999999999999",
            b"9223372036854775807",
            b"-9223372036854775808",
            b"9223372036854775808",
            b"127",
            b"128",
            b"-128",
            b"-129",
            b"255",
            b"256",
            b"-0",
            b"\xff1",
            b"\xe0\x80",
            b" -0 ",
            b"\x0b12",
            b"12\x0c",
            b"\t+5\r\n",
            b"\xc2\xa012",
            b"1 2",
        ];
        for &t in cases {
            for (bits, signed) in [(64, true), (8, true), (8, false), (64, false)] {
                let want = std::str::from_utf8(t).ok().and_then(|s| {
                    let s = s.trim();
                    if !signed && s.starts_with('-') {
                        return None;
                    }
                    let n = s.parse::<i128>().ok()?;
                    let fits = if signed {
                        let half = 1i128 << (bits - 1);
                        (-half..half).contains(&n)
                    } else {
                        n >= 0 && n < (1i128 << bits)
                    };
                    fits.then_some(n as i64)
                });
                assert_eq!(parse(t, 10, bits, signed), want, "{:?} {bits} {signed}", t);
            }
        }
        assert_eq!(parse(b"ff", 16, 64, true), Some(255));
        assert_eq!(parse(b"12", 1, 64, true), None);
    }

    fn end_string() -> String {
        let mut out = RawVec {
            ptr: std::ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        // SAFETY: `out` is a writable `String`.
        unsafe { karac_mir_fmt_end_string(&mut out) };
        if out.ptr.is_null() {
            return String::new();
        }
        // SAFETY: the runtime wrote `len` bytes at `ptr`, from `malloc`.
        let s = unsafe { std::slice::from_raw_parts(out.ptr, out.len) }.to_vec();
        // SAFETY: as above.
        unsafe { free(out.ptr.cast()) };
        String::from_utf8(s).unwrap()
    }

    extern "C" {
        fn free(ptr: *mut core::ffi::c_void);
    }

    /// Formats nest on the one buffer: an inner format ends with its own
    /// text and leaves the outer one's in place; integers print at their
    /// extremes.
    #[test]
    fn formats_nest_and_print_integers() {
        let s = |t: &str| unsafe { karac_mir_fmt_str(t.as_ptr(), t.len()) };
        karac_mir_fmt_begin();
        s("a");
        karac_mir_fmt_i64(i64::MIN);
        karac_mir_fmt_begin();
        s("in");
        karac_mir_fmt_u64(u64::MAX);
        assert_eq!(end_string(), "in18446744073709551615");
        karac_mir_fmt_i64(0);
        karac_mir_fmt_i64(-7);
        karac_mir_fmt_char('é' as u32);
        assert_eq!(end_string(), "a-92233720368547758080-7é");
        karac_mir_fmt_begin();
        assert_eq!(end_string(), "");
    }

    /// The merge is stable and sorts, for a fixed element size and for
    /// one copied by length, against `slice::sort_by_key`.
    #[test]
    fn merge_sort_is_stable() {
        let mut rng = 99u64;
        for n in [0usize, 1, 2, 3, 7, 16, 100, 1000] {
            // (key, original position): 16 bytes, and 12 with the key
            // as an i32 pair padded to an odd size.
            let mut v16: Vec<(i64, i64)> = (0..n as i64)
                .map(|i| {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    ((rng % 10) as i64, i)
                })
                .collect();
            let mut want = v16.clone();
            want.sort_by_key(|e| e.0);
            let mut key = |a: *const u8, b: *const u8| {
                // SAFETY: both point at a `(i64, i64)` element.
                let (x, y) = unsafe {
                    (
                        (a as *const i64).read_unaligned(),
                        (b as *const i64).read_unaligned(),
                    )
                };
                x.cmp(&y) as i32
            };
            // SAFETY: `v16` holds `n` elements of 16 bytes.
            unsafe { merge_sort(v16.as_mut_ptr().cast(), n, 16, &mut key) };
            assert_eq!(v16, want);
            let mut v12: Vec<[u8; 12]> = want
                .iter()
                .rev()
                .map(|&(k, i)| {
                    let mut b = [0u8; 12];
                    b[..8].copy_from_slice(&k.to_le_bytes());
                    b[8..].copy_from_slice(&(i as u32).to_le_bytes());
                    b
                })
                .collect();
            let mut w12 = v12.clone();
            w12.sort_by_key(|b| i64::from_le_bytes(b[..8].try_into().unwrap()));
            // SAFETY: `v12` holds `n` elements of 12 bytes.
            unsafe { merge_sort(v12.as_mut_ptr().cast(), n, 12, &mut key) };
            assert_eq!(v12, w12);
        }
    }
}
