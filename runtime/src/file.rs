//! File handle runtime shim — Phase 8 stdlib `File` slice F2 (with
//! F4 design refinement: struct-return → out-param).
//!
//! Companion to `runtime/src/map.rs`'s shape for opaque-handle stdlib
//! types. Wraps `std::fs::File` behind a stable `extern "C"` ABI so
//! codegen (slice F3 / F4) can dispatch `File.open` / `.read` etc.
//! through extern calls.
//!
//! ## ABI surface
//!
//! Every entry point writes its `KaracIoResult` into a caller-provided
//! `*mut KaracIoResult` out-param (first argument). The struct
//! carries both the operation's success payload (handle or byte
//! count, depending on call kind) AND the IoError discriminator for
//! the codegen-side Result construction. F4's first cut returned the
//! struct by value, but the 32-byte size exceeds the 16-byte
//! register-return threshold on every supported target (System V
//! x86_64 sret, AAPCS AArch64 indirect-via-x8) — modelling the
//! resulting indirect-return ABI in LLVM IR requires `sret` / `byval`
//! parameter attributes that must match Rust's exact lowering on
//! each target. The out-param shape is ABI-trivial: no return value,
//! callee writes into the caller-allocated slot. Codegen allocas a
//! `KaracIoResult` slot in the function entry block, passes its
//! pointer as the first argument, then GEPs + loads the field values.
//!
//! The kind tag follows the order of the `IoError` enum variants
//! declared in `runtime/stdlib/io.kara`:
//!
//!   0 → Ok  (no error; consult `value`/`handle`)
//!   1 → NotFound
//!   2 → PermissionDenied
//!   3 → AlreadyExists
//!   4 → UnexpectedEof
//!   5 → InvalidUtf8 (mapped from `ErrorKind::InvalidData`)
//!   6 → Interrupted
//!   7 → Other (payload in `error_msg_ptr` / `_len`)
//!
//! For `kind == 7` (Other), `error_msg_ptr` points at a freshly
//! `std::alloc::alloc`'d byte buffer of length `error_msg_len`; the
//! Kāra-side codegen takes ownership and frees through the standard
//! `String` drop path (`{ptr, len, cap}` triple where `cap == len`).
//! For all other *error* tags `error_msg_ptr` is null and `_len` is
//! zero. One success path also uses these fields: `read_to_string`
//! returns `kind == 0` with the file's UTF-8 bytes in
//! `error_msg_ptr` / `error_msg_len` (its Ok payload is a `String`,
//! which doesn't fit the single-i64 `value` field) — codegen's
//! `FileOkKind::StringPayload` arm rebuilds the `String` from them.
//!
//! ## Lifetime model
//!
//! `karac_runtime_file_open` / `_create` / `_append` `Box::into_raw`
//! a `KaracFile` and return the raw pointer in
//! `KaracIoResult::handle`. Kāra-side `FreeFileHandle` cleanup
//! actions emit a single `karac_runtime_file_close(handle)` call at
//! scope exit, which reconstructs the `Box` and drops it — releasing
//! the underlying OS file descriptor through `std::fs::File`'s own
//! Drop impl.
//!
//! ## Threading model
//!
//! `KaracFile` wraps `std::fs::File` in a `Mutex` so the same handle
//! can flow across thread boundaries via `Arc` clones (parity with
//! the interpreter's `Value::File(Arc<Mutex<std::fs::File>>)`
//! variant). v1 doesn't expose handle cloning at the Kāra surface,
//! but the FFI layer keeps the option open without ABI churn.

use std::alloc::{alloc, Layout};
use std::io::{Read, Seek, SeekFrom, Write};
use std::ptr;
use std::sync::Mutex;

// ── Public ABI types ────────────────────────────────────────────

/// Opaque handle wrapping a `std::fs::File`. The `Mutex` makes the
/// handle Send + Sync without forcing `std::fs::File` to be cloneable
/// at the FFI boundary; v1 takes one lock per syscall (no contention
/// matters — the interpreter is single-threaded at this surface and
/// compiled binaries serialize each handle's accesses anyway).
#[repr(C)]
pub struct KaracFile {
    inner: Mutex<std::fs::File>,
}

/// Unified result shape for every File operation. Codegen (slice F4)
/// destructures this into the surface-level `Result[T, IoError]`
/// — `value` carries the success payload (handle as `*mut KaracFile`
/// cast to `i64` for open-family; bytes-read/written for read/write;
/// zero for flush), `error_kind` is the IoError variant tag, and
/// `error_msg_ptr`/`_len` are the payload bytes for the `Other`
/// variant (null and zero otherwise).
///
/// `#[repr(C)]` is load-bearing: codegen GEPs into the field layout
/// directly. Reordering or inserting fields here is an ABI break
/// against codegen; the offsets are pinned by `tests::test_io_result_layout_pinned`.
#[repr(C)]
pub struct KaracIoResult {
    /// Success payload — handle pointer (cast to i64) for open-family,
    /// byte count for read/write, zero for flush. Negative bytes never
    /// occur; the underlying `std::io::Read`/`Write` interfaces return
    /// `usize`, capped at i64::MAX in practice (>>> any single-syscall
    /// transfer). Set to zero on error.
    pub value: i64,
    /// IoError variant tag per the module-level table. Zero means OK.
    pub error_kind: i32,
    /// Padding for natural alignment of the trailing pointer.
    pub _pad: i32,
    /// Owned byte buffer carrying the `IoError.Other(String)` message.
    /// Non-null only when `error_kind == 7`. The Kāra-side caller takes
    /// ownership: the `String` aggregate constructed in codegen frees
    /// the buffer at scope exit through the standard 3-word drop path.
    pub error_msg_ptr: *mut u8,
    /// Length of `error_msg_ptr` in bytes. `cap` equals `len` (the
    /// codegen-side `String` drop path expects `cap >= len`; we pick
    /// the smallest layout that satisfies the invariant).
    pub error_msg_len: i64,
}

