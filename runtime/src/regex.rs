//! Regex FFI — the AOT/JIT backend for `runtime/stdlib/regex.kara`'s
//! `#[compiler_builtin]` `Regex.compile` / `Regex.is_match`. Gated behind the
//! opt-in `regex` feature, which is built into `libkarac_runtime_regex.a` and
//! auto-selected by `karac` whenever the emitted object references a
//! `karac_regex_*` symbol — mirroring the opt-in `gpu` archive so the heavy
//! `regex` dependency tree (regex-automata / regex-syntax / aho-corasick /
//! memchr) never touches the lean/full/wasm archives.
//!
//! The interpreter has its own in-process `regex`-crate path
//! (`src/interpreter/method_call_regex.rs`); these entrypoints are the
//! compiled equivalent and MATCH its semantics: a compiled `Regex` stores only
//! its pattern string and re-compiles per call (slice 1 — B-2026-07-14-19; a
//! compiled-handle cache is a later optimization). All non-UTF-8 / null inputs
//! degrade to "no match" / "invalid" rather than trapping.

use regex::Regex;

/// Decode a `(ptr, len)` Kāra-`String` view to `&str`, or `None` for a null
/// pointer or non-UTF-8 bytes. An empty Kāra `String` passes a valid pointer to
/// a NUL global with `len == 0`, so `len == 0` is legal (an empty pattern
/// compiles and matches everywhere).
///
/// # Safety
/// `ptr` must be null or point to `len` initialized, readable bytes.
unsafe fn view<'a>(ptr: *const u8, len: usize) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    std::str::from_utf8(std::slice::from_raw_parts(ptr, len)).ok()
}

/// `karac_regex_validate(pat_ptr, pat_len) -> u8` — `1` if `pat` compiles as a
/// regular expression, `0` otherwise. Backs `Regex.compile`'s Ok/Err decision
/// in codegen (Ok wraps the pattern; Err yields a `RegexError`).
///
/// # Safety
/// `pat_ptr` must be null or point to `pat_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_regex_validate(pat_ptr: *const u8, pat_len: usize) -> u8 {
    match view(pat_ptr, pat_len) {
        Some(pat) => Regex::new(pat).is_ok() as u8,
        None => 0,
    }
}

/// `karac_regex_is_match(pat_ptr, pat_len, s_ptr, s_len) -> u8` — recompiles
/// `pat` and returns `1` if it matches anywhere in `s`, else `0` (including a
/// pattern that no longer compiles or non-UTF-8 input). Re-compile-per-call
/// mirrors the interpreter's `try_eval_regex_method`.
///
/// # Safety
/// Both `(ptr, len)` pairs must each be null or point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_regex_is_match(
    pat_ptr: *const u8,
    pat_len: usize,
    s_ptr: *const u8,
    s_len: usize,
) -> u8 {
    let (pat, subject) = match (view(pat_ptr, pat_len), view(s_ptr, s_len)) {
        (Some(p), Some(s)) => (p, s),
        _ => return 0,
    };
    match Regex::new(pat) {
        Ok(re) => re.is_match(subject) as u8,
        Err(_) => 0,
    }
}

/// `karac_regex_find(pat, s, out_start, out_end) -> u8` — recompiles `pat` and,
/// for the leftmost match, writes its **byte** offsets `[start, end)` into `s`
/// and returns `1`; returns `0` (writing nothing) when there is no match, the
/// pattern no longer compiles, or either input is non-UTF-8 / null. The offsets
/// are byte indices — Kāra `String` is byte-indexed, so codegen slices
/// `s[start..end]` for the `Match.text` field. Backs `Regex.find`'s
/// `Option[Match]` return: `1` → `Some`, `0` → `None`.
///
/// # Safety
/// Both `(ptr, len)` pairs must each be null or point to `len` readable bytes;
/// `out_start` / `out_end` must be null or point to a writable `i64`.
#[no_mangle]
pub unsafe extern "C" fn karac_regex_find(
    pat_ptr: *const u8,
    pat_len: usize,
    s_ptr: *const u8,
    s_len: usize,
    out_start: *mut i64,
    out_end: *mut i64,
) -> u8 {
    let (pat, subject) = match (view(pat_ptr, pat_len), view(s_ptr, s_len)) {
        (Some(p), Some(s)) => (p, s),
        _ => return 0,
    };
    let re = match Regex::new(pat) {
        Ok(r) => r,
        Err(_) => return 0,
    };
    match re.find(subject) {
        Some(m) => {
            if !out_start.is_null() {
                *out_start = m.start() as i64;
            }
            if !out_end.is_null() {
                *out_end = m.end() as i64;
            }
            1
        }
        None => 0,
    }
}

