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
    /// The text being formatted, innermost last: a `println` or a
    /// `format` pushes a buffer, appends its parts, and pops it.
    static FMT: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// Standard output, flushed at exit, at an abort, and before anything
    /// is written to standard error so the two stay in order on a terminal.
    static OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// The message `panic(msg)` set for the abort that follows it.
    static PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

const OUT_LIMIT: usize = 1 << 16;

fn out_flush() {
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
            if o.len() >= OUT_LIMIT {
                drop(o);
                out_flush();
            }
        });
    }
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
    FMT.with(|f| match f.borrow_mut().last_mut() {
        Some(b) => b.push_str(s),
        None => write(false, s.as_bytes()),
    });
}

/// Writes what is buffered for standard output.
#[no_mangle]
pub extern "C" fn karac_mir_flush() {
    out_flush();
}

/// Starts formatting a `println` / `print` / `format`.
#[no_mangle]
pub extern "C" fn karac_mir_fmt_begin() {
    FMT.with(|f| f.borrow_mut().push(String::new()));
}

/// # Safety
/// `ptr` is readable for `len` bytes of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_fmt_str(ptr: *const u8, len: usize) {
    // SAFETY: the caller's contract; a `str` holds UTF-8.
    let b = unsafe { bytes(ptr, len) };
    append(&String::from_utf8_lossy(b));
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_i64(v: i64) {
    append(&v.to_string());
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_u64(v: u64) {
    append(&v.to_string());
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
    append(&v.to_string());
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_f32(v: f32) {
    append(&v.to_string());
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_bool(v: u8) {
    append(if v != 0 { "true" } else { "false" });
}

#[no_mangle]
pub extern "C" fn karac_mir_fmt_char(c: u32) {
    append(&char::from_u32(c).unwrap_or('\u{fffd}').to_string());
}

/// Ends a `println` (bit 0 of `flags`: add a newline; bit 1: to standard
/// error).
#[no_mangle]
pub extern "C" fn karac_mir_print_end(flags: u32) {
    let mut text = FMT.with(|f| f.borrow_mut().pop()).unwrap_or_default();
    if flags & 1 != 0 {
        text.push('\n');
    }
    write(flags & 2 != 0, text.as_bytes());
}

/// Ends a `format` into the `String` at `out`.
///
/// # Safety
/// `out` is writable for a `String`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_fmt_end_string(out: *mut RawVec) {
    let text = FMT.with(|f| f.borrow_mut().pop()).unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe { string_of(out, text.as_bytes()) };
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
    let raw = String::from_utf8_lossy(unsafe { bytes(spec, spec_len) }).into_owned();
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
                let shown = FMT.with(|b| b.borrow_mut().pop()).unwrap_or_default();
                fs.apply_str(&shown)
            }
        },
    };
    // SAFETY: the caller's contract.
    unsafe { string_of(out, text.as_bytes()) };
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
    let cap = want.max(v.cap.saturating_mul(2)).max(4);
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
    let t = match op {
        0 => s.trim().to_string(),
        1 => s.to_uppercase(),
        2 => s.to_lowercase(),
        3 => s.trim_start().to_string(),
        4 => s.trim_end().to_string(),
        _ => {
            let mut cs: Vec<char> = s.chars().collect();
            cs.sort_unstable();
            cs.into_iter().collect()
        }
    };
    // SAFETY: as above.
    unsafe { string_of(out, t.as_bytes()) };
}

/// The bytes `lo..hi` of `s` as a new `String` at `out`. Mode 0 is
/// `substring`, which saturates (an end past the text is its end, an empty
/// or inverted range is empty); mode 1 is `s[lo..hi]`, which panics when
/// out of range or inverted. Both panic off a char boundary. Returns 1 for
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
    let len = s.len() as i64;
    if mode == 2 {
        // `String.slice`: a start out of range is empty, the end clamps,
        // and a cut inside a char is lossy.
        let t = if lo < 0 || lo > len {
            String::new()
        } else {
            let end = hi.clamp(lo, len);
            String::from_utf8_lossy(&s.as_bytes()[lo as usize..end as usize]).into_owned()
        };
        // SAFETY: as above.
        unsafe { string_of(out, t.as_bytes()) };
        return 0;
    }
    let (lo, hi) = if mode == 0 {
        let hi = hi.min(len);
        if lo < 0 || lo >= hi {
            // SAFETY: as above.
            unsafe { string_of(out, b"") };
            return 0;
        }
        (lo, hi)
    } else {
        if !(0 <= lo && lo <= hi && hi <= len) {
            return 1;
        }
        (lo, hi)
    };
    let (lo, hi) = (lo as usize, hi as usize);
    if !s.is_char_boundary(lo) || !s.is_char_boundary(hi) {
        return 1;
    }
    // SAFETY: as above.
    unsafe { string_of(out, &s.as_bytes()[lo..hi]) };
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

