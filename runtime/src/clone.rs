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

// Layout of a Kāra `String` value: `{ ptr data, i64 len, i64 cap }`.
// Matches the codegen-side `string_struct_type` (Vec[u8] re-used for
// String). Layout-equivalent on every supported target.
//
// This aliases `RuntimeKaracString` rather than re-declaring the layout
// locally (as it did until SSO) because the encoding contract in
// `runtime/src/sso.rs` hangs its accessors — `is_inline` / `byte_len` /
// `data_ptr` — off that type. `karac_string_clone` must decode the tag (an
// inline source keeps its bytes *in* the descriptor), so it needs those
// accessors, and a second accessor-less copy of the layout is exactly how
// the two halves would drift apart.
use crate::RuntimeKaracString as KaracString;

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
/// SSO inline handling (`cap < 0`): the source's bytes live in its own 24
/// bytes, so the clone is a struct copy and allocates nothing — the third
/// state of the `cap` discriminant described in `runtime/src/sso.rs`. It is
/// checked first because an inline descriptor's `len`/`cap`/`data` fields
/// are overlaid data bytes, not a heap descriptor.
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
    unsafe {
        let src = &*(src as *const KaracString);
        let dst = &mut *(dst as *mut KaracString);

        // SSO: an inline source carries its bytes *inside* the 24-byte
        // descriptor, so the clone is a plain struct copy — no allocation,
        // and the copy's self-referential data pointer re-derives from the
        // destination's own address the next time `data_ptr()` runs. This
        // must be the FIRST branch: an inline descriptor's `len` field is
        // data bytes 8..=15, not a length, and its `cap` is negative, so
        // every read below would be reading garbage.
        if src.is_inline() {
            dst.data = src.data;
            dst.len = src.len;
            dst.cap = src.cap;
            return;
        }

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
}

/// SSO-aware sibling of [`karac_string_slice`]: validates identically (same
/// `slice_validate`, so the two can never disagree about a legal slice) and
/// then writes a COMPLETE `{ptr, len, cap}` descriptor to `out` — **inline
/// when the slice fits the 23-byte overlay, allocating nothing at all**,
/// heap otherwise.
///
/// This is the payoff path of the SSO campaign. Per-token `substring` in the
/// self-hosted lexer returns an owned `String` copy, and most lexemes —
/// identifiers, keywords, punctuation — are short, so the malloc that
/// dominates the post-dispatch profile disappears for them.
///
/// The encoder lives here rather than in codegen deliberately. `new_inline`
/// (`runtime/src/sso.rs`) is the single source of truth for the layout and
/// is exhaustively unit-tested; re-emitting the same byte-packing as LLVM IR
/// would create a second implementation that could drift from it, and a
/// layout mismatch between the two is silent data corruption. Codegen just
/// calls this and loads the 24 bytes back, so it owns no encoding at all.
/// The call itself is not a new cost: the allocating path already made one.
///
/// Three states, matching the `cap` discriminant:
/// * `n == 0` → `{null, 0, 0}` — the canonical empty String, unchanged from
///   `karac_string_slice`'s convention so nothing downstream sees a new
///   representation for a case that already had one.
/// * `0 < n <= INLINE_CAPACITY` → inline; `cap < 0`, no allocation, no free.
/// * `n > INLINE_CAPACITY` → heap; `{buf, n, n}`, NUL-terminated, exactly as
///   before.
///
/// # Safety
///
/// * `data` must point to a readable buffer of at least `len` bytes when
///   `len > 0`.
/// * `out` must point to a writable `RuntimeKaracString`-sized region.
/// * A heap result owns its allocation on the same `cap == len` contract as
///   `karac_string_clone`; an inline result owns nothing and must not be freed
///   (the `cap > 0` gates already skip it).
#[no_mangle]
pub unsafe extern "C" fn karac_string_slice_into(
    data: *const u8,
    len: i64,
    start: i64,
    end: i64,
    out: *mut KaracString,
) {
    unsafe {
        let (start_us, end_us) = slice_validate(data, len, start, end);
        let n = end_us - start_us;
        let out = &mut *out;

        if n == 0 {
            out.data = ptr::null_mut();
            out.len = 0;
            out.cap = 0;
            return;
        }

        if n <= KaracString::INLINE_CAPACITY {
            *out = KaracString::new_inline(std::slice::from_raw_parts(data.add(start_us), n));
            return;
        }

        // Too long to inline: the heap path, byte-for-byte what
        // `karac_string_slice` produces (alloc n+1, copy n, NUL at [n]).
        let layout = Layout::array::<u8>(n + 1).unwrap();
        let new_data = alloc(layout);
        ptr::copy_nonoverlapping(data.add(start_us), new_data, n);
        *new_data.add(n) = 0;
        out.data = new_data;
        out.len = n as i64;
        out.cap = n as i64;
    }
}

