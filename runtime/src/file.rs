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