// SAFETY: KaracIoResult is plain data + an optionally-owned byte
// buffer. Once returned from an FFI call, exclusive ownership of
// `error_msg_ptr` transfers to the caller. No interior aliasing.
unsafe impl Send for KaracIoResult {}
unsafe impl Sync for KaracIoResult {}

// ── Internal helpers ────────────────────────────────────────────

/// Build a successful result with the given payload value (handle as
/// i64 for open-family, byte count for read/write, zero for flush).
fn ok(value: i64) -> KaracIoResult {
    KaracIoResult {
        value,
        error_kind: 0,
        _pad: 0,
        error_msg_ptr: ptr::null_mut(),
        error_msg_len: 0,
    }
}

/// Build a successful result carrying an owned UTF-8 byte buffer in the
/// `error_msg_ptr` / `error_msg_len` fields. Those fields normally hold
/// the `IoError.Other` message and are null on success — but for
/// `read_to_string` (whose Ok payload is a `String`, not a single i64
/// `value`) they are reused to carry the success content. Codegen's
/// `FileOkKind::StringPayload` arm rebuilds the Kāra `String` aggregate
/// `{ptr, len, cap}` (cap == len) from these two fields; the same drop
/// path that frees an `Other` message frees this buffer. The empty
/// string returns a null pointer + zero length — the `{null, 0, 0}`
/// String shape with `cap == 0`, which the drop path skips.
fn ok_string(s: &str) -> KaracIoResult {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return KaracIoResult {
            value: 0,
            error_kind: 0,
            _pad: 0,
            error_msg_ptr: ptr::null_mut(),
            error_msg_len: 0,
        };
    }
    // SAFETY: layout is valid (non-zero length, byte alignment).
    let layout = Layout::array::<u8>(bytes.len()).expect("file contents layout");
    let buf = unsafe { alloc(layout) };
    if buf.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len());
    }
    KaracIoResult {
        value: 0,
        error_kind: 0,
        _pad: 0,
        error_msg_ptr: buf,
        error_msg_len: bytes.len() as i64,
    }
}

/// Build an error result from a `std::io::Error`. Maps `ErrorKind` to
/// the IoError variant tag table; falls back to `Other` with the
/// formatted error message for kinds not in the v1 variant set.
///
/// The `Other` payload is allocated via `std::alloc::alloc` (matching
/// the rest of the runtime crate's allocator usage) — the Kāra-side
/// `String` aggregate's drop path uses the same allocator family, so
/// the buffer is safe to free through there.
fn err(e: &std::io::Error) -> KaracIoResult {
    use std::io::ErrorKind;
    let kind: i32 = match e.kind() {
        ErrorKind::NotFound => 1,
        ErrorKind::PermissionDenied => 2,
        ErrorKind::AlreadyExists => 3,
        ErrorKind::UnexpectedEof => 4,
        ErrorKind::InvalidData => 5,
        ErrorKind::Interrupted => 6,
        _ => 7,
    };
    if kind != 7 {
        return KaracIoResult {
            value: 0,
            error_kind: kind,
            _pad: 0,
            error_msg_ptr: ptr::null_mut(),
            error_msg_len: 0,
        };
    }
    let msg = e.to_string();
    let bytes = msg.as_bytes();
    if bytes.is_empty() {
        return KaracIoResult {
            value: 0,
            error_kind: 7,
            _pad: 0,
            error_msg_ptr: ptr::null_mut(),
            error_msg_len: 0,
        };
    }
    // SAFETY: layout is valid (non-zero length, byte alignment).
    let layout = Layout::array::<u8>(bytes.len()).expect("io error msg layout");
    let buf = unsafe { alloc(layout) };
    if buf.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len());
    }
    KaracIoResult {
        value: 0,
        error_kind: 7,
        _pad: 0,
        error_msg_ptr: buf,
        error_msg_len: bytes.len() as i64,
    }
}

/// Read the Kāra-side `String` view (`{ptr, len, cap}`) at `path_ptr`
/// (passed as `*const u8` + `i64 len` by codegen) into an owned
/// `String` for `std::fs::OpenOptions`. The borrow is read-only; the
/// caller (codegen) retains ownership of the bytes.
unsafe fn read_path(path_ptr: *const u8, path_len: i64) -> Option<String> {
    if path_ptr.is_null() || path_len < 0 {
        return None;
    }
    let slice = std::slice::from_raw_parts(path_ptr, path_len as usize);
    std::str::from_utf8(slice).ok().map(|s| s.to_string())
}

// ── extern "C" entry points ─────────────────────────────────────
//
// **ABI shape.** Every entry point writes its `KaracIoResult` into a
// caller-provided `*mut KaracIoResult` out-param rather than
// returning the struct by value. The struct is 32 bytes — past the
// 16-byte register-return threshold on both x86_64 SystemV (sret via
// hidden pointer) and AArch64 / Apple Darwin (indirect via x8 / x0).
// Modelling indirect returns in LLVM IR requires explicit
// `byval`/`sret` parameter attributes that must match Rust's exact
// lowering on each target — F4's first cut returned the struct by
// value, which produced an LLVM call that didn't match Rust's
// extern "C" sret convention and corrupted the stack (silent hang
// during the F4 smoke test, 2026-05-26). The out-param shape is
// ABI-trivial: a `*mut KaracIoResult` first arg, the callee writes
// into it, no return value. Codegen at F4 allocas a stack slot and
// passes its address; the load-from-slot following the call extracts
// the fields verbatim.

/// Open `path` in read-only mode. Codegen emits this for `File.open(p)`.
///
/// # Safety
///
/// `out` must point to a writable, suitably-aligned `KaracIoResult`
/// slot. `path_ptr` must point to `path_len` valid bytes of UTF-8
/// text. On success (`out.error_kind == 0`) the handle in `out.value`
/// is owned by the caller and must be released through
/// `karac_runtime_file_close`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_file_open(
    out: *mut KaracIoResult,
    path_ptr: *const u8,
    path_len: i64,
) {
    let Some(path) = read_path(path_ptr, path_len) else {
        *out = err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "path is not valid UTF-8",
        ));
        return;
    };
    *out = match std::fs::OpenOptions::new().read(true).open(&path) {
        Ok(f) => {
            let handle = Box::into_raw(Box::new(KaracFile {
                inner: Mutex::new(f),
            }));
            ok(handle as i64)
        }
        Err(e) => err(&e),
    };
}