/// Try to build an INLINE `String` descriptor from `n` bytes at `src`.
/// Returns 1 if it did, 0 if `n` exceeds the inline capacity — in which case
/// `out` is NOT written and the caller must take its own heap path.
///
/// The narrowest possible shape, and narrow on purpose. Codegen already has a
/// heap path for every construction site (each with its own buffer contract —
/// `String.substring` allocates exactly `n` bytes through
/// `karac_alloc_or_panic`, while `karac_string_slice` allocates `n + 1` through
/// Rust's allocator and NUL-terminates). Folding those into one runtime
/// constructor would force one site's contract onto the other and quietly
/// change which allocator a buffer came from. So this owns ONLY the part that
/// must not be duplicated: the inline ENCODING, which lives in
/// `RuntimeKaracString::new_inline` and nowhere else.
///
/// Returning the verdict rather than taking a threshold is what keeps the
/// capacity out of codegen too: the caller branches on the answer and never
/// learns the number. A wasted call on the heap path is noise against the
/// `malloc` + `memcpy` that path was already going to do.
///
/// # Safety
///
/// * `src` must point to a readable buffer of at least `n` bytes when `n > 0`.
/// * `out` must point to a writable `RuntimeKaracString`-sized region.
/// * An inline result owns nothing and must not be freed — every buffer-free
///   gate already skips it (`cap < 0` fails the `SGT cap, 0` test).
#[no_mangle]
pub unsafe extern "C" fn karac_string_try_inline_into(
    src: *const u8,
    n: i64,
    out: *mut KaracString,
) -> i8 {
    unsafe {
        // A negative `n` cannot arise from a caller that clamped, but it would
        // become an enormous `usize` on the cast — so refuse rather than trust.
        if n < 0 || n as usize > KaracString::INLINE_CAPACITY {
            return 0;
        }
        let n = n as usize;
        // `new_inline` handles `n == 0`, but the canonical empty String is
        // `{null, 0, 0}` and every caller already has its own empty branch, so
        // an empty request never reaches here in practice.
        let bytes = if n == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(src, n)
        };
        *out = KaracString::new_inline(bytes);
        1
    }
}

