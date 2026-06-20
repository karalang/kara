//! Per-type clone runtime helpers used by `emit_clone_fn_for_type_expr`.
//!
//! The codegen-emitted `karac_clone_<typename>` functions (one per type
//! mangled name, cached in the codegen `clone_fn_cache`) inline most of
//! their work: primitives are a load+store, Vec/Map/Set/Tuple recurse
//! through per-element clones synthesised in LLVM IR. The cases that
//! genuinely need a runtime helper are:
//!
//! * `String` — the codegen would otherwise have to duplicate the
//!   alloc-then-memcpy dance every emit site, including the static-
//!   literal `cap == 0` special case. One helper is cleaner.
//!
//! Future helpers (cycle-safe Rc clone, finalizer-aware refcounted clone)
//! land here too.

use std::alloc::{alloc, Layout};
use std::ffi::c_void;
use std::ptr;

/// Layout of a Kāra `String` value: `{ ptr data, i64 len, i64 cap }`.
/// Matches the codegen-side `string_struct_type` (Vec[u8] re-used for
/// String). Layout-equivalent on every supported target.
#[repr(C)]
struct KaracString {
    data: *mut u8,
    len: i64,
    cap: i64,
}

/// Deep-copy a Kāra `String`. Reads `*src` (`{data, len, cap}`), allocates
/// a fresh buffer holding `len` bytes, copies the source contents, and
/// writes `{new_data, len, new_cap}` to `*dst`.
///
/// Static-literal handling: when the source `cap == 0` (the convention for
/// strings whose buffer lives in the program's read-only string pool and
/// therefore must never be freed), the clone allocates a `len`-byte buffer
/// with `new_cap = len` so the cloned String's scope-exit cleanup correctly
/// frees it; the source's `cap = 0` keeps the static buffer untouched. For
/// already-heap-owned source strings (`cap > 0`), the clone's capacity
/// matches the source so a follow-up `push_str` in the cloned String has
/// the same headroom characteristic as a fresh copy.
///
/// Empty strings (`len == 0`) skip the allocation: the new String gets
/// `data = null`, `cap = 0`. The interpreter and codegen scope-exit free
/// paths already handle null-data Strings as no-ops.
///
/// # Safety
///
/// * `src` must point to a readable, fully-initialised `KaracString`.
/// * `dst` must point to a writable `KaracString`-sized region.
/// * The caller is responsible for the resulting String's lifetime —
///   typically registered with the codegen scope-cleanup machinery via
///   the same `track_vec_var` path Strings already use.
#[no_mangle]
pub unsafe extern "C" fn karac_string_clone(src: *const c_void, dst: *mut c_void) {
    let src = &*(src as *const KaracString);
    let dst = &mut *(dst as *mut KaracString);

    if src.len == 0 {
        dst.data = ptr::null_mut();
        dst.len = 0;
        dst.cap = 0;
        return;
    }

    // Allocate `len + 1` bytes and write a NUL at position `len` so the
    // cloned String stays printf-compatible. `Vec.push_str` codegen at
    // `src/codegen/assoc_call.rs:476` maintains the same invariant
    // (alloc len+1, copy len, set [len]=0); String-creating paths in
    // karac are expected to keep this contract because `println(str)` /
    // `printf("%s", data)` reads until NUL. Pre-fix the clone allocated
    // exactly `len` bytes, so a printf on the cloned String read one
    // byte past the allocation (ASAN heap-buffer-overflow, surfaced by
    // tests/memory_sanitizer.rs::asan_vec_extend_from_slice_string_*).
    // The `cap` field still mirrors `len` (no headroom) — only the
    // backing buffer is one byte larger.
    let alloc_bytes = (src.len as usize) + 1;
    let layout = Layout::array::<u8>(alloc_bytes).unwrap();
    let new_data = alloc(layout);
    ptr::copy_nonoverlapping(src.data, new_data, src.len as usize);
    *new_data.add(src.len as usize) = 0;

    dst.data = new_data;
    dst.len = src.len;
    dst.cap = src.len; // capacity matches len — fresh buffer, no headroom.
}

