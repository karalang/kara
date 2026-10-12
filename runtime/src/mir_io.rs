//! The MIR backend's I/O entry points: each wraps the runtime's blocking
//! implementation (`file.rs`, ...) and reports the way `karac_mir_fs` does.
//! A negative return is success, its payload in `*value`; `0..=6` is the
//! failure's `IoError` variant (`NotFound`, `PermissionDenied`,
//! `AlreadyExists`, `UnexpectedEof`, `InvalidUtf8`, `Interrupted`, `Other`),
//! and an `Other` leaves its message as a fresh `String` at `msg`.

use crate::alloc::karac_realloc_or_panic;
use crate::file::{KaracFile, KaracIoResult};
use crate::mir::RawVec;

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

/// The code for a `file.rs` result, its value written to `value`. An
/// `Other` message moves into a `String` at `msg`, and `file.rs`'s copy of
/// it is freed.
///
/// # Safety
/// `value` is writable for an `i64` and `msg` for a `String`.
unsafe fn io_code(r: &KaracIoResult, value: *mut i64, msg: *mut RawVec) -> i32 {
    match r.error_kind {
        0 => {
            // SAFETY: the caller's contract.
            unsafe { value.write(r.value) };
            -1
        }
        7 => {
            let len = usize::try_from(r.error_msg_len).unwrap_or(0);
            if r.error_msg_ptr.is_null() || len == 0 {
                // SAFETY: the caller's contract.
                unsafe { string_of(msg, b"") };
            } else {
                // SAFETY: `file.rs` allocated `len` bytes at the pointer as
                // a `u8` array, and nothing else holds it.
                unsafe {
                    string_of(msg, std::slice::from_raw_parts(r.error_msg_ptr, len));
                    std::alloc::dealloc(
                        r.error_msg_ptr,
                        std::alloc::Layout::array::<u8>(len).expect("io error msg layout"),
                    );
                }
            }
            6
        }
        k => k - 1,
    }
}

/// `File.open` / `create` / `append` (op 0-2, the path at `ptr[..len]`),
/// `read` / `write` (3-4, the buffer at `ptr[..len]`), `flush`, `sync_all`,
/// `sync_data` (5-7) and `seek` (8, `n` the whence and `len` the offset),
/// on the handle `handle` for every op past `append`.
///
/// # Safety
/// `ptr` is valid for `len` bytes (written by `read`); `handle` is a live
/// file handle for ops 3-8; `value` and `msg` as [`io_code`] has them.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_file(
    op: i32,
    handle: *mut KaracFile,
    ptr: *mut u8,
    len: i64,
    n: i64,
    value: *mut i64,
    msg: *mut RawVec,
) -> i32 {
    use crate::file as f;
    let mut r = KaracIoResult {
        value: 0,
        error_kind: 0,
        _pad: 0,
        error_msg_ptr: std::ptr::null_mut(),
        error_msg_len: 0,
    };
    if op == 8 && n == 0 && len < 0 {
        // `SeekFrom.Start` of a negative offset, refused before the OS
        // sees it, with the interpreter's message rather than the
        // platform's (`file.rs` hands it on as a huge unsigned position).
        // SAFETY: the caller's contract.
        unsafe { string_of(msg, b"invalid seek to a negative position") };
        return 6;
    }
    // SAFETY: the caller's contract, which is each callee's.
    unsafe {
        match op {
            0 => f::karac_runtime_file_open(&mut r, ptr, len),
            1 => f::karac_runtime_file_create(&mut r, ptr, len),
            2 => f::karac_runtime_file_append(&mut r, ptr, len),
            3 => f::karac_runtime_file_read(&mut r, handle, ptr, len),
            4 => f::karac_runtime_file_write(&mut r, handle, ptr, len),
            5 => f::karac_runtime_file_flush(&mut r, handle),
            6 => f::karac_runtime_file_sync_all(&mut r, handle),
            7 => f::karac_runtime_file_sync_data(&mut r, handle),
            _ => f::karac_runtime_file_seek(&mut r, handle, n as u8, len),
        }
        io_code(&r, value, msg)
    }
}