/// `karac_regex_find_all(pat, s, out_count) -> *mut i64` — recompiles `pat` and
/// returns a heap array of `2 * count` `i64`s laid out as
/// `[start0, end0, start1, end1, …]` (byte offsets of every non-overlapping
/// match, left to right), writing `count` to `out_count`. The array is
/// allocated through `karac_alloc_or_panic` (libc `malloc`), so **the caller
/// (codegen) owns it and frees it with the matching `free`** after
/// materializing the `Vec[Match]`. Returns null with `count == 0` when there
/// are no matches, the pattern no longer compiles, or either input is
/// non-UTF-8 / null — codegen treats null as the empty `Vec`. Mirrors the
/// interpreter's `find_iter`.
///
/// # Safety
/// Both `(ptr, len)` pairs must each be null or point to `len` readable bytes;
/// `out_count` must be null or point to a writable `i64`.
#[no_mangle]
pub unsafe extern "C" fn karac_regex_find_all(
    pat_ptr: *const u8,
    pat_len: usize,
    s_ptr: *const u8,
    s_len: usize,
    out_count: *mut i64,
) -> *mut i64 {
    if !out_count.is_null() {
        *out_count = 0;
    }
    let (pat, subject) = match (view(pat_ptr, pat_len), view(s_ptr, s_len)) {
        (Some(p), Some(s)) => (p, s),
        _ => return core::ptr::null_mut(),
    };
    let re = match Regex::new(pat) {
        Ok(r) => r,
        Err(_) => return core::ptr::null_mut(),
    };
    let mut offs: Vec<i64> = Vec::new();
    for m in re.find_iter(subject) {
        offs.push(m.start() as i64);
        offs.push(m.end() as i64);
    }
    let count = offs.len() / 2;
    if count == 0 {
        return core::ptr::null_mut();
    }
    // Hand the offsets to codegen through a libc-`malloc`'d buffer so its
    // `free`-based cleanup matches the allocator; a Rust-`Vec`/`Box` pointer
    // freed by libc `free` would be UB (different allocator).
    let bytes = offs.len() * core::mem::size_of::<i64>();
    let buf = crate::alloc::karac_alloc_or_panic(bytes) as *mut i64;
    core::ptr::copy_nonoverlapping(offs.as_ptr(), buf, offs.len());
    if !out_count.is_null() {
        *out_count = count as i64;
    }
    buf
}

/// `karac_regex_replace_all(pat, s, repl, out_len) -> *mut u8` — recompiles
/// `pat`, replaces every non-overlapping match in `s` with `repl`, and returns
/// the result as a libc-`malloc`'d byte buffer (through `karac_alloc_or_panic`),
/// writing its byte length to `out_len`. **The caller (codegen) owns the buffer
/// and frees it** by wrapping it as an owned Kāra `String` (`cap > 0`). On an
/// invalid pattern or non-UTF-8 / null input the subject is returned unchanged
/// (a fresh copy), so the return is always a live owned buffer — never null.
/// Mirrors the interpreter's `replace_all`. The buffer is always at least one
/// byte (`max(len, 1)`) so an empty result is still a unique freeable pointer.
///
/// # Safety
/// All three `(ptr, len)` pairs must each be null or point to `len` readable
/// bytes; `out_len` must be null or point to a writable `i64`.
#[no_mangle]
pub unsafe extern "C" fn karac_regex_replace_all(
    pat_ptr: *const u8,
    pat_len: usize,
    s_ptr: *const u8,
    s_len: usize,
    repl_ptr: *const u8,
    repl_len: usize,
    out_len: *mut i64,
) -> *mut u8 {
    let subject = view(s_ptr, s_len).unwrap_or("");
    let replacement = view(repl_ptr, repl_len).unwrap_or("");
    // Compute the replaced bytes; fall back to the subject on an invalid /
    // non-UTF-8 pattern so the compiled `String` is always well-formed.
    let out: std::borrow::Cow<str> = match view(pat_ptr, pat_len).and_then(|p| Regex::new(p).ok()) {
        Some(re) => re.replace_all(subject, replacement),
        None => std::borrow::Cow::Borrowed(subject),
    };
    let src = out.as_bytes();
    let len = src.len();
    if !out_len.is_null() {
        *out_len = len as i64;
    }
    // `max(len, 1)` so an empty result is still a unique non-null freeable
    // pointer (codegen sets `cap = max(len, 1)` to match).
    let buf = crate::alloc::karac_alloc_or_panic(if len == 0 { 1 } else { len });
    core::ptr::copy_nonoverlapping(src.as_ptr(), buf, len);
    buf
}