/// Slice a Kāra `String`: `s[start..end]` → a fresh heap `String` buffer
/// holding the bytes `data[start..end]`. Returns the new buffer pointer
/// (NUL-terminated, `end - start` content bytes); the codegen caller builds
/// the `{ptr, len, cap}` aggregate with `len = cap = end - start`. The
/// empty-slice case (`start == end`) returns null, matching
/// `karac_string_clone`'s empty-String convention (`data = null`,
/// `cap = 0`), so the scope-exit free path treats it as a no-op.
///
/// Validation mirrors the interpreter (`src/interpreter/eval_expr.rs`
/// range-index `Value::String` arm) and Rust's `&s[a..b]`:
///
/// * Bounds: `0 <= start <= end <= len`, else a fatal `string slice bounds
///   … out of range` runtime error.
/// * UTF-8 char boundaries: both `start` and `end` must fall on a char
///   boundary (a byte index `i` is a boundary iff `i == 0`, `i == len`, or
///   `data[i]` is not a `0b10xxxxxx` continuation byte), else a fatal
///   `E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY` runtime error.
///
/// Both fatal paths print to stderr and `exit(1)`, matching codegen's
/// `emit_panic` shape (a non-boundary slice is a panic, not a recoverable
/// error — same as Rust).
///
/// # Safety
///
/// * `data` must point to a readable buffer of at least `len` bytes when
///   `len > 0`.
/// * The returned pointer (when non-null) owns a heap allocation the caller
///   must register with the String scope-cleanup machinery (same `cap == len`
///   contract as `karac_string_clone`).
#[no_mangle]
pub unsafe extern "C" fn karac_string_slice(
    data: *const u8,
    len: i64,
    start: i64,
    end: i64,
) -> *mut u8 {
    if start < 0 || end < start || end > len {
        // Lean fatal print (raw write(2), no std-IO) — see `fatal` /
        // B-2026-06-11-8; this symbol is on every String-slice program's path.
        crate::fatal::eprint_fmt(format_args!(
            "runtime error: string slice bounds {}..{} out of range (len {})\n",
            start, end, len
        ));
        std::process::exit(1);
    }
    let len_us = len as usize;
    let start_us = start as usize;
    let end_us = end as usize;
    let bytes: &[u8] = if len_us == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(data, len_us)
    };
    // A byte index `i` is a UTF-8 char boundary iff it's the start/end of
    // the buffer or `bytes[i]` is not a `0b10xxxxxx` continuation byte. The
    // `i == len_us` short-circuit keeps `bytes[i]` from indexing past the end.
    let is_boundary = |i: usize| i == 0 || i == len_us || (bytes[i] & 0xC0) != 0x80;
    if !is_boundary(start_us) || !is_boundary(end_us) {
        crate::fatal::eprint_fmt(format_args!(
            "runtime error: E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY: byte range \
             {}..{} does not fall on UTF-8 char boundaries\n",
            start, end
        ));
        std::process::exit(1);
    }
    let n = end_us - start_us;
    if n == 0 {
        return ptr::null_mut();
    }
    // Alloc `n + 1` and NUL-terminate so the result stays printf-compatible,
    // matching `karac_string_clone`'s buffer contract (`cap == n`, buffer is
    // `n + 1` bytes).
    let layout = Layout::array::<u8>(n + 1).unwrap();
    let new_data = alloc(layout);
    ptr::copy_nonoverlapping(data.add(start_us), new_data, n);
    *new_data.add(n) = 0;
    new_data
}

