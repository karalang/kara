//! The MIR backend's I/O entry points: each wraps the runtime's blocking
//! implementation (`file.rs`, ...) and reports the way `karac_mir_fs` does.
//! A negative return is success, its payload in `*value`; `0..=6` is the
//! failure's `IoError` variant (`NotFound`, `PermissionDenied`,
//! `AlreadyExists`, `UnexpectedEof`, `InvalidUtf8`, `Interrupted`, `Other`),
//! and an `Other` leaves its message as a fresh `String` at `msg`.

use crate::alloc::karac_realloc_or_panic;
use crate::file::{KaracFile, KaracIoResult};
use crate::mir::RawVec;

#[cfg(feature = "tls")]
extern "C" {
    fn free(p: *mut std::ffi::c_void);
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

/// The code for a `file.rs` result, its value written to `value`. An
/// `Other` message moves into a `String` at `msg`, and `file.rs`'s copy of
/// it is freed.
///
/// # Safety
/// `value` is writable for an `i64` and `msg` for a `String`.
unsafe fn io_code(r: &KaracIoResult, value: *mut i64, msg: *mut RawVec) -> i32 {
    match r.error_kind {
        // A `String` payload rides the message fields (`ok_string`).
        0 if !r.error_msg_ptr.is_null() => {
            let len = usize::try_from(r.error_msg_len).unwrap_or(0);
            // SAFETY: as for an `Other` message below.
            unsafe {
                string_of(msg, std::slice::from_raw_parts(r.error_msg_ptr, len));
                std::alloc::dealloc(
                    r.error_msg_ptr,
                    std::alloc::Layout::array::<u8>(len).expect("io payload layout"),
                );
            }
            -1
        }
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

/// An empty result, for a callee to fill.
fn blank() -> KaracIoResult {
    KaracIoResult {
        value: 0,
        error_kind: 0,
        _pad: 0,
        error_msg_ptr: std::ptr::null_mut(),
        error_msg_len: 0,
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
    let mut r = blank();
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

/// `Command.spawn`: the program, its arguments (`args_len` `String`s at
/// `args`), its environment (`env_len` `EnvVar`s, a key and a value
/// `String` each, at `env`) and the `Stdio` of each stream (0 inherit, 1
/// null, 2 piped). The child's pid lands in `*value`.
///
/// # Safety
/// Each pointer is valid for what it holds; `value` and `msg` as
/// [`io_code`] has them.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_process_spawn(
    prog: *const RawVec,
    args: *const RawVec,
    args_len: i64,
    env: *const u8,
    env_len: i64,
    stdin: i64,
    stdout: i64,
    stderr: i64,
    value: *mut i64,
    msg: *mut RawVec,
) -> i32 {
    let mut r = blank();
    // SAFETY: a MIR `String` is the runtime's `{ptr, len, cap}` descriptor,
    // and an `EnvVar` two of them; the rest is the caller's contract.
    unsafe {
        crate::process::karac_runtime_process_spawn(
            &mut r,
            prog.cast(),
            args.cast(),
            args_len,
            env.cast(),
            env_len,
            stdin,
            stdout,
            stderr,
        );
        io_code(&r, value, msg)
    }
}

/// A `Child`'s `wait` (op 0) and `try_wait` (1), their status packed into
/// `*value` as `process.rs` packs it, and `kill` (2); a `ChildStdout` or
/// `ChildStderr`'s `read_to_string` (3, 4), the text a `String` at `msg`;
/// a `ChildStdin`'s `write` of the `String` at `data` (5) and `close` (6).
///
/// # Safety
/// `data` is a `String` for op 5; `value` and `msg` as [`io_code`] has them.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_process(
    op: i32,
    pid: i64,
    data: *const RawVec,
    value: *mut i64,
    msg: *mut RawVec,
) -> i32 {
    use crate::process as p;
    let mut r = blank();
    // SAFETY: the caller's contract, which is each callee's.
    unsafe {
        if op == 3 || op == 4 {
            // An empty read comes back without a buffer.
            string_of(msg, b"");
        }
        match op {
            0 => p::karac_runtime_process_wait(&mut r, pid),
            1 => p::karac_runtime_process_try_wait(&mut r, pid),
            2 => p::karac_runtime_process_kill(&mut r, pid),
            3 | 4 => p::karac_runtime_process_read_to_string(&mut r, pid, i64::from(op - 3)),
            5 => p::karac_runtime_process_stdin_write(&mut r, pid, data.cast()),
            _ => p::karac_runtime_process_stdin_close(pid),
        }
        io_code(&r, value, msg)
    }
}

/// `TcpListener` / `TcpStream` over the runtime's blocking socket calls
/// (`event_loop.rs`): `op` 0 `bind(ptr, len)`, 1 `connect(ptr, len)`,
/// 2 `accept(fd)`, 3 `try_clone(fd)`, 4 `shutdown_write(fd)`, 5 `read(fd,
/// ptr, len)`, 6 `write(fd, ptr, len)` and 7 `write_all(fd, ptr, len)`.
/// A negative return is success, with the descriptor or byte count in
/// `value`; otherwise it is the `TcpError` variant, 0 `Interrupted`, 1
/// `Other` (its `i32` in `value`), 2 `AddrInUse`, 3 `ConnectionRefused`
/// and 4 `PermissionDenied`, as the MIR interpreter's `tcp_method` has them.
///
/// # Safety
///
/// `ptr` and `len` describe the address or buffer the op reads or writes,
/// and `value` is writable.
#[cfg(feature = "net")]
#[no_mangle]
pub unsafe extern "C" fn karac_mir_tcp(
    op: i32,
    fd: i64,
    ptr: *mut u8,
    len: i64,
    value: *mut i64,
) -> i32 {
    use crate::event_loop as ev;
    // A construction result: the descriptor, or the runtime's cause code.
    let built = |r: i64| match r {
        r if r >= 0 => (-1, r),
        -2 => (2, 0),
        -3 => (3, 0),
        -4 => (4, 0),
        r => (1, r),
    };
    // A `read` / `write` result: the count, or `-errno`.
    let moved = |n: i64| match n {
        n if n >= 0 => (-1, n),
        -4 => (0, 0),
        n => (1, -n),
    };
    // SAFETY: the caller's contract, which is each callee's.
    let (code, v) = unsafe {
        match op {
            0 => built(ev::karac_runtime_tcp_bind(ptr, len)),
            1 => built(ev::karac_runtime_tcp_connect(ptr, len)),
            2 => built(ev::karac_runtime_tcp_accept(fd)),
            3 => match ev::karac_runtime_tcp_try_clone(fd) {
                r if r >= 0 => (-1, r),
                _ => (1, -1),
            },
            4 => match ev::karac_runtime_tcp_shutdown(fd, 1) {
                0 => (-1, 0),
                _ => (1, -1),
            },
            5 => moved(ev::karac_runtime_tcp_read(fd, ptr, len)),
            6 => moved(ev::karac_runtime_tcp_write(fd, ptr, len)),
            _ => {
                // Partial writes go round again, and so does EINTR.
                let mut done = 0;
                loop {
                    if done >= len {
                        break (-1, done);
                    }
                    match ev::karac_runtime_tcp_write(fd, ptr.add(done as usize), len - done) {
                        0 => break (-1, done),
                        n if n > 0 => done += n,
                        -4 => {}
                        n => break (1, -n),
                    }
                }
            }
        }
    };
    // SAFETY: as above.
    unsafe { *value = v };
    code
}

/// The headers of each `Response` the MIR backend's client made, by the
/// address of its body's buffer: a MIR `Response` is its status and body
/// only, so the runtime's header handle cannot ride in it. Every body gets
/// a buffer of its own (an empty one is given a byte of capacity), so the
/// address names one live response. Entries stay until exit, as the
/// runtime's own header table does.
#[cfg(feature = "tls")]
static MIR_RESPONSE_HEADERS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<usize, i64>>,
> = std::sync::LazyLock::new(Default::default);

/// `Client.get` (`op` 0, `a` the URL), `Client.post` (1, `a` the URL and
/// `b` the body) and `RequestBuilder.send` (2, `handle`). Returns 1 with
/// the status at `status` and the body as a `String` at `out`, or 0 with
/// the error's message as a `String` at `out`.
///
/// # Safety
///
/// `a` / `b` describe readable bytes (or are null with a zero length),
/// `status` is writable for an `i64` and `out` for a `String`.
#[cfg(feature = "tls")]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_client_mir_send(
    op: i32,
    a: *const u8,
    a_len: usize,
    b: *const u8,
    b_len: usize,
    handle: i64,
    status: *mut i64,
    out: *mut RawVec,
) -> i32 {
    let mut st = 0i64;
    let (mut body, mut body_len) = (std::ptr::null_mut::<u8>(), 0i64);
    let (mut msg, mut msg_len) = (std::ptr::null_mut::<u8>(), 0i64);
    let mut headers = 0i64;
    // SAFETY: the caller's contract, which is each callee's; the out
    // params are locals.
    unsafe {
        match op {
            0 => crate::karac_runtime_http_client_get(
                a,
                a_len,
                &mut st,
                &mut body,
                &mut body_len,
                &mut msg,
                &mut msg_len,
                &mut headers,
            ),
            1 => crate::karac_runtime_http_client_post(
                a,
                a_len,
                b,
                b_len,
                &mut st,
                &mut body,
                &mut body_len,
                &mut msg,
                &mut msg_len,
                &mut headers,
            ),
            _ => crate::karac_runtime_http_builder_send(
                handle,
                &mut st,
                &mut body,
                &mut body_len,
                &mut msg,
                &mut msg_len,
                &mut headers,
            ),
        }
    }
    // Moves a `malloc`'d buffer from the client into a `String`.
    let take = |p: *mut u8, n: i64| -> RawVec {
        let n = usize::try_from(n).unwrap_or(0);
        let cap = n.max(1);
        let q = karac_realloc_or_panic(std::ptr::null_mut(), cap);
        if !p.is_null() {
            // SAFETY: the client wrote `n` bytes at `p` and gave it up.
            unsafe {
                std::ptr::copy_nonoverlapping(p, q, n);
                free(p.cast());
            }
        }
        RawVec {
            ptr: q,
            len: n,
            cap,
        }
    };
    let ok = st > 0;
    let s = if ok {
        take(body, body_len)
    } else {
        take(msg, msg_len)
    };
    if ok {
        if let Ok(mut t) = MIR_RESPONSE_HEADERS.lock() {
            t.insert(s.ptr as usize, headers);
        }
    }
    // SAFETY: the caller's contract.
    unsafe {
        status.write(st);
        out.write(s);
    }
    i32::from(ok)
}

/// The runtime's header handle for the `Response` whose body's buffer is
/// at `body`, or 0.
#[cfg(feature = "tls")]
fn mir_response_headers(body: *const u8) -> i64 {
    MIR_RESPONSE_HEADERS
        .lock()
        .ok()
        .and_then(|t| t.get(&(body as usize)).copied())
        .unwrap_or(0)
}

/// `Response.header(name)` for the response whose body's buffer is at
/// `body`: 1 with the value as a `String` at `out`, or 0 when it has no
/// such header (names compare without case).
///
/// # Safety
///
/// `name` describes `name_len` readable bytes and `out` is writable for a
/// `String`.
#[cfg(feature = "tls")]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_client_mir_header(
    body: *const u8,
    name: *const u8,
    name_len: usize,
    out: *mut RawVec,
) -> i32 {
    let h = mir_response_headers(body);
    // SAFETY: the caller's contract; a hit is a NUL-terminated string the
    // header table keeps until exit.
    unsafe {
        let v = crate::karac_runtime_http_response_header(h, name, name_len);
        if v.is_null() {
            return 0;
        }
        string_of(out, std::ffi::CStr::from_ptr(v).to_bytes());
    }
    1
}

/// `Response.headers()` for the response whose body's buffer is at `body`:
/// a fresh `Vec[(String, String)]` at `out`, in the order they came.
///
/// # Safety
///
/// `out` is writable for a `Vec`.
#[cfg(feature = "tls")]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_http_client_mir_headers(body: *const u8, out: *mut RawVec) {
    let h = mir_response_headers(body);
    let n = usize::try_from(crate::karac_runtime_http_response_headers_count(h)).unwrap_or(0);
    let pair = 2 * std::mem::size_of::<RawVec>();
    let items = if n == 0 {
        std::ptr::null_mut()
    } else {
        karac_realloc_or_panic(std::ptr::null_mut(), n * pair)
    };
    for i in 0..n {
        // SAFETY: `items` has room for `n` pairs; each accessor's hit is a
        // NUL-terminated string the header table keeps until exit.
        unsafe {
            let at = items.add(i * pair).cast::<RawVec>();
            for (k, key) in [true, false].into_iter().enumerate() {
                let p = if key {
                    crate::karac_runtime_http_response_header_key_at(h, i as i64)
                } else {
                    crate::karac_runtime_http_response_header_val_at(h, i as i64)
                };
                let b = if p.is_null() {
                    &[][..]
                } else {
                    std::ffi::CStr::from_ptr(p).to_bytes()
                };
                string_of(at.add(k), b);
            }
        }
    }
    // SAFETY: the caller's contract.
    unsafe {
        out.write(RawVec {
            ptr: items,
            len: n,
            cap: n,
        })
    };
}
