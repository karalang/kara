//! File handle runtime shim — Phase 8 stdlib `File` slice F2.
//!
//! Companion to `runtime/src/map.rs`'s shape for opaque-handle stdlib
//! types. Wraps `std::fs::File` behind a stable `extern "C"` ABI so
//! codegen (slice F3 / F4) can dispatch `File.open` / `.read` etc.
//! through extern calls.
//!
//! ## ABI surface
//!
//! Every entry point returns a `KaracIoResult` carrying both the
//! operation's success payload (handle or byte count, depending on
//! call kind) AND the IoError discriminator for the codegen-side
//! Result construction. The kind tag follows the order of the
//! `IoError` enum variants declared in `runtime/stdlib/io.kara`:
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
//! For all other tags `error_msg_ptr` is null and `_len` is zero.
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

/// Open `path` in read-only mode. Codegen emits this for `File.open(p)`.
///
/// # Safety
///
/// `path_ptr` must point to `path_len` valid bytes of UTF-8 text.
/// Codegen always satisfies this via the Kāra String's `{ptr, len}`
/// invariant. The returned `handle` (when `error_kind == 0`) is
/// owned by the caller and must be released through
/// `karac_runtime_file_close`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_file_open(
    path_ptr: *const u8,
    path_len: i64,
) -> KaracIoResult {
    let Some(path) = read_path(path_ptr, path_len) else {
        return err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "path is not valid UTF-8",
        ));
    };
    match std::fs::OpenOptions::new().read(true).open(&path) {
        Ok(f) => {
            let handle = Box::into_raw(Box::new(KaracFile {
                inner: Mutex::new(f),
            }));
            ok(handle as i64)
        }
        Err(e) => err(&e),
    }
}

/// Open `path` in write+truncate mode (creating if absent). Codegen
/// emits this for `File.create(p)`.
///
/// # Safety
///
/// See `karac_runtime_file_open`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_file_create(
    path_ptr: *const u8,
    path_len: i64,
) -> KaracIoResult {
    let Some(path) = read_path(path_ptr, path_len) else {
        return err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "path is not valid UTF-8",
        ));
    };
    match std::fs::OpenOptions::new()
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
    }
}

/// Open `path` in append mode (creating if absent, positioning writes
/// at end-of-file). Codegen emits this for `File.append(p)`.
///
/// # Safety
///
/// See `karac_runtime_file_open`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_file_append(
    path_ptr: *const u8,
    path_len: i64,
) -> KaracIoResult {
    let Some(path) = read_path(path_ptr, path_len) else {
        return err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "path is not valid UTF-8",
        ));
    };
    match std::fs::OpenOptions::new()
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
    }
}

/// Read up to `buf_len` bytes from `handle` into `buf_ptr`. Returns
/// the number of bytes read in `value` on success; zero means clean
/// EOF (not an error).
///
/// # Safety
///
/// `handle` must be a live pointer returned from `_open` / `_create` /
/// `_append` and not yet closed. `buf_ptr` must point to writable
/// memory of at least `buf_len` bytes; `buf_len >= 0`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_file_read(
    handle: *mut KaracFile,
    buf_ptr: *mut u8,
    buf_len: i64,
) -> KaracIoResult {
    if handle.is_null() {
        return err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "null file handle",
        ));
    }
    if buf_len < 0 {
        return err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "negative buffer length",
        ));
    }
    let file = &*handle;
    let mut guard = match file.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let buf = std::slice::from_raw_parts_mut(buf_ptr, buf_len as usize);
    match guard.read(buf) {
        Ok(n) => ok(n as i64),
        Err(e) => err(&e),
    }
}

/// Write up to `buf_len` bytes from `buf_ptr` to `handle`. Returns
/// the number of bytes written in `value`.
///
/// # Safety
///
/// See `karac_runtime_file_read`; `buf_ptr` is read-only here.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_file_write(
    handle: *mut KaracFile,
    buf_ptr: *const u8,
    buf_len: i64,
) -> KaracIoResult {
    if handle.is_null() {
        return err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "null file handle",
        ));
    }
    if buf_len < 0 {
        return err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "negative buffer length",
        ));
    }
    let file = &*handle;
    let mut guard = match file.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let buf = std::slice::from_raw_parts(buf_ptr, buf_len as usize);
    match guard.write(buf) {
        Ok(n) => ok(n as i64),
        Err(e) => err(&e),
    }
}

/// Flush the file's write buffer. Returns OK with `value == 0` on
/// success.
///
/// # Safety
///
/// `handle` must be a live pointer returned from an open-family call.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_file_flush(handle: *mut KaracFile) -> KaracIoResult {
    if handle.is_null() {
        return err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "null file handle",
        ));
    }
    let file = &*handle;
    let mut guard = match file.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    match guard.flush() {
        Ok(()) => ok(0),
        Err(e) => err(&e),
    }
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
/// `whence`: 0 = Start, 1 = Current, 2 = End. Returns the new
/// position in `value` on success.
///
/// # Safety
///
/// `handle` must be a live pointer returned from an open-family call.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_file_seek(
    handle: *mut KaracFile,
    whence: u8,
    offset: i64,
) -> KaracIoResult {
    if handle.is_null() {
        return err(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "null file handle",
        ));
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
            return err(&std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid seek whence",
            ));
        }
    };
    match guard.seek(pos) {
        Ok(p) => ok(p as i64),
        Err(e) => err(&e),
    }
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

    #[test]
    fn test_create_write_flush_open_read_roundtrip() {
        let tmp = std::env::temp_dir().join("karac_runtime_file_roundtrip.txt");
        let _ = std::fs::remove_file(&tmp);
        let path_str = tmp.to_str().unwrap();
        let path_bytes = path_str.as_bytes();

        unsafe {
            // Create
            let res = karac_runtime_file_create(path_bytes.as_ptr(), path_bytes.len() as i64);
            assert_eq!(res.error_kind, 0);
            let handle = res.value as *mut KaracFile;
            assert!(!handle.is_null());

            // Write "hi\n"
            let data = b"hi\n";
            let res = karac_runtime_file_write(handle, data.as_ptr(), data.len() as i64);
            assert_eq!(res.error_kind, 0);
            assert_eq!(res.value, 3);

            // Flush
            let res = karac_runtime_file_flush(handle);
            assert_eq!(res.error_kind, 0);

            // Close (drops the handle)
            karac_runtime_file_close(handle);

            // Reopen + read
            let res = karac_runtime_file_open(path_bytes.as_ptr(), path_bytes.len() as i64);
            assert_eq!(res.error_kind, 0);
            let handle = res.value as *mut KaracFile;

            let mut buf = [0u8; 8];
            let res = karac_runtime_file_read(handle, buf.as_mut_ptr(), buf.len() as i64);
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
            let res = karac_runtime_file_open(path.as_ptr(), path.len() as i64);
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
            let res = karac_runtime_file_append(path_bytes.as_ptr(), path_bytes.len() as i64);
            assert_eq!(res.error_kind, 0);
            let handle = res.value as *mut KaracFile;

            let data = b"second";
            let res = karac_runtime_file_write(handle, data.as_ptr(), data.len() as i64);
            assert_eq!(res.error_kind, 0);

            karac_runtime_file_close(handle);
        }

        let contents = std::fs::read(&tmp).expect("read temp");
        assert_eq!(contents, b"first second");
        let _ = std::fs::remove_file(&tmp);
    }
}