/// Allocate a fresh NUL-terminated heap buffer holding `bytes`, write its
/// length to `*out_len`, and return the buffer pointer. Empty input returns
/// `null` + `*out_len == 0` (the `karac_string_slice` empty convention; codegen
/// builds a `{null, 0, 0}` String). The buffer contract matches
/// `karac_string_slice`: `cap == len`, the allocation is `len + 1` bytes.
///
/// # Safety
/// `out_len` must point to a writable `i64`.
unsafe fn alloc_string_result(bytes: &[u8], out_len: *mut i64) -> *mut u8 {
    let n = bytes.len();
    *out_len = n as i64;
    if n == 0 {
        return ptr::null_mut();
    }
    let layout = Layout::array::<u8>(n + 1).unwrap();
    let new_data = alloc(layout);
    ptr::copy_nonoverlapping(bytes.as_ptr(), new_data, n);
    *new_data.add(n) = 0;
    new_data
}

/// Borrow `(data, len)` as a `&str`. The Kāra String invariant guarantees valid
/// UTF-8, so this never fails in practice; on the impossible invalid-UTF-8 path
/// it fatally exits rather than returning silently-wrong bytes.
///
/// # Safety
/// `data` must point to a readable buffer of at least `len` bytes when `len > 0`.
unsafe fn str_from_raw<'a>(data: *const u8, len: i64) -> &'a str {
    let bytes: &[u8] = if len <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(data, len as usize)
    };
    match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => {
            crate::fatal::eprint_fmt(format_args!(
                "runtime error: internal: String buffer was not valid UTF-8\n"
            ));
            std::process::exit(1);
        }
    }
}

/// `String.to_lowercase()` — full Unicode lowercase (Rust `str::to_lowercase`),
/// matching the interpreter exactly. Returns a fresh owned buffer (the mapping
/// can change the byte length, e.g. `İ` → `i̇`).
///
/// # Safety
/// `data`/`len` are a Kāra String body; `out_len` must be writable. See
/// [`alloc_string_result`].
#[no_mangle]
pub unsafe extern "C" fn karac_string_to_lowercase(
    data: *const u8,
    len: i64,
    out_len: *mut i64,
) -> *mut u8 {
    let lowered = str_from_raw(data, len).to_lowercase();
    alloc_string_result(lowered.as_bytes(), out_len)
}

/// `String.to_uppercase()` — full Unicode uppercase (Rust `str::to_uppercase`;
/// e.g. `ß` → `SS`). Mirror of [`karac_string_to_lowercase`].
///
/// # Safety
/// See [`karac_string_to_lowercase`].
#[no_mangle]
pub unsafe extern "C" fn karac_string_to_uppercase(
    data: *const u8,
    len: i64,
    out_len: *mut i64,
) -> *mut u8 {
    let upped = str_from_raw(data, len).to_uppercase();
    alloc_string_result(upped.as_bytes(), out_len)
}

/// `String.trim()` — strip leading and trailing Unicode whitespace (Rust
/// `str::trim`), returning a fresh OWNED copy of the trimmed range (Kāra's trim
/// allocates rather than borrowing a view).
///
/// # Safety
/// See [`karac_string_to_lowercase`].
#[no_mangle]
pub unsafe extern "C" fn karac_string_trim(
    data: *const u8,
    len: i64,
    out_len: *mut i64,
) -> *mut u8 {
    let trimmed = str_from_raw(data, len).trim();
    alloc_string_result(trimmed.as_bytes(), out_len)
}

/// `String.replace(from, to)` — replace every non-overlapping occurrence of
/// `from` with `to` (Rust `str::replace`). Returns a fresh owned buffer.
///
/// # Safety
/// `data`/`from`/`to` are Kāra String bodies (their `*_len` byte counts);
/// `out_len` must be writable. See [`alloc_string_result`].
#[no_mangle]
pub unsafe extern "C" fn karac_string_replace(
    data: *const u8,
    len: i64,
    from: *const u8,
    from_len: i64,
    to: *const u8,
    to_len: i64,
    out_len: *mut i64,
) -> *mut u8 {
    let haystack = str_from_raw(data, len);
    let from_s = str_from_raw(from, from_len);
    let to_s = str_from_raw(to, to_len);
    let replaced = haystack.replace(from_s, to_s);
    alloc_string_result(replaced.as_bytes(), out_len)
}