/// Bounds- and UTF-8-boundary-validate a `s[start..end]` slice request,
/// returning the validated `(start, end)` as `usize`. Both failure paths
/// print to stderr and `exit(1)`, matching codegen's `emit_panic` shape (a
/// non-boundary slice is a panic, not a recoverable error — same as Rust).
///
/// Shared by `karac_string_slice` and its SSO sibling
/// `karac_string_slice_into` so the two can never disagree about what a
/// legal slice is. That mattered enough to factor out: the SSO path returns
/// an inline descriptor without allocating, and it would have been easy —
/// and silently unsound — to let it skip the char-boundary check that the
/// allocating path performs.
///
/// # Safety
///
/// `data` must point to a readable buffer of at least `len` bytes when
/// `len > 0`.
unsafe fn slice_validate(data: *const u8, len: i64, start: i64, end: i64) -> (usize, usize) {
    unsafe {
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
        (start_us, end_us)
    }
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
    unsafe {
        let (start_us, end_us) = slice_validate(data, len, start, end);
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
}

/// Allocate a fresh NUL-terminated heap buffer holding `bytes`, write its
/// length to `*out_len`, and return the buffer pointer. Empty input returns
/// `null` + `*out_len == 0` (the `karac_string_slice` empty convention; codegen
/// builds a `{null, 0, 0}` String). The buffer contract matches
/// `karac_string_slice`: `cap == len`, the allocation is `len + 1` bytes.
///
/// # Safety
/// `out_len` must point to a writable `i64`.
pub(crate) unsafe fn alloc_string_result(bytes: &[u8], out_len: *mut i64) -> *mut u8 {
    unsafe {
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
}

/// Borrow `(data, len)` as a `&str`. The Kāra String invariant guarantees valid
/// UTF-8, so this never fails in practice; on the impossible invalid-UTF-8 path
/// it fatally exits rather than returning silently-wrong bytes.
///
/// # Safety
/// `data` must point to a readable buffer of at least `len` bytes when `len > 0`.
pub(crate) unsafe fn str_from_raw<'a>(data: *const u8, len: i64) -> &'a str {
    unsafe {
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
    unsafe {
        let lowered = str_from_raw(data, len).to_lowercase();
        alloc_string_result(lowered.as_bytes(), out_len)
    }
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
    unsafe {
        let upped = str_from_raw(data, len).to_uppercase();
        alloc_string_result(upped.as_bytes(), out_len)
    }
}

/// `String.sorted()` — return a fresh String whose characters (Unicode scalar
/// values) are sorted ascending. Result matches the interpreter's
/// `chars().sort_unstable()` byte-for-byte (`src/interpreter/method_call_seq.rs`),
/// so `run` and `build` agree on multi-byte input, not just ASCII. The
/// canonical anagram key: two strings are anagrams iff their `sorted()` forms
/// are equal. Returns a fresh owned buffer.
///
/// **ASCII fast-path.** Below `0x80` a byte's value *is* its Unicode scalar
/// value, so byte order == char order and the UTF-8 decode → `Vec<char>` → sort
/// → re-encode round-trip is unnecessary: copy the bytes once (via
/// `alloc_string_result`) and sort the result buffer IN PLACE. One allocation +
/// a byte sort, versus the char path's three allocations + two UTF-8 passes —
/// ~2× faster on the `sorted()` hot path (kata #49's dominant cost) and
/// byte-identical to the char path for any ASCII string (which is the common
/// case). Multi-byte input falls through to the char sort, preserving the
/// Unicode-scalar semantics the interpreter defines.
///
/// # Safety
/// See [`karac_string_to_lowercase`].
#[no_mangle]
pub unsafe extern "C" fn karac_string_sorted(
    data: *const u8,
    len: i64,
    out_len: *mut i64,
) -> *mut u8 {
    unsafe {
        let n = if len < 0 { 0 } else { len as usize };
        let bytes: &[u8] = if n == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(data, n)
        };
        if bytes.is_ascii() {
            let out = alloc_string_result(bytes, out_len);
            if !out.is_null() {
                std::slice::from_raw_parts_mut(out, n).sort_unstable();
            }
            return out;
        }
        // Multi-byte UTF-8: sort by Unicode scalar value, matching the interpreter.
        let mut chars: Vec<char> = str_from_raw(data, len).chars().collect();
        chars.sort_unstable();
        let sorted: String = chars.into_iter().collect();
        alloc_string_result(sorted.as_bytes(), out_len)
    }
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
    unsafe {
        let trimmed = str_from_raw(data, len).trim();
        alloc_string_result(trimmed.as_bytes(), out_len)
    }
}

/// `String.trim_start()` — strip only LEADING Unicode whitespace (Rust
/// `str::trim_start`), returning a fresh owned copy. Sibling of
/// [`karac_string_trim`].
///
/// # Safety
/// See [`karac_string_to_lowercase`].
#[no_mangle]
pub unsafe extern "C" fn karac_string_trim_start(
    data: *const u8,
    len: i64,
    out_len: *mut i64,
) -> *mut u8 {
    unsafe {
        let trimmed = str_from_raw(data, len).trim_start();
        alloc_string_result(trimmed.as_bytes(), out_len)
    }
}

/// `String.trim_end()` — strip only TRAILING Unicode whitespace (Rust
/// `str::trim_end`), returning a fresh owned copy. Sibling of
/// [`karac_string_trim`].
///
/// # Safety
/// See [`karac_string_to_lowercase`].
#[no_mangle]
pub unsafe extern "C" fn karac_string_trim_end(
    data: *const u8,
    len: i64,
    out_len: *mut i64,
) -> *mut u8 {
    unsafe {
        let trimmed = str_from_raw(data, len).trim_end();
        alloc_string_result(trimmed.as_bytes(), out_len)
    }
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
    unsafe {
        let haystack = str_from_raw(data, len);
        let from_s = str_from_raw(from, from_len);
        let to_s = str_from_raw(to, to_len);
        let replaced = haystack.replace(from_s, to_s);
        alloc_string_result(replaced.as_bytes(), out_len)
    }
}

/// `String.replacen(from, to, n)` — replace at most the first `n`
/// non-overlapping occurrences of `from` with `to` (Rust `str::replacen`).
/// A negative `n` is clamped to `0` (replace nothing), matching the codegen
/// contract that a `usize`-shaped count is never fed a wrapped value.
/// Returns a fresh owned buffer.
///
/// # Safety
/// `data`/`from`/`to` are Kāra String bodies (their `*_len` byte counts);
/// `out_len` must be writable. See [`alloc_string_result`].
#[no_mangle]
pub unsafe extern "C" fn karac_string_replacen(
    data: *const u8,
    len: i64,
    from: *const u8,
    from_len: i64,
    to: *const u8,
    to_len: i64,
    n: i64,
    out_len: *mut i64,
) -> *mut u8 {
    unsafe {
        let haystack = str_from_raw(data, len);
        let from_s = str_from_raw(from, from_len);
        let to_s = str_from_raw(to, to_len);
        let count = if n < 0 { 0 } else { n as usize };
        let replaced = haystack.replacen(from_s, to_s, count);
        alloc_string_result(replaced.as_bytes(), out_len)
    }
}

/// `Vec[String].join(sep)` / `.concat()` — concatenate the vector's string
/// elements with `sep` between every adjacent pair (`concat` passes an empty
/// `sep`). `parts` is the Vec's data buffer: `count` contiguous
/// `{ptr, len, cap}` element triples (the Kāra String layout); only
/// `ptr`/`len` are read — ownership of the elements stays with the vector
/// (B-2026-07-16-14). Returns a fresh owned buffer via
/// [`alloc_string_result`]; an empty vector yields the empty string
/// (null/0, the canonical empty Kāra String).
///
/// # Safety
/// `parts` must point at `count` valid element triples whose `ptr`/`len`
/// describe live UTF-8 string bodies (null/0 for the empty string);
/// `sep`/`sep_len` is a Kāra String body; `out_len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn karac_string_join(
    parts: *const KaracStrTriple,
    count: i64,
    sep: *const u8,
    sep_len: i64,
    out_len: *mut i64,
) -> *mut u8 {
    unsafe {
        let sep_s = str_from_raw(sep, sep_len);
        let mut out = String::new();
        for i in 0..count {
            let t = &*parts.offset(i as isize);
            if i > 0 {
                out.push_str(sep_s);
            }
            out.push_str(str_from_raw(t.ptr, t.len));
        }
        alloc_string_result(out.as_bytes(), out_len)
    }
}

/// One `{ptr, len, cap}` Kāra String element as laid out inside a
/// `Vec[String]` data buffer — the read-only view [`karac_string_join`]
/// walks. `cap` is present for layout fidelity only; join never reads it.
#[repr(C)]
pub struct KaracStrTriple {
    ptr: *const u8,
    len: i64,
    #[allow(dead_code)]
    cap: i64,
}

/// `String.strip_prefix(prefix)` — if the string starts with `prefix`, return a
/// fresh OWNED copy of the remainder and set `*out_matched = 1`; otherwise set
/// `*out_matched = 0` and return null (Rust `str::strip_prefix`, but allocating
/// an owned copy rather than borrowing a `&str`). A matched empty remainder
/// (`"ab".strip_prefix("ab")`) sets matched=1 with a null/0-len result — the
/// empty String `{null, 0, 0}`, which codegen wraps as `Some("")`, distinct
/// from the no-match `None`.
///
/// # Safety
/// `data`/`prefix` are Kāra String bodies (their `*_len` byte counts);
/// `out_len` and `out_matched` must be writable. See [`alloc_string_result`].
#[no_mangle]
pub unsafe extern "C" fn karac_string_strip_prefix(
    data: *const u8,
    len: i64,
    prefix: *const u8,
    prefix_len: i64,
    out_len: *mut i64,
    out_matched: *mut i32,
) -> *mut u8 {
    unsafe {
        let s = str_from_raw(data, len);
        let p = str_from_raw(prefix, prefix_len);
        match s.strip_prefix(p) {
            Some(rest) => {
                *out_matched = 1;
                alloc_string_result(rest.as_bytes(), out_len)
            }
            None => {
                *out_matched = 0;
                *out_len = 0;
                ptr::null_mut()
            }
        }
    }
}

/// `String.strip_suffix(suffix)` — the trailing-edge sibling of
/// [`karac_string_strip_prefix`] (Rust `str::strip_suffix`): if the string ends
/// with `suffix`, return a fresh owned copy of the leading remainder with
/// `*out_matched = 1`; otherwise `*out_matched = 0` and null.
///
/// # Safety
/// See [`karac_string_strip_prefix`].
#[no_mangle]
pub unsafe extern "C" fn karac_string_strip_suffix(
    data: *const u8,
    len: i64,
    suffix: *const u8,
    suffix_len: i64,
    out_len: *mut i64,
    out_matched: *mut i32,
) -> *mut u8 {
    unsafe {
        let s = str_from_raw(data, len);
        let p = str_from_raw(suffix, suffix_len);
        match s.strip_suffix(p) {
            Some(rest) => {
                *out_matched = 1;
                alloc_string_result(rest.as_bytes(), out_len)
            }
            None => {
                *out_matched = 0;
                *out_len = 0;
                ptr::null_mut()
            }
        }
    }
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
    unsafe {
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
    unsafe {
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
    unsafe {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeKaracString;

    /// The verdict is the contract: 1 means `out` holds a complete inline
    /// descriptor, 0 means `out` was NOT touched and the caller owns the heap
    /// path. Sweep the boundary rather than spot-checking it — the off-by-one
    /// at `INLINE_CAPACITY` is the whole risk.
    #[test]
    fn try_inline_answers_by_length_and_writes_only_when_it_says_yes() {
        let src = [b'z'; 64];
        for n in 0..=40usize {
            // A sentinel that no successful write could produce, so "untouched"
            // is distinguishable from "written with something plausible".
            let mut out = RuntimeKaracString {
                data: 0xDEAD_BEEF as *mut u8,
                len: -12345,
                cap: -54321,
            };
            let ok =
                unsafe { karac_string_try_inline_into(src.as_ptr(), n as i64, &mut out as *mut _) };
            if n <= RuntimeKaracString::INLINE_CAPACITY {
                assert_eq!(ok, 1, "n={n} fits and must be inlined");
                assert!(out.cap < 0, "n={n} must carry the inline tag");
                assert_eq!(out.byte_len(), n, "n={n} length round-trip");
                assert_eq!(out.as_bytes(), &src[..n], "n={n} bytes round-trip");
            } else {
                assert_eq!(ok, 0, "n={n} exceeds capacity and must decline");
                assert_eq!(out.len, -12345, "declined call must not write `out`");
                assert_eq!(out.cap, -54321, "declined call must not write `out`");
            }
        }
    }

    /// A negative length is refused rather than cast into an enormous `usize`.
    #[test]
    fn try_inline_refuses_a_negative_length() {
        let src = [b'a'; 8];
        let mut out = RuntimeKaracString {
            data: core::ptr::null_mut(),
            len: 7,
            cap: 7,
        };
        let ok = unsafe { karac_string_try_inline_into(src.as_ptr(), -1, &mut out as *mut _) };
        assert_eq!(ok, 0);
        assert_eq!(out.len, 7, "refused call must not write `out`");
    }

    /// Multi-byte content travels verbatim — the encoder copies bytes and does
    /// not inspect them, and `substring` has already done its own boundary
    /// check by the time it calls here.
    #[test]
    fn try_inline_preserves_multibyte_content() {
        let s = "héllo wörld";
        assert!(s.len() <= RuntimeKaracString::INLINE_CAPACITY);
        let mut out = RuntimeKaracString {
            data: core::ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        let ok =
            unsafe { karac_string_try_inline_into(s.as_ptr(), s.len() as i64, &mut out as *mut _) };
        assert_eq!(ok, 1);
        assert_eq!(out.as_bytes(), s.as_bytes());
    }

    // Test-only: the heap-fallback cases free what `karac_string_slice_into`
    // allocated so the suite stays leak-clean under LSan.
    use std::alloc::dealloc;

    /// Cloning an SSO **inline** source must copy the descriptor verbatim
    /// and allocate nothing: the bytes live in the 24 bytes themselves, so
    /// the copy is the clone. Reading `src.len`/`src.data` instead — what
    /// the pre-SSO clone did — would treat overlaid data bytes as a heap
    /// descriptor and `alloc` a garbage size off a negative `cap`.
    ///
    /// This is the half of Slice 2's "tag-aware clone" that is testable
    /// before inline *construction* exists: `new_inline` is the reference
    /// encoder, so it can hand `karac_string_clone` an inline source that
    /// codegen cannot yet produce.
    #[test]
    fn clone_of_inline_source_is_a_struct_copy() {
        for text in [
            "".as_bytes(),
            b"a",
            b"hello",
            // Exactly INLINE_CAPACITY — the boundary the encoder accepts.
            b"23 bytes exactly here!!",
        ] {
            let src = KaracString::new_inline(text);
            let mut dst = KaracString {
                data: ptr::null_mut(),
                len: -1,
                cap: -1,
            };
            unsafe {
                karac_string_clone(
                    &src as *const KaracString as *const c_void,
                    &mut dst as *mut KaracString as *mut c_void,
                );
            }
            assert!(dst.is_inline(), "clone of an inline source stays inline");
            assert!(
                !dst.is_owned_heap(),
                "an inline clone owns no buffer, so drop must not free it"
            );
            assert_eq!(dst.byte_len(), text.len());
            assert_eq!(dst.as_bytes(), text);
            // Verbatim 24-byte copy: every field matches the source.
            assert_eq!(dst.data, src.data);
            assert_eq!(dst.len, src.len);
            assert_eq!(dst.cap, src.cap);
        }
    }

    /// The inline clone is self-referential: `data_ptr()` must re-derive
    /// from the *destination's* address, not carry the source's. Moving
    /// the clone (here: into a `Box`) and reading it back proves the bytes
    /// travelled with the descriptor rather than being aliased.
    #[test]
    fn inline_clone_data_ptr_follows_the_destination() {
        let src = KaracString::new_inline(b"lexeme");
        let mut dst = KaracString {
            data: ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        unsafe {
            karac_string_clone(
                &src as *const KaracString as *const c_void,
                &mut dst as *mut KaracString as *mut c_void,
            );
        }
        assert_eq!(dst.data_ptr(), &dst as *const KaracString as *const u8);
        let moved = Box::new(dst);
        assert_eq!(moved.data_ptr(), &*moved as *const KaracString as *const u8);
        assert_eq!(moved.as_bytes(), b"lexeme");
    }

    /// Drive `karac_string_slice_into` and return the descriptor it wrote.
    unsafe fn slice_into(s: &str, start: i64, end: i64) -> KaracString {
        let mut out = KaracString {
            data: ptr::null_mut(),
            len: -1,
            cap: -1,
        };
        unsafe {
            karac_string_slice_into(
                s.as_ptr(),
                s.len() as i64,
                start,
                end,
                &mut out as *mut KaracString,
            );
        }
        out
    }

    /// A slice that fits the 23-byte overlay must come back INLINE and have
    /// allocated nothing — this is the whole point of the campaign. The
    /// lexer's short lexemes (identifiers, keywords, punctuation) all land
    /// here, which is where the malloc that dominates the post-dispatch
    /// profile goes away.
    #[test]
    fn slice_into_inlines_a_short_slice() {
        let subject = "fn lexeme(x) { let ident = 1; }";
        for (start, end, want) in [
            (0i64, 2i64, "fn"),
            (3, 9, "lexeme"),
            (15, 18, "let"),
            (19, 24, "ident"),
            // Exactly INLINE_CAPACITY bytes.
            (0, 23, "fn lexeme(x) { let iden"),
        ] {
            let out = unsafe { slice_into(subject, start, end) };
            assert!(out.is_inline(), "{want:?} fits inline");
            assert!(!out.is_owned_heap(), "an inline slice owns no buffer");
            assert_eq!(out.byte_len(), want.len());
            assert_eq!(out.as_bytes(), want.as_bytes());
            // The bytes live in the descriptor itself.
            assert_eq!(out.data_ptr(), &out as *const KaracString as *const u8);
        }
    }

    /// One byte past the overlay must fall back to the heap, byte-for-byte
    /// what `karac_string_slice` already produced (`cap == len`, buffer
    /// NUL-terminated at `[len]`).
    #[test]
    fn slice_into_falls_back_to_heap_past_the_boundary() {
        let subject = "0123456789abcdefghijklmnopqrstuvwxyz";
        let out = unsafe { slice_into(subject, 0, 24) };
        assert!(!out.is_inline(), "24 bytes exceeds the 23-byte overlay");
        assert!(out.is_owned_heap(), "the heap slice owns its buffer");
        assert_eq!(out.byte_len(), 24);
        assert_eq!(out.as_bytes(), &subject.as_bytes()[..24]);
        assert_eq!(out.cap, 24, "cap mirrors len — fresh buffer, no headroom");
        unsafe {
            assert_eq!(*out.data.add(24), 0, "heap buffer stays NUL-terminated");
            dealloc(out.data, Layout::array::<u8>(25).unwrap());
        }
    }

    /// The boundary is exactly 23/24, and it is worth pinning: an off-by-one
    /// here either corrupts the descriptor (inlining 24 bytes would overwrite
    /// the flag/length trailer) or silently gives up the win at 23.
    #[test]
    fn slice_into_boundary_is_exactly_inline_capacity() {
        let subject = "x".repeat(64);
        for n in 0..=30usize {
            let out = unsafe { slice_into(&subject, 0, n as i64) };
            let want_inline = n > 0 && n <= KaracString::INLINE_CAPACITY;
            assert_eq!(
                out.is_inline(),
                want_inline,
                "n={n} should{} be inline",
                if want_inline { "" } else { " not" }
            );
            assert_eq!(out.byte_len(), n, "n={n} length round-trips");
            assert_eq!(out.as_bytes(), &subject.as_bytes()[..n], "n={n} bytes");
            if out.is_owned_heap() {
                unsafe { dealloc(out.data, Layout::array::<u8>(n + 1).unwrap()) };
            }
        }
    }

    /// An empty slice keeps the canonical `{null, 0, 0}` it always had,
    /// rather than becoming a third representation of "empty".
    #[test]
    fn slice_into_empty_stays_the_canonical_null_string() {
        let out = unsafe { slice_into("hello", 2, 2) };
        assert!(!out.is_inline());
        assert!(out.is_static(), "cap == 0");
        assert!(out.data.is_null());
        assert_eq!(out.byte_len(), 0);
    }

    /// Multi-byte content must survive the overlay unchanged — the encoder
    /// copies bytes, so a char-boundary-legal slice of UTF-8 round-trips.
    #[test]
    fn slice_into_preserves_multibyte_content() {
        let subject = "héllo wörld";
        let out = unsafe { slice_into(subject, 0, subject.len() as i64) };
        assert_eq!(subject.len(), 13, "two 2-byte chars");
        assert!(out.is_inline(), "13 bytes fits");
        assert_eq!(
            std::str::from_utf8(out.as_bytes()).unwrap(),
            subject,
            "UTF-8 survives the inline overlay"
        );
    }

    /// Read back the heap buffer `karac_string_slice` returns as a `&str`.
    /// `n` is the expected content length (`end - start`).
    unsafe fn slice_str(s: &str, start: i64, end: i64, n: usize) -> String {
        unsafe {
            let ptr = karac_string_slice(s.as_ptr(), s.len() as i64, start, end);
            assert!(!ptr.is_null(), "non-empty slice must return a buffer");
            let bytes = std::slice::from_raw_parts(ptr, n);
            let out = String::from_utf8(bytes.to_vec()).unwrap();
            // NUL terminator at [n] keeps the buffer printf-compatible.
            assert_eq!(*ptr.add(n), 0, "buffer must be NUL-terminated");
            out
        }
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

    /// Read back the heap buffer `karac_string_sorted` returns as a `String`.
    unsafe fn sorted_str(s: &str) -> String {
        unsafe {
            let mut out_len: i64 = -1;
            let ptr = karac_string_sorted(s.as_ptr(), s.len() as i64, &mut out_len);
            if s.is_empty() {
                assert!(ptr.is_null(), "empty input sorts to null");
                assert_eq!(out_len, 0);
                return String::new();
            }
            let bytes = std::slice::from_raw_parts(ptr, out_len as usize);
            let out = String::from_utf8(bytes.to_vec()).unwrap();
            assert_eq!(
                *ptr.add(out_len as usize),
                0,
                "buffer must be NUL-terminated"
            );
            out
        }
    }

    /// The reference sort: what the interpreter (and the pre-fast-path codegen
    /// helper) does — collect chars, sort by Unicode scalar, re-encode.
    fn char_sort(s: &str) -> String {
        let mut chars: Vec<char> = s.chars().collect();
        chars.sort_unstable();
        chars.into_iter().collect()
    }

    #[test]
    fn sorted_ascii_fast_path_matches_char_sort() {
        // ASCII input takes the byte-sort fast-path; the result must be
        // byte-identical to the char-sort reference for every ASCII string.
        unsafe {
            for s in [
                "hgfedcba",
                "tea",
                "dcba",
                "",
                "the quick brown fox",
                "aaa",
                "z",
            ] {
                assert_eq!(sorted_str(s), char_sort(s), "ascii sort mismatch for {s:?}");
            }
        }
    }

    #[test]
    fn sorted_multibyte_falls_back_to_char_sort() {
        // Any non-ASCII byte routes to the char path, sorting by Unicode scalar
        // value — so it stays byte-identical to the interpreter on multi-byte
        // input (e.g. 'é'=U+00E9 sorts after the ASCII letters).
        unsafe {
            for s in ["zébra", "héllo", "café ☕ münchen", "ñ", "日本語"] {
                assert_eq!(sorted_str(s), char_sort(s), "utf8 sort mismatch for {s:?}");
            }
        }
    }
}