/// Open `path` in write+truncate mode (creating if absent). Codegen
/// emits this for `File.create(p)`.
///
/// # Safety
///
/// See `karac_runtime_file_open`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_file_create(
    out: *mut KaracIoResult,
    path_ptr: *const u8,
    path_len: i64,
) {
    let Some(path) = read_path(path_ptr, path_len) else {
        *out = err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "path is not valid UTF-8",
        ));
        return;
    };
    *out = match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
    {
        Ok(f) => {
            let handle = Box::into_raw(Box::new(KaracFile {
                inner: Mutex::new(f),
            }));
            ok(handle as i64)
        }
        Err(e) => err(&e),
    };
}

/// Open `path` in append mode (creating if absent, positioning writes
/// at end-of-file). Codegen emits this for `File.append(p)`.
///
/// # Safety
///
/// See `karac_runtime_file_open`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_file_append(
    out: *mut KaracIoResult,
    path_ptr: *const u8,
    path_len: i64,
) {
    let Some(path) = read_path(path_ptr, path_len) else {
        *out = err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "path is not valid UTF-8",
        ));
        return;
    };
    *out = match std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
    {
        Ok(f) => {
            let handle = Box::into_raw(Box::new(KaracFile {
                inner: Mutex::new(f),
            }));
            ok(handle as i64)
        }
        Err(e) => err(&e),
    };
}

/// Read the entire contents of `path` into a UTF-8 `String`. Codegen
/// emits this for `FileSystem.read_to_string(path)` — a one-shot
/// slurp that needs no live `File` handle (unlike the `_open` + `_read`
/// loop). On success the string bytes are returned through
/// `out.error_msg_ptr` / `out.error_msg_len` (the success-payload reuse
/// of the buffer fields described in the module header) with
/// `out.error_kind == 0`; codegen's `FileOkKind::StringPayload` arm
/// rebuilds the `String` aggregate from them. Non-UTF-8 file contents
/// map to `IoError.InvalidUtf8` (tag 5) via `std::fs::read_to_string`'s
/// own `ErrorKind::InvalidData`.
///
/// # Safety
///
/// `out` must point to a writable, suitably-aligned `KaracIoResult`
/// slot. `path_ptr` must point to `path_len` valid bytes of UTF-8 text.
/// On success the buffer in `out.error_msg_ptr` is owned by the caller
/// and freed through the Kāra `String` drop path.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_file_read_to_string(
    out: *mut KaracIoResult,
    path_ptr: *const u8,
    path_len: i64,
) {
    let Some(path) = read_path(path_ptr, path_len) else {
        *out = err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "path is not valid UTF-8",
        ));
        return;
    };
    *out = match std::fs::read_to_string(&path) {
        Ok(s) => ok_string(&s),
        Err(e) => err(&e),
    };
}

/// Read up to `buf_len` bytes from `handle` into `buf_ptr`. On success
/// `out.value` holds the byte count (0 = clean EOF, not an error).
///
/// # Safety
///
/// `out` must point to a writable, suitably-aligned `KaracIoResult`
/// slot. `handle` must be a live pointer returned from `_open` /
/// `_create` / `_append` and not yet closed. `buf_ptr` must point to
/// writable memory of at least `buf_len` bytes; `buf_len >= 0`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_file_read(
    out: *mut KaracIoResult,
    handle: *mut KaracFile,
    buf_ptr: *mut u8,
    buf_len: i64,
) {
    if handle.is_null() {
        *out = err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "null file handle",
        ));
        return;
    }
    if buf_len < 0 {
        *out = err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "negative buffer length",
        ));
        return;
    }
    let file = &*handle;
    let mut guard = match file.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let buf = std::slice::from_raw_parts_mut(buf_ptr, buf_len as usize);
    *out = match guard.read(buf) {
        Ok(n) => ok(n as i64),
        Err(e) => err(&e),
    };
}

/// Write up to `buf_len` bytes from `buf_ptr` to `handle`. On success
/// `out.value` holds the byte count.
///
/// # Safety
///
/// See `karac_runtime_file_read`; `buf_ptr` is read-only here.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_file_write(
    out: *mut KaracIoResult,
    handle: *mut KaracFile,
    buf_ptr: *const u8,
    buf_len: i64,
) {
    if handle.is_null() {
        *out = err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "null file handle",
        ));
        return;
    }
    if buf_len < 0 {
        *out = err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "negative buffer length",
        ));
        return;
    }
    let file = &*handle;
    let mut guard = match file.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let buf = std::slice::from_raw_parts(buf_ptr, buf_len as usize);
    *out = match guard.write(buf) {
        Ok(n) => ok(n as i64),
        Err(e) => err(&e),
    };
}

/// Flush the file's write buffer. On success `out.value == 0`.
///
/// # Safety
///
/// `out` must point to a writable, suitably-aligned `KaracIoResult`
/// slot. `handle` must be a live pointer returned from an open-family
/// call.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_file_flush(out: *mut KaracIoResult, handle: *mut KaracFile) {
    if handle.is_null() {
        *out = err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "null file handle",
        ));
        return;
    }
    let file = &*handle;
    let mut guard = match file.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    *out = match guard.flush() {
        Ok(()) => ok(0),
        Err(e) => err(&e),
    };
}

/// Close the file handle and free its memory. Called by codegen's
/// scope-exit `FreeFileHandle` cleanup action.
///
/// # Safety
///
/// `handle` must be a live pointer returned from an open-family call
/// and not previously closed. After this call the pointer is invalid.
/// Calling with `null` is safe (no-op) — codegen may emit a guarded
/// null check.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_file_close(handle: *mut KaracFile) {
    if handle.is_null() {
        return;
    }
    drop(Box::from_raw(handle));
}