/// Borrowed (non-allocating) sibling of `karac_string_slice`: validates the
/// `start..end` range against `(data, len)` with the *identical* bounds and
/// UTF-8 char-boundary checks (same fatal `exit(1)` messages), then returns a
/// pointer **into the source buffer** (`data + start`) without copying.
///
/// Codegen builds a borrowed `String` view `{ptr: <this>, len: end - start,
/// cap: 0}` from the result. `cap == 0` is the existing static/borrowed marker
/// the scope-exit and `Map`/`Vec`-free `cap > 0` guards already skip, so the
/// view is never freed by the caller. The view is only ever handed to map
/// lookup methods (`get`/`contains_key`/`remove`/`get_or`), which hash and
/// compare the `{ptr, len}` bytes and never retain the key, and to
/// `karac_map_insert_borrowed_str_old`, which deep-copies the bytes on a fresh
/// insertion — so the borrowed pointer never outlives the source string.
///
/// Returns null for an empty slice (`start == end`), matching
/// `karac_string_slice`; the `len == 0` view's pointer is never dereferenced.
///
/// # Safety
/// Same contract as `karac_string_slice`: `data` must point to a readable
/// buffer of at least `len` bytes when `len > 0`.
#[no_mangle]
pub unsafe extern "C" fn karac_string_slice_borrow(
    data: *const u8,
    len: i64,
    start: i64,
    end: i64,
) -> *const u8 {
    if start < 0 || end < start || end > len {
        // Lean fatal print (raw write(2), no std-IO) — see `fatal` /
        // B-2026-06-11-8; this symbol is on every String-slice program's path.
        crate::fatal::eprint_fmt(format_args!(
            "runtime error: string slice bounds {}..{} out of range (len {})\n",
            start, end, len
        ));
        std::process::exit(1);
    }
    let len_us = len as usize;
    let start_us = start as usize;
    let end_us = end as usize;
    let bytes: &[u8] = if len_us == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(data, len_us)
    };
    let is_boundary = |i: usize| i == 0 || i == len_us || (bytes[i] & 0xC0) != 0x80;
    if !is_boundary(start_us) || !is_boundary(end_us) {
        crate::fatal::eprint_fmt(format_args!(
            "runtime error: E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY: byte range \
             {}..{} does not fall on UTF-8 char boundaries\n",
            start, end
        ));
        std::process::exit(1);
    }
    if end_us == start_us {
        return ptr::null();
    }
    data.add(start_us)
}