/// Runs a program's `main` body on a thread with a 1 GiB stack (reserved,
/// committed as it is touched) and returns its exit code, so recursion
/// reaches the depth the interpreter's does.
#[no_mangle]
pub extern "C" fn karac_mir_run_main(body: extern "C" fn() -> i32) -> i32 {
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
    let idx: Vec<usize> = (0..n).collect();
    idx.binary_search_by(|&i| {
        // SAFETY: the caller's contract.
        let o = unsafe { cmp(buf.add(i * size), key) };
        o.cmp(&0)
    })
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

/// `std.time::sleep_ms`: a real pause; a count of zero or less is a
/// no-op.
#[no_mangle]
pub extern "C" fn karac_mir_sleep_ms(ms: i64) {
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

/// `FileSystem.write(path, text)` (`op` 0), `read_to_string(path)` (1) or
/// `read_lines(path)` (2) on the real file system. Returns -1 on success,
/// with the text read as a `String` (or its lines as a `Vec[String]`) at
/// `out`; otherwise the index of the failure's `IoError` variant (in
/// declaration order: `NotFound`, `PermissionDenied`, `AlreadyExists`,
/// `UnexpectedEof`, `InvalidUtf8`, `Interrupted`, `Other`), with `Other`'s
/// message as a `String` at `out`.
///
/// # Safety
/// Each pointer is readable for its length; `out` is writable for a
/// `String` or `Vec`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_fs(
    op: i32,
    path: *const u8,
    path_len: usize,
    text: *const u8,
    text_len: usize,
    out: *mut RawVec,
) -> i32 {
    use std::io::ErrorKind as K;
    // SAFETY: the caller's contract.
    let path = String::from_utf8_lossy(unsafe { bytes(path, path_len) }).into_owned();
    let res = match op {
        // SAFETY: as above.
        0 => std::fs::write(&path, unsafe { bytes(text, text_len) }).map(|()| None),
        _ => std::fs::read_to_string(&path).map(Some),
    };
    let e = match res {
        Ok(None) => return -1,
        Ok(Some(t)) if op == 2 => {
            let lines: Vec<&str> = t.lines().collect();
            // SAFETY: `out` is writable for a `Vec` of `String`s.
            unsafe {
                karac_mir_vec_alloc(out, lines.len(), std::mem::size_of::<RawVec>());
                let buf = (*out).ptr.cast::<RawVec>();
                for (i, l) in lines.iter().enumerate() {
                    string_of(buf.add(i), l.as_bytes());
                }
            }
            return -1;
        }
        Ok(Some(t)) => {
            // SAFETY: `out` is writable for a `String`.
            unsafe { string_of(out, t.as_bytes()) };
            return -1;
        }
        Err(e) => e,
    };
    match e.kind() {
        K::NotFound => 0,
        K::PermissionDenied => 1,
        K::AlreadyExists => 2,
        K::UnexpectedEof => 3,
        K::InvalidData => 4,
        K::Interrupted => 5,
        _ => {
            // SAFETY: `out` is writable for a `String`.
            unsafe { string_of(out, e.to_string().as_bytes()) };
            6
        }
    }
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
    // SAFETY: every index passed is below `n`.
    let at = |i: usize| unsafe { buf.add(i * size) };
    let mut order: Vec<usize> = (0..n).collect();
    let mut width = 1;
    while width < n {
        let mut out = Vec::with_capacity(n);
        for lo in (0..n).step_by(2 * width) {
            let mid = (lo + width).min(n);
            let hi = (lo + 2 * width).min(n);
            let (mut i, mut j) = (lo, mid);
            while i < mid && j < hi {
                // The right element goes first only when strictly less.
                if cmp(at(order[j]), at(order[i])) < 0 {
                    out.push(order[j]);
                    j += 1;
                } else {
                    out.push(order[i]);
                    i += 1;
                }
            }
            out.extend_from_slice(&order[i..mid]);
            out.extend_from_slice(&order[j..hi]);
        }
        order = out;
        width *= 2;
    }
    let mut copy = vec![0u8; n * size];
    for (k, &i) in order.iter().enumerate() {
        // SAFETY: `i < n`; `copy` holds `n` elements.
        unsafe { std::ptr::copy_nonoverlapping(at(i), copy.as_mut_ptr().add(k * size), size) };
    }
    // SAFETY: as above.
    unsafe { std::ptr::copy_nonoverlapping(copy.as_ptr(), buf, n * size) };
}

/// Ends a `panic_message` format: its text is the next abort's message.
#[no_mangle]
pub extern "C" fn karac_mir_panic_set() {
    let text = FMT.with(|f| f.borrow_mut().pop()).unwrap_or_default();
    PANIC.with(|p| *p.borrow_mut() = Some(text));
}

/// Ends the process with `code`, after writing what is buffered.
#[no_mangle]
pub extern "C" fn karac_mir_exit(code: i32) -> ! {
    out_flush();
    std::process::exit(code)
}

/// A runtime error: the message on standard error, then exit 101.
///
/// # Safety
/// `ptr` is readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_abort(ptr: *const u8, len: usize) -> ! {
    // SAFETY: the caller's contract.
    let msg = PANIC
        .with(|p| p.borrow_mut().take())
        .unwrap_or_else(|| String::from_utf8_lossy(unsafe { bytes(ptr, len) }).into_owned());
    write(true, format!("runtime error: {msg}\n").as_bytes());
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
    let text = String::from_utf8_lossy(unsafe { bytes(ptr, len) }).into_owned();
    let text = text.trim();
    if !(2..=36).contains(&radix) || (signed == 0 && text.starts_with('-')) {
        return 0;
    }
    let Ok(n) = i128::from_str_radix(text, radix as u32) else {
        return 0;
    };
    let fits = if signed != 0 {
        let half = 1i128 << (bits - 1);
        (-half..half).contains(&n)
    } else {
        n >= 0 && n < (1i128 << bits)
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
    let text = String::from_utf8_lossy(unsafe { bytes(ptr, len) }).into_owned();
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
/// `resolve` can lend it, and their ids. It lives until the process ends,
/// as the interpreter's do.
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