/// Seek a file handle. v1 doesn't expose seek through the Kāra
/// surface (deferred to a follow-on slice), but the FFI symbol is
/// exported here so codegen can call it once the surface lands —
/// avoids a runtime-rebuild requirement when seek ships.
///
/// `whence`: 0 = Start, 1 = Current, 2 = End. On success
/// `out.value` holds the new position.
///
/// # Safety
///
/// `out` must point to a writable `KaracIoResult`. `handle` must be a
/// live pointer returned from an open-family call.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_file_seek(
    out: *mut KaracIoResult,
    handle: *mut KaracFile,
    whence: u8,
    offset: i64,
) {
    if handle.is_null() {
        *out = err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "null file handle",
        ));
        return;
    }
    let file = &*handle;
    let mut guard = match file.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let pos = match whence {
        0 => SeekFrom::Start(offset as u64),
        1 => SeekFrom::Current(offset),
        2 => SeekFrom::End(offset),
        _ => {
            *out = err(&std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid seek whence",
            ));
            return;
        }
    };
    *out = match guard.seek(pos) {
        Ok(p) => ok(p as i64),
        Err(e) => err(&e),
    };
}

/// `stdin.read_line() -> Result[String, IoError]` — read one line
/// (including the trailing `\n`, matching the interpreter's
/// `("Stdin", "read_line")` arm which returns `Value::String(buf)`
/// verbatim). EOF returns `Ok("")` (Rust's `read_line` yields `Ok(0)`
/// with an empty buffer at end of input), which `ok_string("")` lowers
/// to the canonical `{null, 0, 0}` empty String. The Ok payload travels
/// in the `error_msg_ptr`/`error_msg_len` fields, which codegen's
/// `FileOkKind::StringPayload` arm rebuilds into the Kāra `String`
/// aggregate — identical to `FileSystem.read_to_string`.
///
/// # Safety
///
/// `out` must point to a writable `KaracIoResult` slot, which codegen
/// allocas on the caller's stack before the call.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_stdin_read_line(out: *mut KaracIoResult) {
    if out.is_null() {
        return;
    }
    let mut buf = String::new();
    *out = match std::io::stdin().read_line(&mut buf) {
        Ok(_) => ok_string(&buf),
        Err(e) => err(&e),
    };
}

/// `stdin.lines()` per-line pull (phase-8 `Stdin.lines()` slice). Reads one
/// line from stdin, strips the trailing `\n` / `\r\n`, and writes
/// `Result.Ok(stripped)` into `out`. Return code drives the codegen for-loop
/// (mirrors the interpreter's `Value::StdinLines` drain):
///
/// - `0` — EOF: nothing written, stop the loop (no body run).
/// - `1` — got a line: `Ok(stripped)` in `out`, run the body and continue.
/// - `2` — read error (e.g. invalid UTF-8): `Err(IoError)` in `out`, run the
///   body once, then stop.
///
/// EOF is unambiguous: `read_line` returns `Ok(0)` only at end of input, while
/// even an empty line yields at least `"\n"` (`Ok(1)`), so the empty-vs-EOF
/// collision the raw `read_line` extern would have never arises here.
///
/// # Safety
///
/// `out` must point to a writable `KaracIoResult` slot (codegen-allocated).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_stdin_next_line(out: *mut KaracIoResult) -> i32 {
    if out.is_null() {
        return 0;
    }
    let mut buf = String::new();
    match std::io::stdin().read_line(&mut buf) {
        Ok(0) => 0,
        Ok(_) => {
            if buf.ends_with('\n') {
                buf.pop();
                if buf.ends_with('\r') {
                    buf.pop();
                }
            }
            *out = ok_string(&buf);
            1
        }
        Err(e) => {
            *out = err(&e);
            2
        }
    }
}

/// `stdin.read_to_string() -> Result[String, IoError]` — slurp all of
/// stdin to EOF. Companion to `read_line`; same `KaracIoResult`
/// String-payload ABI and the same `("Stdin", "read_to_string")`
/// interpreter arm. Empty input → `Ok("")`.
///
/// # Safety
///
/// `out` must point to a writable `KaracIoResult` slot (codegen-allocated).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_stdin_read_to_string(out: *mut KaracIoResult) {
    if out.is_null() {
        return;
    }
    use std::io::Read;
    let mut buf = String::new();
    *out = match std::io::stdin().read_to_string(&mut buf) {
        Ok(_) => ok_string(&buf),
        Err(e) => err(&e),
    };
}

