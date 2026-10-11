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
    if n < 2 || size == 0 {
        return;
    }
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
                // SAFETY: both indices are below `n`.
                if unsafe { cmp(at(order[j]), at(order[i])) } < 0 {
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
