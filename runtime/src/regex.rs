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