/// `fs.read_lines(path) -> Result[Vec[String], IoError]` codegen backing
/// (B-2026-07-11-38). Reads the whole file and splits into lines with Rust
/// `str::lines()` semantics: split on `\n`, strip a trailing `\r` (CRLF), a
/// final newline yields no trailing empty element. Two out-params so the
/// `Vec[String]` success payload and the `IoError` discriminator travel
/// separately (a `Vec` is 3 words — it does not fit `KaracIoResult`'s single
/// `value`):
///
///   - `out_io` (first, per the file-IO out-param-first convention): a
///     `KaracIoResult` carrying ONLY the error discriminator — `ok(0)` on
///     success, `err(&e)` (kind + optional `Other` message) on failure.
///     Codegen branches on `out_io.error_kind` to build `Result.Ok` / `Err`.
///   - `out_vec`: on success, the `Vec[String]` in Kāra `{ptr,len,cap}` shape,
///     each element a heap `RuntimeKaracString` with `cap == len` — built
///     exactly like `karac_runtime_env_args_into`, so the codegen scope-exit
///     cleanup frees the element buffer and each String like any owned Kāra
///     aggregate. On error (and for an empty file) the canonical `{null,0,0}`
///     (matching `Vec.new()`, so cleanup is a no-op and no stale `cap>0` is
///     ever freed).
///
/// # Safety
///
/// `path_ptr`/`path_len` describe a UTF-8 path (borrowed Kāra `String` bytes);
/// `out_io` / `out_vec` point at writable codegen-allocated slots.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_fs_read_lines(
    out_io: *mut KaracIoResult,
    out_vec: *mut crate::KaracVec,
    path_ptr: *const u8,
    path_len: i64,
) {
    if out_io.is_null() || out_vec.is_null() {
        return;
    }
    let empty_vec = crate::KaracVec {
        data: ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    let path = if path_ptr.is_null() || path_len <= 0 {
        String::new()
    } else {
        let slice = std::slice::from_raw_parts(path_ptr, path_len as usize);
        match std::str::from_utf8(slice) {
            Ok(s) => s.to_string(),
            Err(_) => {
                *out_vec = empty_vec;
                *out_io = err(&std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "read_lines: path is not valid UTF-8",
                ));
                return;
            }
        }
    };
    match std::fs::read_to_string(&path) {
        Err(e) => {
            *out_vec = empty_vec;
            *out_io = err(&e);
        }
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            let count = lines.len();
            if count == 0 {
                *out_vec = empty_vec;
            } else {
                let elem_size = std::mem::size_of::<crate::RuntimeKaracString>();
                let align = std::mem::align_of::<crate::RuntimeKaracString>();
                let vec_layout = Layout::from_size_align(elem_size * count, align)
                    .expect("read_lines Vec layout");
                let buf = alloc(vec_layout) as *mut crate::RuntimeKaracString;
                if buf.is_null() {
                    std::alloc::handle_alloc_error(vec_layout);
                }
                for (i, line) in lines.iter().enumerate() {
                    let bytes = line.as_bytes();
                    let s = if bytes.is_empty() {
                        crate::RuntimeKaracString {
                            data: ptr::null_mut(),
                            len: 0,
                            cap: 0,
                        }
                    } else {
                        let str_layout = Layout::array::<u8>(bytes.len()).unwrap();
                        let str_buf = alloc(str_layout);
                        if str_buf.is_null() {
                            std::alloc::handle_alloc_error(str_layout);
                        }
                        ptr::copy_nonoverlapping(bytes.as_ptr(), str_buf, bytes.len());
                        crate::RuntimeKaracString {
                            data: str_buf,
                            len: bytes.len() as i64,
                            cap: bytes.len() as i64,
                        }
                    };
                    ptr::write(buf.add(i), s);
                }
                *out_vec = crate::KaracVec {
                    data: buf as *mut u8,
                    len: count as i64,
                    cap: count as i64,
                };
            }
            *out_io = ok(0);
        }
    }
}

/// `FileSystem.write(path, contents) -> Result[Unit, IoError]` — one-shot
/// whole-file write (create-or-truncate). Codegen counterpart to the
/// interpreter's `("FileSystem", "write")` arm (`std::fs::write`). Same
/// `KaracIoResult` out-param ABI as the open-family; the Ok payload is
/// `Unit`, so success writes `ok(0)` (codegen's `FileOkKind::Unit` arm
/// ignores the value field). `path`/`contents` are borrowed Kāra `String`
/// views (`*const u8` + `i64 len`); the caller retains ownership.
///
/// # Safety
///
/// `out` must point to a writable `KaracIoResult` slot (codegen-allocated);
/// `path_ptr`/`contents_ptr` must describe valid byte ranges of the given
/// lengths.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_fs_write(
    out: *mut KaracIoResult,
    path_ptr: *const u8,
    path_len: i64,
    contents_ptr: *const u8,
    contents_len: i64,
) {
    let Some(path) = read_path(path_ptr, path_len) else {
        *out = err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "path is not valid UTF-8",
        ));
        return;
    };
    // Contents are raw bytes — no UTF-8 round-trip needed (a Kāra String is
    // already valid UTF-8, but `fs::write` takes `&[u8]` anyway).
    let contents: &[u8] = if contents_ptr.is_null() || contents_len <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(contents_ptr, contents_len as usize)
    };
    *out = match std::fs::write(&path, contents) {
        Ok(()) => ok(0),
        Err(e) => err(&e),
    };
}

// ── DataFrame CSV serialization (phase-11 CSV leg) ─────────────────

/// RFC-4180-lite cell quoting — a cell containing a comma, double-quote,
/// CR, or LF is wrapped in double quotes with embedded quotes doubled;
/// anything else passes through. Byte-identical to the interpreter's
/// `write_csv` quoting (`src/interpreter/method_call_dataframe.rs`).
fn csv_quote(cell: &str) -> String {
    if cell.contains(',') || cell.contains('"') || cell.contains('\n') || cell.contains('\r') {
        format!("\"{}\"", cell.replace('"', "\"\""))
    } else {
        cell.to_string()
    }
}

