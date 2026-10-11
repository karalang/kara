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