/// Decode the next UTF-8 character starting at `byte_offset` in the byte
/// slice `(data, len)`. Writes the Unicode scalar value (codepoint) through
/// `out_codepoint` and returns the byte offset after the decoded character.
///
/// Used by codegen for `for c in s` / `for c in s.chars()` on a Kāra
/// `String`. The interpreter side uses Rust's `str::chars` directly; this
/// extern is the codegen-side equivalent so compiled-mode and tree-walk
/// produce identical per-char sequences (same Unicode scalar values).
///
/// Malformed UTF-8 produces the standard replacement character `U+FFFD`
/// for the offending byte and advances by one byte — matches Rust's
/// `String::from_utf8_lossy` recovery semantics. v1 expects well-formed
/// UTF-8 from sources upstream of codegen; the recovery path exists to
/// keep the loop forward-progressing on garbage rather than infinite-
/// looping.
///
/// # Safety
///
/// * `data` must point to a readable buffer of at least `len` bytes when
///   `byte_offset < len`; the helper performs no out-of-bounds read past
///   `len`. Callers (the codegen-emitted for-loop) gate this call on
///   `byte_offset < len`.
/// * `out_codepoint` must point to a writable `u32`.
#[no_mangle]
pub unsafe extern "C" fn karac_string_decode_char(
    data: *const u8,
    len: i64,
    byte_offset: i64,
    out_codepoint: *mut u32,
) -> i64 {
    // O(1)-per-call single-char UTF-8 decoder. Prior versions
    // delegated to `std::str::from_utf8(slice)` over the *whole*
    // remaining slice, then `chars().next()`. That made each call
    // O(remaining_bytes) — for a 104K-char `for c in s.chars()`
    // pass, total validation work grew quadratically (~5.4B bytes
    // re-validated). Investigation (`wip-chars-inline.md`, 2026-05-15)
    // measured this as the dominant per-char cost in karac vs Rust
    // (karac 776 ns/char vs Rust 0.96 ns/char → 810× slower on
    // pure-iter bench). The fix is mechanical: look at one to four
    // bytes for the next character; never touch the rest of the
    // slice.
    //
    // Output parity with the prior implementation on well-formed
    // input is exact (same Unicode scalar value, same byte
    // advancement). For malformed input the new version emits
    // U+FFFD and advances 1 byte at the malformed position; the
    // prior version tried a small valid-prefix recovery before
    // emitting FFFD. Both shapes are forward-progressing and match
    // `String::from_utf8_lossy`'s recovery family; the simpler
    // single-byte advance is the standard "WHATWG UTF-8 decoder"
    // recovery rule.
    if byte_offset < 0 || byte_offset >= len {
        *out_codepoint = 0;
        return len;
    }
    let start = byte_offset as usize;
    let total = len as usize;
    let remaining = total - start;
    let b0 = *data.add(start);

    // ── ASCII fast path (the hot path for English / source code) ─
    if b0 < 0x80 {
        *out_codepoint = b0 as u32;
        return (start + 1) as i64;
    }

    // ── Determine continuation width from lead byte ──────────────
    let width: usize = if b0 < 0xC2 {
        // 0x80..0xC0: stray continuation byte at start (malformed).
        // 0xC0..0xC2: 2-byte overlong of a 1-byte ASCII codepoint —
        // disallowed by the UTF-8 spec since RFC 3629. Reject both.
        *out_codepoint = 0xFFFD;
        return (start + 1) as i64;
    } else if b0 < 0xE0 {
        2
    } else if b0 < 0xF0 {
        3
    } else if b0 < 0xF5 {
        // 0xF5..0xF8 would technically be 4-byte leads but they
        // start above the U+10FFFF Unicode cap — disallowed.
        4
    } else {
        *out_codepoint = 0xFFFD;
        return (start + 1) as i64;
    };

    if remaining < width {
        // Truncated sequence at end of string.
        *out_codepoint = 0xFFFD;
        return (start + 1) as i64;
    }

    // ── Combine continuation bytes, validating each ──────────────
    let mut cp: u32 = match width {
        2 => (b0 & 0x1F) as u32,
        3 => (b0 & 0x0F) as u32,
        4 => (b0 & 0x07) as u32,
        _ => unreachable!(),
    };
    for i in 1..width {
        let b = *data.add(start + i);
        if b & 0xC0 != 0x80 {
            // Expected a `10xxxxxx` continuation byte; got something
            // else. Bail out with FFFD; advance 1 byte (don't
            // consume the malformed lead+partial run).
            *out_codepoint = 0xFFFD;
            return (start + 1) as i64;
        }
        cp = (cp << 6) | ((b & 0x3F) as u32);
    }

    // ── Reject surrogates, overlongs, out-of-range codepoints ────
    let valid = match width {
        2 => cp >= 0x80,
        3 => cp >= 0x800 && !(0xD800..=0xDFFF).contains(&cp),
        4 => (0x10000..=0x10FFFF).contains(&cp),
        _ => unreachable!(),
    };
    if !valid {
        *out_codepoint = 0xFFFD;
        return (start + 1) as i64;
    }

    *out_codepoint = cp;
    (start + width) as i64
}