/// `df.write_csv(path) -> Result[Unit, IoError]` — serialize a compiled
/// DataFrame to a CSV file (the codegen twin of the interpreter arm; the
/// serialization rules — header of names in schema order, `Display`
/// formatting per cell, NULL → empty cell, RFC-4180-lite quoting — are
/// identical, and Rust's `{}` on i64/f64 IS the interpreter's `Display`,
/// so output stays byte-identical across backends).
///
/// Walks codegen's fixed control-block layouts directly (the same ABI
/// coupling as `KaracVec` / the fs helpers; layouts documented in
/// `src/codegen/dataframe.rs` / `column.rs` headers and pinned by the
/// E2E round-trip test):
///
/// - DataFrame control: `{ ptr entries, i64 len, i64 capacity }`
/// - entry (stride 40): `{ ptr name_data, i64 name_len, ptr col_ctrl,
///   i64 elem_size, i64 kind }` — kind: 0 = other (bool at size 1),
///   1 = signed int, 2 = unsigned int, 3 = float, 4 = String
/// - Column control: `{ ptr data, ptr null_bitmap, i64 len, i64 cap }`;
///   validity bit `i` = `(bitmap[i/8] >> (i%8)) & 1`, 1 = valid
/// - String element: the 24-byte `{ ptr, i64 len, i64 cap }` struct inline
///   in the data buffer.
///
/// # Safety
///
/// `out` must point to a writable `KaracIoResult` slot; `df_ctrl` must be
/// a live DataFrame control block laid out as above; `path_ptr`/`path_len`
/// must describe a valid byte range.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_df_write_csv(
    out: *mut KaracIoResult,
    df_ctrl: *const u8,
    path_ptr: *const u8,
    path_len: i64,
) {
    let Some(path) = read_path(path_ptr, path_len) else {
        *out = err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "path is not valid UTF-8",
        ));
        return;
    };
    let entries = *(df_ctrl as *const *const u8);
    let n_cols = *(df_ctrl.add(8) as *const i64);

    struct Col {
        name: String,
        data: *const u8,
        bitmap: *const u8,
        len: i64,
        elem_size: i64,
        kind: i64,
    }
    let mut cols: Vec<Col> = Vec::with_capacity(n_cols.max(0) as usize);
    for i in 0..n_cols.max(0) as usize {
        let e = entries.add(i * 40);
        let name_data = *(e as *const *const u8);
        let name_len = *(e.add(8) as *const i64);
        let col_ctrl = *(e.add(16) as *const *const u8);
        let name = if name_data.is_null() || name_len <= 0 {
            String::new()
        } else {
            String::from_utf8_lossy(std::slice::from_raw_parts(name_data, name_len as usize))
                .into_owned()
        };
        cols.push(Col {
            name,
            data: *(col_ctrl as *const *const u8),
            bitmap: *(col_ctrl.add(8) as *const *const u8),
            len: *(col_ctrl.add(16) as *const i64),
            elem_size: *(e.add(24) as *const i64),
            kind: *(e.add(32) as *const i64),
        });
    }

    let mut text = String::new();
    let header: Vec<String> = cols.iter().map(|c| csv_quote(&c.name)).collect();
    text.push_str(&header.join(","));
    text.push('\n');
    let height = cols.first().map_or(0, |c| c.len).max(0);
    for row in 0..height as usize {
        let mut cells: Vec<String> = Vec::with_capacity(cols.len());
        for c in cols.iter() {
            let valid = !c.bitmap.is_null() && (*c.bitmap.add(row / 8) >> (row % 8)) & 1 == 1;
            if !valid {
                cells.push(String::new());
                continue;
            }
            let p = c.data.add(row * c.elem_size as usize);
            let cell = match (c.kind, c.elem_size) {
                (1, 1) => format!("{}", *(p as *const i8)),
                (1, 2) => format!("{}", *(p as *const i16)),
                (1, 4) => format!("{}", *(p as *const i32)),
                (1, 8) => format!("{}", *(p as *const i64)),
                (2, 1) => format!("{}", *p),
                (2, 2) => format!("{}", *(p as *const u16)),
                (2, 4) => format!("{}", *(p as *const u32)),
                (2, 8) => format!("{}", *(p as *const u64)),
                (3, 4) => format!("{}", *(p as *const f32)),
                (3, 8) => format!("{}", *(p as *const f64)),
                (0, 1) => (if *p != 0 { "true" } else { "false" }).to_string(),
                (4, _) => {
                    let sptr = *(p as *const *const u8);
                    let slen = *(p.add(8) as *const i64);
                    if sptr.is_null() || slen <= 0 {
                        String::new()
                    } else {
                        String::from_utf8_lossy(std::slice::from_raw_parts(sptr, slen as usize))
                            .into_owned()
                    }
                }
                // Unknown kind/size combination — an empty cell rather than
                // UB; new element classes must extend this table.
                _ => String::new(),
            };
            cells.push(csv_quote(&cell));
        }
        text.push_str(&cells.join(","));
        text.push('\n');
    }

    *out = match std::fs::write(&path, text) {
        Ok(()) => ok(0),
        Err(e) => err(&e),
    };
}

/// RFC-4180-lite CSV record/field splitter — the runtime twin of the
/// interpreter's `parse_csv_to_dataframe` splitter
/// (`src/interpreter/method_call_dataframe.rs`); the two must stay
/// semantically identical (run-vs-build parity). `Some(s)` = value cell,
/// `None` = NULL (an UNQUOTED empty cell; a quoted cell — even `""` — is
/// always a value). Errors on an unterminated quoted cell.
#[allow(clippy::type_complexity)]
fn csv_split_records(text: &str) -> Result<Vec<Vec<Option<String>>>, String> {
    let mut records: Vec<Vec<Option<String>>> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut fields: Vec<Option<String>> = Vec::new();
    let mut chars = text.chars().peekable();
    let mut in_quotes = false;
    fn flush(field: &mut String, quoted: &mut bool, fields: &mut Vec<Option<String>>) {
        let cell = std::mem::take(field);
        fields.push(if cell.is_empty() && !*quoted {
            None
        } else {
            Some(cell)
        });
        *quoted = false;
    }
    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' => {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                other => field.push(other),
            }
            continue;
        }
        match c {
            '"' => {
                in_quotes = true;
                quoted = true;
            }
            ',' => flush(&mut field, &mut quoted, &mut fields),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                flush(&mut field, &mut quoted, &mut fields);
                records.push(std::mem::take(&mut fields));
            }
            '\n' => {
                flush(&mut field, &mut quoted, &mut fields);
                records.push(std::mem::take(&mut fields));
            }
            other => field.push(other),
        }
    }
    if in_quotes {
        return Err("CSV parse error: unterminated quoted cell".to_string());
    }
    if !field.is_empty() || quoted || !fields.is_empty() {
        flush(&mut field, &mut quoted, &mut fields);
        records.push(fields);
    }
    Ok(records)
}

/// malloc-compatible allocation of `size` bytes (8-aligned), zeroed.
/// Freed by codegen's `free` (the same pairing `fs_read_lines` relies on).
/// Returns null for `size == 0` — codegen's frees are null-guarded.
unsafe fn df_alloc_zeroed(size: usize) -> *mut u8 {
    if size == 0 {
        return ptr::null_mut();
    }
    let layout = Layout::from_size_align(size, 8).expect("df_read_csv layout");
    let p = std::alloc::alloc_zeroed(layout);
    if p.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    p
}

/// Copy `bytes` into a fresh malloc-compatible buffer (null for empty).
unsafe fn df_alloc_bytes(bytes: &[u8]) -> *mut u8 {
    if bytes.is_empty() {
        return ptr::null_mut();
    }
    let layout = Layout::array::<u8>(bytes.len()).unwrap();
    let p = alloc(layout);
    if p.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
    p
}