/// Encode a Unicode scalar value as 1–4 UTF-8 bytes written through `out`.
/// Returns the number of bytes written. Peer to `karac_string_decode_char`
/// — used by codegen's `compile_print` / f-string `char`-arm to render a
/// codepoint as the glyph rather than the integer codepoint. Codepoints
/// outside the Unicode scalar range (≥ 0x110000) and the surrogate range
/// (0xD800..=0xDFFF) are normalized to U+FFFD (`EF BF BD`, 3 bytes).
///
/// # Safety
///
/// * `out` must point to a writable buffer of at least 4 bytes. The
///   compiler emits a 4-byte stack alloca per call site (see
///   `emit_codepoint_to_utf8`), so this precondition is satisfied at
///   every generated call site.
#[no_mangle]
pub unsafe extern "C" fn karac_string_encode_char(cp: u32, out: *mut u8) -> i64 {
    if cp < 0x80 {
        *out = cp as u8;
        1
    } else if cp < 0x800 {
        *out = 0xC0 | ((cp >> 6) as u8);
        *out.add(1) = 0x80 | ((cp & 0x3F) as u8);
        2
    } else if cp < 0x10000 {
        // Surrogates (0xD800..=0xDFFF) aren't valid scalar values; emit
        // U+FFFD instead of round-tripping the surrogate as 3 bytes (which
        // would produce malformed UTF-8 the next reader would reject).
        // Well-formed Kāra `char` values can't hold a surrogate — the
        // decoder normalizes them on the way in — but a downstream
        // arithmetic op could land here on a synthetic codepoint.
        if (0xD800..=0xDFFF).contains(&cp) {
            *out = 0xEF;
            *out.add(1) = 0xBF;
            *out.add(2) = 0xBD;
            return 3;
        }
        *out = 0xE0 | ((cp >> 12) as u8);
        *out.add(1) = 0x80 | (((cp >> 6) & 0x3F) as u8);
        *out.add(2) = 0x80 | ((cp & 0x3F) as u8);
        3
    } else if cp < 0x110000 {
        *out = 0xF0 | ((cp >> 18) as u8);
        *out.add(1) = 0x80 | (((cp >> 12) & 0x3F) as u8);
        *out.add(2) = 0x80 | (((cp >> 6) & 0x3F) as u8);
        *out.add(3) = 0x80 | ((cp & 0x3F) as u8);
        4
    } else {
        // Out-of-range codepoint → U+FFFD (3 bytes).
        *out = 0xEF;
        *out.add(1) = 0xBF;
        *out.add(2) = 0xBD;
        3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read back the heap buffer `karac_string_slice` returns as a `&str`.
    /// `n` is the expected content length (`end - start`).
    unsafe fn slice_str(s: &str, start: i64, end: i64, n: usize) -> String {
        let ptr = karac_string_slice(s.as_ptr(), s.len() as i64, start, end);
        assert!(!ptr.is_null(), "non-empty slice must return a buffer");
        let bytes = std::slice::from_raw_parts(ptr, n);
        let out = String::from_utf8(bytes.to_vec()).unwrap();
        // NUL terminator at [n] keeps the buffer printf-compatible.
        assert_eq!(*ptr.add(n), 0, "buffer must be NUL-terminated");
        out
    }

    #[test]
    fn slice_half_open_copies_subrange() {
        unsafe {
            assert_eq!(slice_str("hello world", 0, 5, 5), "hello");
            assert_eq!(slice_str("hello world", 6, 11, 5), "world");
        }
    }

    #[test]
    fn slice_full_range_copies_all() {
        unsafe {
            assert_eq!(slice_str("hello", 0, 5, 5), "hello");
        }
    }

    #[test]
    fn slice_empty_returns_null() {
        unsafe {
            let ptr = karac_string_slice("hello".as_ptr(), 5, 2, 2);
            assert!(ptr.is_null(), "empty slice (start == end) returns null");
        }
    }

    #[test]
    fn slice_multibyte_on_boundary() {
        // "héllo": 'h'=byte 0, 'é'=bytes 1..3, so 1..3 is a clean 'é'.
        unsafe {
            assert_eq!(slice_str("héllo", 1, 3, 2), "é");
            assert_eq!(slice_str("héllo", 0, 1, 1), "h");
        }
    }
}