/// `DataFrame.read_csv(path) -> Result[DataFrame, IoError]` — parse a CSV
/// file into a freshly-built DataFrame control-block graph (the codegen
/// twin of the interpreter arm; parsing/inference semantics identical).
/// First record = column names; per-column inference over value cells
/// (all i64 → kind 1/size 8, else all f64 → kind 3/size 8, else String →
/// kind 4/size 24); an unquoted-empty cell is a NULL slot (validity bit 0,
/// zeroed data). Ragged rows / empty file / unterminated quote →
/// `IoError.Other(<msg>)`; read errors map through the std error kinds.
///
/// Every allocation (control block, entries, names, column controls,
/// data buffers, bitmaps, String cell heaps) is malloc-compatible and laid
/// out exactly as codegen builds frames itself, so the caller's ordinary
/// `FreeDataFrame` cleanup frees the whole graph — String cells carry
/// `cap == len` so `column_free_allocations`' cap-guard frees them, and
/// NULL/empty cells are `{null, 0, 0}` (skipped).
///
/// # Safety
///
/// `out_io` must point to a writable `KaracIoResult`; `out_df` to a
/// writable pointer slot; `path_ptr`/`path_len` must describe a valid
/// byte range.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_df_read_csv(
    out_io: *mut KaracIoResult,
    out_df: *mut *mut u8,
    path_ptr: *const u8,
    path_len: i64,
) {
    *out_df = ptr::null_mut();
    let Some(path) = read_path(path_ptr, path_len) else {
        *out_io = err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "path is not valid UTF-8",
        ));
        return;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            *out_io = err(&e);
            return;
        }
    };
    let records = match csv_split_records(&text) {
        Ok(r) => r,
        Err(msg) => {
            *out_io = err(&std::io::Error::other(msg));
            return;
        }
    };
    let Some(header) = records.first() else {
        *out_io = err(&std::io::Error::other(
            "CSV parse error: empty file (no header row)",
        ));
        return;
    };
    let names: Vec<String> = header
        .iter()
        .enumerate()
        .map(|(i, c)| c.clone().unwrap_or_else(|| format!("column_{i}")))
        .collect();
    let width = names.len();
    for (i, rec) in records.iter().enumerate().skip(1) {
        if rec.len() != width {
            *out_io = err(&std::io::Error::other(format!(
                "CSV parse error: row {} has {} cell(s) but the header has {}",
                i,
                rec.len(),
                width
            )));
            return;
        }
    }
    let rows = records.len() - 1;

    // Build entries (stride 40: name*, name_len, col_ctrl*, elem_size, kind).
    let entries = df_alloc_zeroed(width * 40);
    for (ci, name) in names.iter().enumerate() {
        let cells: Vec<&Option<String>> = records.iter().skip(1).map(|r| &r[ci]).collect();
        let all_i64 = cells
            .iter()
            .all(|c| c.as_ref().is_none_or(|s| s.parse::<i64>().is_ok()));
        let all_f64 = all_i64
            || cells
                .iter()
                .all(|c| c.as_ref().is_none_or(|s| s.parse::<f64>().is_ok()));
        let (kind, elem_size): (i64, usize) = if all_i64 {
            (1, 8)
        } else if all_f64 {
            (3, 8)
        } else {
            (4, 24)
        };
        // Data buffer + validity bitmap (bit i = valid). Zero-initialized,
        // so NULL slots need no store and String NULLs are `{null,0,0}`.
        let data = df_alloc_zeroed(rows * elem_size);
        let bitmap = df_alloc_zeroed(rows.div_ceil(8));
        for (ri, cell) in cells.iter().enumerate() {
            let Some(s) = cell.as_ref() else { continue };
            *bitmap.add(ri / 8) |= 1 << (ri % 8);
            let p = data.add(ri * elem_size);
            match kind {
                1 => *(p as *mut i64) = s.parse::<i64>().unwrap(),
                3 => *(p as *mut f64) = s.parse::<f64>().unwrap(),
                _ => {
                    let bytes = s.as_bytes();
                    *(p as *mut *mut u8) = df_alloc_bytes(bytes);
                    *(p.add(8) as *mut i64) = bytes.len() as i64;
                    *(p.add(16) as *mut i64) = bytes.len() as i64; // cap == len → owned
                }
            }
        }
        // Column control {data, bitmap, len, cap}.
        let ctrl = df_alloc_zeroed(32);
        *(ctrl as *mut *mut u8) = data;
        *(ctrl.add(8) as *mut *mut u8) = bitmap;
        *(ctrl.add(16) as *mut i64) = rows as i64;
        *(ctrl.add(24) as *mut i64) = rows as i64;
        // Entry.
        let e = entries.add(ci * 40);
        let nbytes = name.as_bytes();
        *(e as *mut *mut u8) = df_alloc_bytes(nbytes);
        *(e.add(8) as *mut i64) = nbytes.len() as i64;
        *(e.add(16) as *mut *mut u8) = ctrl;
        *(e.add(24) as *mut i64) = elem_size as i64;
        *(e.add(32) as *mut i64) = kind;
    }
    // DataFrame control {entries, len, capacity}.
    let control = df_alloc_zeroed(24);
    *(control as *mut *mut u8) = entries;
    *(control.add(8) as *mut i64) = width as i64;
    *(control.add(16) as *mut i64) = width as i64;
    *out_df = control;
    *out_io = ok(0);
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the ABI layout. Codegen will read field offsets directly;
    /// any change to KaracIoResult must update both sides.
    #[test]
    fn test_io_result_layout_pinned() {
        use std::mem::{offset_of, size_of};
        assert_eq!(offset_of!(KaracIoResult, value), 0);
        assert_eq!(offset_of!(KaracIoResult, error_kind), 8);
        assert_eq!(offset_of!(KaracIoResult, _pad), 12);
        assert_eq!(offset_of!(KaracIoResult, error_msg_ptr), 16);
        assert_eq!(offset_of!(KaracIoResult, error_msg_len), 24);
        assert_eq!(size_of::<KaracIoResult>(), 32);
    }

    /// Helper: allocate a fresh `KaracIoResult` slot and yield its
    /// address as the `out` first-arg every test passes to the extern.
    /// Returning the struct by value (vs. through `out`) is what F4's
    /// first cut tripped over — the runtime tests use the same
    /// out-param shape as codegen will, so the tests double as ABI
    /// rehearsal for the F4 call sites.
    fn fresh_result() -> KaracIoResult {
        KaracIoResult {
            value: 0,
            error_kind: 0,
            _pad: 0,
            error_msg_ptr: ptr::null_mut(),
            error_msg_len: 0,
        }
    }

    #[test]
    fn test_create_write_flush_open_read_roundtrip() {
        let tmp = std::env::temp_dir().join("karac_runtime_file_roundtrip.txt");
        let _ = std::fs::remove_file(&tmp);
        let path_str = tmp.to_str().unwrap();
        let path_bytes = path_str.as_bytes();

        unsafe {
            // Create
            let mut res = fresh_result();
            karac_runtime_file_create(&mut res, path_bytes.as_ptr(), path_bytes.len() as i64);
            assert_eq!(res.error_kind, 0);
            let handle = res.value as *mut KaracFile;
            assert!(!handle.is_null());

            // Write "hi\n"
            let data = b"hi\n";
            let mut res = fresh_result();
            karac_runtime_file_write(&mut res, handle, data.as_ptr(), data.len() as i64);
            assert_eq!(res.error_kind, 0);
            assert_eq!(res.value, 3);

            // Flush
            let mut res = fresh_result();
            karac_runtime_file_flush(&mut res, handle);
            assert_eq!(res.error_kind, 0);

            // Close (drops the handle)
            karac_runtime_file_close(handle);

            // Reopen + read
            let mut res = fresh_result();
            karac_runtime_file_open(&mut res, path_bytes.as_ptr(), path_bytes.len() as i64);
            assert_eq!(res.error_kind, 0);
            let handle = res.value as *mut KaracFile;

            let mut buf = [0u8; 8];
            let mut res = fresh_result();
            karac_runtime_file_read(&mut res, handle, buf.as_mut_ptr(), buf.len() as i64);
            assert_eq!(res.error_kind, 0);
            assert_eq!(res.value, 3);
            assert_eq!(&buf[..3], b"hi\n");

            karac_runtime_file_close(handle);
        }

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_open_nonexistent_returns_not_found() {
        let path = b"/nonexistent_karac_runtime_test_F2.txt";
        unsafe {
            let mut res = fresh_result();
            karac_runtime_file_open(&mut res, path.as_ptr(), path.len() as i64);
            assert_eq!(res.error_kind, 1, "expected NotFound tag");
            assert!(res.error_msg_ptr.is_null());
        }
    }

    #[test]
    fn test_close_null_is_safe() {
        unsafe {
            karac_runtime_file_close(ptr::null_mut());
        }
    }

    #[test]
    fn test_append_extends_file() {
        let tmp = std::env::temp_dir().join("karac_runtime_file_append.txt");
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(&tmp, b"first ").expect("seed temp");
        let path_str = tmp.to_str().unwrap();
        let path_bytes = path_str.as_bytes();

        unsafe {
            let mut res = fresh_result();
            karac_runtime_file_append(&mut res, path_bytes.as_ptr(), path_bytes.len() as i64);
            assert_eq!(res.error_kind, 0);
            let handle = res.value as *mut KaracFile;

            let data = b"second";
            let mut res = fresh_result();
            karac_runtime_file_write(&mut res, handle, data.as_ptr(), data.len() as i64);
            assert_eq!(res.error_kind, 0);

            karac_runtime_file_close(handle);
        }

        let contents = std::fs::read(&tmp).expect("read temp");
        assert_eq!(contents, b"first second");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_read_to_string_returns_contents_in_msg_buffer() {
        let tmp = std::env::temp_dir().join("karac_runtime_read_to_string.txt");
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(&tmp, b"hello\nworld\n").expect("seed temp");
        let path_str = tmp.to_str().unwrap();
        let path_bytes = path_str.as_bytes();

        unsafe {
            let mut res = fresh_result();
            karac_runtime_file_read_to_string(
                &mut res,
                path_bytes.as_ptr(),
                path_bytes.len() as i64,
            );
            assert_eq!(res.error_kind, 0, "expected Ok");
            assert!(!res.error_msg_ptr.is_null());
            assert_eq!(res.error_msg_len, 12);
            let slice = std::slice::from_raw_parts(res.error_msg_ptr, res.error_msg_len as usize);
            assert_eq!(slice, b"hello\nworld\n");
            // Free the buffer the way the Kāra String drop path would.
            let layout = Layout::array::<u8>(res.error_msg_len as usize).unwrap();
            std::alloc::dealloc(res.error_msg_ptr, layout);
        }
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_read_to_string_empty_file_is_null_zero() {
        let tmp = std::env::temp_dir().join("karac_runtime_read_to_string_empty.txt");
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(&tmp, b"").expect("seed temp");
        let path_str = tmp.to_str().unwrap();
        let path_bytes = path_str.as_bytes();

        unsafe {
            let mut res = fresh_result();
            karac_runtime_file_read_to_string(
                &mut res,
                path_bytes.as_ptr(),
                path_bytes.len() as i64,
            );
            assert_eq!(res.error_kind, 0);
            assert!(res.error_msg_ptr.is_null());
            assert_eq!(res.error_msg_len, 0);
        }
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_read_to_string_nonexistent_returns_not_found() {
        let path = b"/nonexistent_karac_read_to_string_test.txt";
        unsafe {
            let mut res = fresh_result();
            karac_runtime_file_read_to_string(&mut res, path.as_ptr(), path.len() as i64);
            assert_eq!(res.error_kind, 1, "expected NotFound tag");
            assert!(res.error_msg_ptr.is_null());
        }
    }
}
