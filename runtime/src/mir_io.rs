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
pub(crate) unsafe fn string_of(out: *mut RawVec, b: &[u8]) {
    let ptr = if b.is_empty() {
        std::ptr::null_mut()
    } else {
        let p = karac_realloc_or_panic(std::ptr::null_mut(), b.len());
        // SAFETY: `p` has room for `b.len()` bytes.
        unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), p, b.len()) };
        p
    };
    // SAFETY: the caller's contract.
    unsafe { out.write(RawVec::new(ptr, b.len(), b.len())) };
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
        7 if r._pad == 1 || r._pad == 2 => {
            // `InvalidInput` / `Unsupported`, which `file::err` folds into
            // `Other` for the legacy backends: no message.
            let len = usize::try_from(r.error_msg_len).unwrap_or(0);
            if !r.error_msg_ptr.is_null() && len > 0 {
                // SAFETY: as for an `Other` message below.
                unsafe {
                    std::alloc::dealloc(
                        r.error_msg_ptr,
                        std::alloc::Layout::array::<u8>(len).expect("io error msg layout"),
                    );
                }
            }
            6 + r._pad
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
/// `sync_data` (5-7), `seek` (8, `n` the whence and `len` the offset) and
/// `set_len` (9, `len` the length), on the handle `handle` for every op
/// past `append`; `OpenOptions.open` (10, `n` the options' bits, the path at
/// `ptr[..len]`).
///
/// # Safety
/// `ptr` is valid for `len` bytes (written by `read`); `handle` is a live
/// file handle for ops 3-9; `value` and `msg` as [`io_code`] has them.
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
        // sees it, as the interpreter refuses it (`file.rs` hands it on
        // as a huge unsigned position): `InvalidInput`.
        return 7;
    }
    if op == 9 || op == 10 {
        // `set_len` (B-2026-10-15-28): `len` the new length, which is
        // `InvalidInput` when negative, on the file at `handle`.
        // `OpenOptions.open`: `n` the options' bits, the path at
        // `ptr[..len]`.
        let res = if op == 9 {
            if len < 0 {
                return 7;
            }
            // SAFETY: the caller's contract: a live handle.
            let file = unsafe { &*handle };
            file.with(|f| f.set_len(len as u64)).map(|()| 0)
        } else {
            // SAFETY: the caller's contract.
            let path = String::from_utf8_lossy(unsafe {
                std::slice::from_raw_parts(ptr, usize::try_from(len).unwrap_or(0))
            })
            .into_owned();
            open_options(n)
                .and_then(|o| o.open(path))
                .map(|f| Box::into_raw(Box::new(KaracFile::new(f))) as i64)
        };
        return match res {
            Ok(v) => {
                // SAFETY: the caller's contract.
                unsafe { value.write(v) };
                -1
            }
            // SAFETY: the caller's contract.
            Err(e) => unsafe { io_error_code(&e, msg) },
        };
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

/// The `std::fs::OpenOptions` for `OpenOptions.open`'s bits, or
/// `InvalidInput` for a combination that opens nothing. The interpreter's
/// `mir::fs::open_options` is the same function (B-2026-10-15-28).
fn open_options(flags: i64) -> std::io::Result<std::fs::OpenOptions> {
    let bit = |b: i64| flags & b != 0;
    let (read, write, append) = (bit(1), bit(2), bit(4));
    let (truncate, create, create_new) = (bit(8), bit(16), bit(32));
    let writes = write || append;
    if !(read || writes)
        || ((truncate || create || create_new) && !writes)
        || (create_new && truncate)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "these options open nothing",
        ));
    }
    let mut o = std::fs::OpenOptions::new();
    o.read(read)
        .write(write)
        .append(append)
        .truncate(truncate)
        .create(create)
        .create_new(create_new);
    Ok(o)
}

/// A `BufReader` or `BufWriter` over the `File` or the child's pipe its
/// constructor took.
pub enum KaracBuf {
    Reader(std::io::BufReader<Box<dyn std::io::Read>>),
    Writer(std::io::BufWriter<Box<dyn std::io::Write>>),
}

/// Appends `b` to the `String` at `s`, growing its buffer into a fresh
/// allocation when it is full (a `String`'s bytes can be a constant's,
/// which no `realloc` may touch).
///
/// # Safety
/// `s` is a live `String`.
pub(crate) unsafe fn push_bytes(s: *mut RawVec, b: &[u8]) {
    if b.is_empty() {
        return;
    }
    // SAFETY: the caller's contract.
    let s = unsafe { &mut *s };
    if s.cap - s.len < b.len() {
        let cap = (s.len + b.len()).max(s.cap * 2);
        let p = karac_realloc_or_panic(std::ptr::null_mut(), cap);
        if s.len > 0 {
            // SAFETY: the old buffer holds `len` bytes; `p` has room.
            unsafe { std::ptr::copy_nonoverlapping(s.ptr, p, s.len) };
        }
        if s.cap != 0 {
            extern "C" {
                fn free(ptr: *mut std::ffi::c_void);
            }
            // SAFETY: a `String` with capacity owns its malloc'd buffer.
            unsafe { free(s.ptr.cast()) };
        }
        s.ptr = p;
        s.cap = cap;
    }
    // SAFETY: room for `b.len()` more bytes past `len`.
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), s.ptr.add(s.len), b.len()) };
    s.len += b.len();
}

/// The code for a failed `std::io` call, as [`io_code`] reports one.
///
/// # Safety
/// `msg` is writable for a `String`.
pub(crate) unsafe fn io_error_code(e: &std::io::Error, msg: *mut RawVec) -> i32 {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::NotFound => 0,
        K::PermissionDenied => 1,
        K::AlreadyExists => 2,
        K::UnexpectedEof => 3,
        K::InvalidData => 4,
        K::Interrupted => 5,
        K::InvalidInput => 7,
        K::Unsupported => 8,
        _ => {
            // SAFETY: the caller's contract.
            unsafe { string_of(msg, e.to_string().as_bytes()) };
            6
        }
    }
}

/// `BufReader.new` / `with_capacity` (op 0) and `BufWriter.new` /
/// `with_capacity` (op 1) over the `File` at `handle`, which they take
/// (`len` the capacity, `<= 0` for the default), and the two over a child's
/// pipe (ops 10 and 11, B-2026-10-15-44); then, on the buffered
/// handle at `handle`: `read_line` (2) and `read_to_string` (3), appending
/// to the `String` at `ptr`; `read` (4) into `ptr[..len]`; a `LinesIter`'s
/// `next` (5), the line without its `\n` / `\r\n` as a fresh `String` at
/// `msg`, or `-2` at the end; `write` (6) and `write_all` (7) of
/// `ptr[..len]`; `flush` (8); `consume` (9) of `len` bytes; `fill_buf`
/// (12), a view of the reader's buffer as a pointer and a length at
/// `value[0..2]` (B-2026-10-14-72), which stays valid until the next call
/// on the handle.
///
/// # Safety
/// `handle` is a live file handle for ops 0-1 and a live buffered handle
/// otherwise; `ptr` holds what its op reads or writes; `value` and `msg` as
/// [`io_code`] has them, with `value` writable for two `i64`s for op 12.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_buf(
    op: i32,
    handle: *mut std::ffi::c_void,
    ptr: *mut u8,
    len: i64,
    value: *mut i64,
    msg: *mut RawVec,
) -> i32 {
    use std::io::{BufRead, Read, Write};
    if op <= 1 || op == 10 || op == 11 {
        let cap = usize::try_from(len).ok().filter(|&n| n > 0).unwrap_or(8192);
        let buf = if op >= 10 {
            // SAFETY: a live pipe from `karac_mir_child`, which this takes.
            match unsafe { crate::mir_process::take_pipe(handle) } {
                Ok(r) => KaracBuf::Reader(std::io::BufReader::with_capacity(cap, r)),
                Err(w) => KaracBuf::Writer(std::io::BufWriter::with_capacity(cap, w)),
            }
        } else {
            // SAFETY: a live handle from `karac_runtime_file_open` & co.,
            // which this takes.
            let file = unsafe { crate::file::take_file(handle.cast()) };
            if op == 0 {
                KaracBuf::Reader(std::io::BufReader::with_capacity(cap, Box::new(file)))
            } else {
                KaracBuf::Writer(std::io::BufWriter::with_capacity(cap, Box::new(file)))
            }
        };
        // SAFETY: the caller's contract.
        unsafe { value.write(Box::into_raw(Box::new(buf)) as i64) };
        return -1;
    }
    // SAFETY: the caller's contract.
    let buf = unsafe { &mut *handle.cast::<KaracBuf>() };
    let n = usize::try_from(len).unwrap_or(0);
    let res: std::io::Result<i64> = match (op, buf) {
        (2 | 3, KaracBuf::Reader(r)) => {
            let mut s = Vec::new();
            let got = if op == 2 {
                r.read_until(b'\n', &mut s)
            } else {
                r.read_to_end(&mut s)
            };
            got.and_then(|k| {
                std::str::from_utf8(&s)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                // SAFETY: the caller's contract.
                unsafe { push_bytes(ptr.cast(), &s) };
                Ok(k as i64)
            })
        }
        (4, KaracBuf::Reader(r)) if ptr.is_null() || n == 0 => r.read(&mut []).map(|k| k as i64),
        (4, KaracBuf::Reader(r)) => {
            // SAFETY: the caller's contract.
            let out = unsafe { std::slice::from_raw_parts_mut(ptr, n) };
            r.read(out).map(|k| k as i64)
        }
        (5, KaracBuf::Reader(r)) => {
            let mut s = Vec::new();
            match r.read_until(b'\n', &mut s) {
                Ok(0) => return -2,
                Ok(_) => {
                    if s.last() == Some(&b'\n') {
                        s.pop();
                        if s.last() == Some(&b'\r') {
                            s.pop();
                        }
                    }
                    match std::str::from_utf8(&s) {
                        Ok(_) => {
                            // SAFETY: the caller's contract.
                            unsafe { string_of(msg, &s) };
                            return -1;
                        }
                        Err(e) => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                    }
                }
                Err(e) => Err(e),
            }
        }
        (6, KaracBuf::Writer(w)) => {
            // SAFETY: the caller's contract.
            w.write(unsafe { buf_bytes(ptr, n) }).map(|k| k as i64)
        }
        (7, KaracBuf::Writer(w)) => {
            // SAFETY: the caller's contract.
            w.write_all(unsafe { buf_bytes(ptr, n) }).map(|()| 0)
        }
        (8, KaracBuf::Writer(w)) => w.flush().map(|()| 0),
        (9, KaracBuf::Reader(r)) => {
            r.consume(n);
            Ok(0)
        }
        (12, KaracBuf::Reader(r)) => match r.fill_buf() {
            Ok(b) => {
                // SAFETY: the caller's contract.
                unsafe {
                    value.write(b.as_ptr() as i64);
                    value.add(1).write(b.len() as i64);
                }
                return -1;
            }
            Err(e) => Err(e),
        },
        _ => Err(std::io::Error::other("a buffered handle of the other kind")),
    };
    match res {
        Ok(v) => {
            // SAFETY: the caller's contract.
            unsafe { value.write(v) };
            -1
        }
        // SAFETY: the caller's contract.
        Err(e) => unsafe { io_error_code(&e, msg) },
    }
}

/// The standard streams: `Stdin.read_line` (op 0, the line with its `\n`)
/// and `read_to_string` (1), each a fresh `String` at `msg`; a
/// `StdinLines`'s `next` (2), the line without its `\n` / `\r\n`, or `-2`
/// at the end; `Stdout.flush` (3) and `Stderr.flush` (4). Text that is not
/// UTF-8 is an `InvalidUtf8` (docs/library/io.md § Standard streams).
///
/// # Safety
/// `value` and `msg` as [`io_code`] has them.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_std_stream(op: i32, value: *mut i64, msg: *mut RawVec) -> i32 {
    use std::io::{BufRead, Read, Write};
    let mut s = Vec::new();
    let got = match op {
        0 | 2 => std::io::stdin().lock().read_until(b'\n', &mut s),
        1 => std::io::stdin().lock().read_to_end(&mut s),
        3 => {
            crate::mir::out_flush();
            std::io::stdout().flush().map(|()| 0)
        }
        _ => std::io::stderr().flush().map(|()| 0),
    };
    let res = got.and_then(|n| {
        if op == 2 {
            if n == 0 {
                return Ok(None);
            }
            if s.last() == Some(&b'\n') {
                s.pop();
                if s.last() == Some(&b'\r') {
                    s.pop();
                }
            }
        }
        std::str::from_utf8(&s)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Some(()))
    });
    match res {
        Ok(None) => -2,
        Ok(Some(())) => {
            // SAFETY: the caller's contract.
            unsafe {
                if op <= 2 {
                    string_of(msg, &s);
                }
                value.write(0);
            }
            -1
        }
        // SAFETY: the caller's contract.
        Err(e) => unsafe { io_error_code(&e, msg) },
    }
}

/// `Env.var(name)`: 0 with the variable's value as a fresh `String` at
/// `out`, 1 when it is unset, 2 when its value is not UTF-8
/// (`VarError.NotPresent` / `NotUnicode`, docs/library/time-random-env.md).
///
/// # Safety
/// `p` is readable for `n` bytes; `out` is writable for a `String`.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_env_var(out: *mut RawVec, p: *const u8, n: u64) -> i32 {
    let n = n as usize;
    let name = if n == 0 {
        &[][..]
    } else {
        // SAFETY: the caller's contract.
        unsafe { std::slice::from_raw_parts(p, n) }
    };
    let Ok(name) = std::str::from_utf8(name) else {
        return 1;
    };
    match std::env::var(name) {
        Ok(v) => {
            // SAFETY: the caller's contract.
            unsafe { string_of(out, v.as_bytes()) };
            0
        }
        Err(std::env::VarError::NotUnicode(_)) => 2,
        Err(std::env::VarError::NotPresent) => 1,
    }
}

/// Drops a `BufReader` or `BufWriter`, closing its file; a writer flushes
/// first and ignores a failure, as io.md has it.
///
/// # Safety
/// `handle` came from [`karac_mir_buf`] and is dropped once.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_buf_free(handle: *mut std::ffi::c_void) {
    if !handle.is_null() {
        // SAFETY: the caller's contract.
        drop(unsafe { Box::from_raw(handle.cast::<KaracBuf>()) });
    }
}

/// The `len` bytes at `p`, or none when `p` is null.
///
/// # Safety
/// `p` is null or describes `len` readable bytes.
unsafe fn buf_bytes<'a>(p: *const u8, len: usize) -> &'a [u8] {
    if p.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: the caller's contract.
        unsafe { std::slice::from_raw_parts(p, len) }
    }
}

/// `TcpListener` / `TcpStream` over the runtime's blocking socket calls
/// (`event_loop.rs`): `op` 0 `bind(ptr, len)`, 1 `connect(ptr, len)`,
/// 2 `accept(fd)`, 3 `try_clone(fd)`, 4 `shutdown_write(fd)`, 5 `read(fd,
/// ptr, len)`, 6 `write(fd, ptr, len)` and 7 `write_all(fd, ptr, len)`.
/// A negative return is success, with the descriptor or byte count in
/// `value`; otherwise it is the `NetError` variant, 0 `Interrupted`, 1
/// `Other` (its `i32` in `value`), 2 `AddrInUse`, 3 `ConnectionRefused`,
/// 4 `PermissionDenied`, 5 `TimedOut` (a read past the timeout
/// `set_read_timeout` set) and 7 `InvalidInput` (an address that does not
/// parse), as the MIR interpreter's `tcp_method` has them.
///
/// # Safety
///
/// A test-harness hook, off unless `KARAC_ANNOUNCE_PORT` is set: a listener
/// bound to port 0 reports `BOUND_PORT=<n>` on stderr, so a driver that
/// cannot ask `local_addr` learns where to connect. A program's own output
/// never carries it (B-2026-10-15-69). The MIR interpreter does the same.
///
/// # Safety
/// `ptr`/`len` describe the address `karac_mir_tcp` was given.
#[cfg(all(feature = "net", unix))]
unsafe fn announce_port(ptr: *mut u8, len: i64, fd: i64) {
    if std::env::var_os("KARAC_ANNOUNCE_PORT").is_none() {
        return;
    }
    // SAFETY: the caller's contract.
    let requested = unsafe { std::str::from_utf8(buf_bytes(ptr, len.max(0) as usize)) }
        .ok()
        .and_then(|t| t.parse::<std::net::SocketAddr>().ok());
    if requested.is_some_and(|a| a.port() == 0) {
        // SAFETY: `fd` is the listener the bind just made.
        if let Ok(local) = unsafe { borrowed::<std::net::TcpListener>(fd) }.local_addr() {
            eprintln!("BOUND_PORT={}", local.port());
        }
    }
}

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
    #[cfg(unix)]
    {
        // SAFETY: the caller's contract.
        if let Some(code) = unsafe { tcp_unix(op, fd, ptr, len, value) } {
            return code;
        }
    }
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
        n if timed_out(-n) => (5, 0),
        n => (1, -n),
    };
    // An address that does not parse is refused here, before the runtime
    // folds it into its other failures.
    // SAFETY: the caller's contract.
    let parses = || unsafe {
        std::str::from_utf8(buf_bytes(ptr, len.max(0) as usize))
            .is_ok_and(|t| t.parse::<std::net::SocketAddr>().is_ok())
    };
    // SAFETY: the caller's contract, which is each callee's.
    let (code, v) = unsafe {
        match op {
            0 | 1 if !parses() => (7, 0),
            0 => {
                let bound = built(ev::tcp_bind(ptr, len, false));
                #[cfg(unix)]
                if bound.0 == -1 {
                    announce_port(ptr, len, bound.1);
                    if let Err(e) = crate::mir_sock::set_nonblocking(bound.1 as i32) {
                        crate::mir_sock::close(bound.1 as i32);
                        return net_cause(&e).0;
                    }
                }
                bound
            }
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

/// [`karac_mir_tcp`]'s calls that can wait, on unix, under the reactor
/// contract (`mir_sock.rs`): `connect` (1), `accept` (2), `read` (5),
/// `write` (6), `write_all` (7) and `connect_finish` (8, `fd` the socket
/// a `connect` answered `IO_IN_PROGRESS` with). An armed call that would
/// block answers `IO_NOT_READY`; `None` leaves the op to the caller.
///
/// # Safety
/// As [`karac_mir_tcp`].
#[cfg(all(feature = "net", unix))]
unsafe fn tcp_unix(op: i32, fd: i64, ptr: *mut u8, len: i64, value: *mut i64) -> Option<i32> {
    use crate::mir_sock::{self as sock, Got};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::os::fd::IntoRawFd;
    // Taken on every path, so a stale bit never reaches a later call.
    let armed = sock::take_armed();
    if !matches!(op, 1 | 2 | 5..=8) {
        return None;
    }
    let raw = fd as i32;
    // SAFETY: the caller's contract.
    let buf = || unsafe {
        if ptr.is_null() || len <= 0 {
            &mut [][..]
        } else {
            std::slice::from_raw_parts_mut(ptr, len as usize)
        }
    };
    let mut infl = -1;
    let got = match op {
        // The retry of a connect the running task has under way ends it,
        // once the readiness loop saw its socket writable.
        1 => match crate::mir_task::take_inflight() {
            Some(c) => {
                if !armed {
                    sock::karac_mir_io_wait(i64::from(c), 1, -1);
                }
                match sock::connect_finish(c) {
                    Got::Done(c) => (-1, c),
                    Got::NotReady => {
                        crate::mir_task::set_inflight(c);
                        (sock::IO_NOT_READY, 0)
                    }
                    Got::Fail(e) => net_cause(&e),
                }
            }
            None => {
                // SAFETY: the caller's contract.
                let text = unsafe { std::str::from_utf8(buf_bytes(ptr, len.max(0) as usize)) };
                match text
                    .ok()
                    .and_then(|t| t.parse::<std::net::SocketAddr>().ok())
                {
                    None => (7, 0),
                    Some(at) => match sock::connect(at, armed, &mut infl) {
                        Got::Done(c) => (-1, c),
                        Got::NotReady => {
                            crate::mir_task::set_inflight(infl as i32);
                            (sock::IO_IN_PROGRESS, infl)
                        }
                        Got::Fail(e) => net_cause(&e),
                    },
                }
            }
        },
        8 => match sock::connect_finish(raw) {
            Got::Done(c) => (-1, c),
            Got::NotReady => (sock::IO_NOT_READY, 0),
            Got::Fail(e) => net_cause(&e),
        },
        _ => {
            let got = match op {
                // SAFETY: `fd` is a listener (the caller's contract).
                2 => sock::drive(raw, false, armed, || unsafe {
                    let (c, _) = borrowed::<TcpListener>(fd).accept()?;
                    let c = c.into_raw_fd();
                    if let Err(e) = sock::set_nonblocking(c) {
                        sock::close(c);
                        return Err(e);
                    }
                    Ok(i64::from(c))
                }),
                // SAFETY: `fd` is a stream (the caller's contract).
                5 => sock::drive(raw, false, armed, || unsafe {
                    borrowed::<TcpStream>(fd).read(buf()).map(|n| n as i64)
                }),
                // SAFETY: as above.
                6 => sock::drive(raw, true, armed, || unsafe {
                    borrowed::<TcpStream>(fd).write(buf()).map(|n| n as i64)
                }),
                // Every byte, waiting as it must: `write_all` is a library
                // loop over `write` in a task, so this is never armed.
                _ => {
                    let all = buf();
                    let mut done = 0;
                    loop {
                        if done == all.len() {
                            break Got::Done(done as i64);
                        }
                        // SAFETY: as above.
                        let got = sock::drive(raw, true, false, || unsafe {
                            borrowed::<TcpStream>(fd)
                                .write(&all[done..])
                                .map(|n| n as i64)
                        });
                        match got {
                            Got::Done(0) => break Got::Done(done as i64),
                            Got::Done(n) => done += n as usize,
                            Got::Fail(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                            other => break other,
                        }
                    }
                }
            };
            match got {
                Got::Done(n) => (-1, n),
                Got::NotReady => (sock::IO_NOT_READY, 0),
                Got::Fail(e) => net_cause(&e),
            }
        }
    };
    // SAFETY: the caller's contract.
    unsafe { *value = got.1 };
    Some(got.0)
}

/// Whether the OS error `errno` is a read timeout expiring (`EAGAIN` from
/// a socket with `SO_RCVTIMEO` set, or `ETIMEDOUT`).
#[cfg(feature = "net")]
fn timed_out(errno: i64) -> bool {
    matches!(
        std::io::Error::from_raw_os_error(errno as i32).kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// The `NetError` code of a socket call's failure, as [`karac_mir_tcp`]
/// numbers them, and the `Other` payload: the OS error code, or -1.
#[cfg(all(feature = "net", unix))]
fn net_cause(e: &std::io::Error) -> (i32, i64) {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::Interrupted => (0, 0),
        K::AddrInUse => (2, 0),
        K::ConnectionRefused => (3, 0),
        K::PermissionDenied => (4, 0),
        K::WouldBlock | K::TimedOut => (5, 0),
        _ => (1, i64::from(e.raw_os_error().unwrap_or(-1))),
    }
}

/// The socket behind the descriptor `fd`, borrowed: it is not closed when
/// the wrapper goes (`karac_runtime_tcp_close` closes it).
///
/// # Safety
/// `fd` is an open socket of type `T`'s kind.
#[cfg(all(feature = "net", unix))]
unsafe fn borrowed<T: std::os::fd::FromRawFd>(fd: i64) -> std::mem::ManuallyDrop<T> {
    // SAFETY: the caller's contract.
    std::mem::ManuallyDrop::new(unsafe { T::from_raw_fd(fd as std::os::fd::RawFd) })
}

/// The sockets' methods beyond [`karac_mir_tcp`]'s, by `op`:
/// 0 `TcpListener.local_addr(fd)`, 1 `TcpStream.set_read_timeout(fd)`,
/// 2 `UdpSocket.bind(ptr, len)`, 3 `send_to(fd, ptr, len, addr)`,
/// 4 `recv_from(fd, ptr, len)`, 5 `connect(fd, addr)`, 6 `send(fd, ptr,
/// len)`, 7 `recv(fd, ptr, len)`, 8 `UdpSocket.local_addr(fd)`,
/// 9 `UdpSocket.set_read_timeout(fd)` and 10 `UdpSocket.try_clone(fd)`.
/// A timeout is `len` milliseconds when `addr_len` is 1, and none when it
/// is 0; one below 1 ms, like an address that does not parse, is
/// `InvalidInput`. Results as `karac_mir_tcp`'s, with an address (`local_addr`,
/// `recv_from`'s sender) as a fresh `String` at `out`.
///
/// # Safety
///
/// `ptr` and `len` describe the address or buffer the op reads or writes,
/// `addr` and `addr_len` an address for `send_to` / `connect`, `fd` is a
/// socket of the op's kind, `value` is writable and `out` is writable for
/// a `String`.
#[cfg(all(feature = "net", unix))]
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn karac_mir_net(
    op: i32,
    fd: i64,
    ptr: *mut u8,
    len: i64,
    addr: *const u8,
    addr_len: i64,
    value: *mut i64,
    out: *mut RawVec,
) -> i32 {
    use crate::mir_sock::{self as sock, Got};
    use std::net::{TcpListener, TcpStream, UdpSocket};
    use std::os::fd::IntoRawFd;
    let armed = sock::take_armed();
    // A datagram call that can wait, under the reactor contract
    // (`mir_sock.rs`): an armed one that would block is `IO_NOT_READY`.
    let driven = |write: bool, call: &mut dyn FnMut() -> std::io::Result<i64>| match sock::drive(
        fd as i32, write, armed, call,
    ) {
        Got::Done(n) => Ok(n),
        Got::NotReady => Err((sock::IO_NOT_READY, 0)),
        Got::Fail(e) => Err(net_cause(&e)),
    };
    // SAFETY: the caller's contract.
    let text = |p: *const u8, n: i64| unsafe {
        std::str::from_utf8(buf_bytes(p, n.max(0) as usize))
            .ok()
            .and_then(|t| t.parse::<std::net::SocketAddr>().ok())
    };
    // SAFETY: the caller's contract.
    let bytes = || unsafe { buf_bytes(ptr, len.max(0) as usize) };
    // SAFETY: the caller's contract.
    let bytes_mut = || unsafe {
        if ptr.is_null() || len <= 0 {
            &mut [][..]
        } else {
            std::slice::from_raw_parts_mut(ptr, len as usize)
        }
    };
    let timeout = || match (addr_len, len) {
        (0, _) => Ok(None),
        (_, ms) if ms >= 1 => Ok(Some(std::time::Duration::from_millis(ms as u64))),
        _ => Err((7, 0)),
    };
    let addr_out = |a: std::io::Result<std::net::SocketAddr>| match a {
        Ok(a) => {
            // SAFETY: the caller's contract.
            unsafe { string_of(out, a.to_string().as_bytes()) };
            Ok(0)
        }
        Err(e) => Err(net_cause(&e)),
    };
    let unit = |r: std::io::Result<()>| r.map(|()| 0).map_err(|e| net_cause(&e));
    // SAFETY: the caller's contract.
    let res: Result<i64, (i32, i64)> = unsafe {
        match op {
            0 => addr_out(borrowed::<TcpListener>(fd).local_addr()),
            1 => timeout().and_then(|t| unit(borrowed::<TcpStream>(fd).set_read_timeout(t))),
            2 => match text(ptr, len) {
                None => Err((7, 0)),
                Some(at) => match UdpSocket::bind(at).and_then(|s| {
                    s.set_nonblocking(true)?;
                    Ok(s)
                }) {
                    Ok(s) => Ok(i64::from(s.into_raw_fd())),
                    Err(e) => Err(net_cause(&e)),
                },
            },
            3 => match text(addr, addr_len) {
                None => Err((7, 0)),
                Some(at) => driven(true, &mut || {
                    borrowed::<UdpSocket>(fd)
                        .send_to(bytes(), at)
                        .map(|n| n as i64)
                }),
            },
            4 => {
                let mut from = None;
                driven(false, &mut || {
                    let (n, a) = borrowed::<UdpSocket>(fd).recv_from(bytes_mut())?;
                    from = Some(a);
                    Ok(n as i64)
                })
                .and_then(|n| addr_out(Ok(from.expect("recv_from's sender"))).map(|_| n))
            }
            5 => match text(addr, addr_len) {
                None => Err((7, 0)),
                Some(at) => unit(borrowed::<UdpSocket>(fd).connect(at)),
            },
            6 => driven(true, &mut || {
                borrowed::<UdpSocket>(fd).send(bytes()).map(|n| n as i64)
            }),
            7 => driven(false, &mut || {
                borrowed::<UdpSocket>(fd)
                    .recv(bytes_mut())
                    .map(|n| n as i64)
            }),
            8 => addr_out(borrowed::<UdpSocket>(fd).local_addr()),
            9 => timeout().and_then(|t| unit(borrowed::<UdpSocket>(fd).set_read_timeout(t))),
            _ => borrowed::<UdpSocket>(fd)
                .try_clone()
                .map(|s| i64::from(s.into_raw_fd()))
                .map_err(|e| net_cause(&e)),
        }
    };
    let (code, v) = match res {
        Ok(v) => (-1, v),
        Err(c) => c,
    };
    // SAFETY: the caller's contract.
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
    a_len: u64,
    b: *const u8,
    b_len: u64,
    handle: i64,
    status: *mut i64,
    out: *mut RawVec,
) -> i32 {
    let a_len = a_len as usize;
    let b_len = b_len as usize;
    let mut st = 0i64;
    let (mut body, mut body_len) = (std::ptr::null_mut::<u8>(), 0i64);
    let (mut msg, mut msg_len) = (std::ptr::null_mut::<u8>(), 0i64);
    let mut headers = 0i64;
    // Inside a task, the request already went to a helper, which the
    // task's `__ready` waited for (`mir_http.rs`).
    #[cfg(unix)]
    let answered = crate::mir_http::take_answer();
    #[cfg(not(unix))]
    let answered = None;
    // SAFETY: the caller's contract, which is each callee's; the out
    // params are locals.
    unsafe {
        if let Some(res) = answered {
            crate::http_deliver(
                res,
                &mut st,
                &mut body,
                &mut body_len,
                &mut msg,
                &mut msg_len,
                &mut headers,
            );
        } else {
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
        RawVec::new(q, n, cap)
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
    name_len: u64,
    out: *mut RawVec,
) -> i32 {
    let name_len = name_len as usize;
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
    unsafe { out.write(RawVec::new(items, n, n)) };
}

// The WebSocket handshake's accept key, shared with the MIR interpreter
// (`src/ws_frame.rs`, by `#[path]`). The rest of WebSocket is Kāra
// (runtime/stdlib/source/websocket.kara); the frame half serves tests.
#[cfg(feature = "net")]
#[path = "../../src/ws_frame.rs"]
#[allow(dead_code)]
mod ws_frame;

/// `WebSocket.__accept_key(key)`: the `Sec-WebSocket-Accept` value, as a
/// fresh `String` at `out`.
///
/// # Safety
/// `p` and `n` describe the key's bytes, and `out` is writable for a
/// `String`.
#[cfg(feature = "net")]
#[no_mangle]
pub unsafe extern "C" fn karac_mir_ws_accept_key(p: *const u8, n: i64, out: *mut RawVec) {
    // SAFETY: the caller's contract.
    let key = unsafe { buf_bytes(p, n.max(0) as usize) };
    let accept = ws_frame::accept_key(&String::from_utf8_lossy(key));
    // SAFETY: the caller's contract.
    unsafe { string_of(out, accept.as_bytes()) };
}

// The servers' requests and responses on the MIR backend (`src/http1.rs`,
// shared by `#[path]` with the MIR interpreter).
#[cfg(feature = "net")]
#[path = "../../src/http1.rs"]
#[allow(dead_code)] // the interpreter's half: `body_text`, a bad request's reason
mod http1;

#[cfg(feature = "net")]
crate::mir_task::program_state! {
    /// The request each task's handler is answering, by task id (0 outside
    /// the executor), which `Request`'s methods read: a server answers each
    /// connection in a task of its own (B-2026-10-25-41).
    static MIR_REQUESTS: Option<std::collections::HashMap<u64, http1::Request>> = None;
}

/// The running task's key in [`MIR_REQUESTS`].
#[cfg(feature = "net")]
fn request_key() -> u64 {
    crate::mir_task::current_task().unwrap_or(0)
}

/// Makes `req` the running task's request, forgotten when the task ends.
#[cfg(feature = "net")]
fn set_request(req: http1::Request) {
    thread_local! {
        static HOOKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    let key = request_key();
    // Each executor's own tasks end on its thread.
    if key != 0 && !HOOKED.with(|h| h.replace(true)) {
        crate::mir_task::on_task_end(|task| {
            MIR_REQUESTS.with(|r| r.as_mut().map(|r| r.remove(&task)));
        });
    }
    MIR_REQUESTS.with(|r| r.get_or_insert_with(Default::default).insert(key, req));
}

/// `f` of the running task's request, or of an empty one before it has
/// one.
#[cfg(feature = "net")]
fn with_request<T>(f: impl FnOnce(&http1::Request) -> T) -> T {
    let key = request_key();
    MIR_REQUESTS.with(|r| match r.as_ref().and_then(|r| r.get(&key)) {
        Some(req) => f(req),
        None => f(&http1::Request::default()),
    })
}

/// `Server.__install(buf, eof)` for the servers in
/// runtime/stdlib/source/http_server.kara: makes the request at the start
/// of the `n` bytes at `p` the current one, and returns the bytes it took;
/// 0 when they hold no whole request yet and `eof` is 0, or -1 when the
/// connection holds no request.
///
/// # Safety
///
/// `p` describes `n` readable bytes.
#[cfg(feature = "net")]
#[no_mangle]
pub unsafe extern "C" fn karac_mir_http_install(p: *const u8, n: i64, eof: i32) -> i64 {
    // SAFETY: the caller's contract.
    let buf = unsafe { buf_bytes(p, n.max(0) as usize) };
    match http1::parse_request(buf, eof != 0) {
        http1::Parse::Done(req, used) => {
            set_request(req);
            used as i64
        }
        http1::Parse::More => 0,
        http1::Parse::Closed | http1::Parse::Bad(_) => -1,
    }
}

/// `Server.__wire(resp)`: the bytes of a response to the current request
/// with `status`, the body's bytes and `headers` pairs of `String`s, which
/// keeps the connection as the request asks, as a `Vec[u8]` at `out`.
///
/// # Safety
///
/// `body` describes `body_len` readable bytes, `headers` holds
/// `headers_len` `(String, String)`s, and `out` is writable for a `Vec`.
#[cfg(feature = "net")]
#[no_mangle]
pub unsafe extern "C" fn karac_mir_http_wire(
    status: i64,
    body: *const u8,
    body_len: u64,
    headers: *const RawVec,
    headers_len: u64,
    out: *mut RawVec,
) {
    // SAFETY: the caller's contract.
    let pairs = unsafe { header_pairs(headers, headers_len as usize) };
    // SAFETY: the caller's contract.
    let body = unsafe { bytes_at(body, body_len as usize) };
    let wire = http1::response_bytes(
        status,
        body,
        &pairs,
        with_request(http1::Request::keep_alive),
    );
    // SAFETY: the caller's contract; a `Vec[u8]` is laid out as a `String`.
    unsafe { string_of(out, &wire) };
}

/// `Server.__keep_alive()`: 1 when the current request keeps its
/// connection open for the next one, else 0.
#[cfg(feature = "net")]
#[no_mangle]
pub extern "C" fn karac_mir_http_keep_alive() -> u8 {
    u8::from(with_request(http1::Request::keep_alive))
}

/// The `n` `(String, String)` pairs at `headers`.
///
/// # Safety
///
/// `headers` holds `n` `(String, String)`s, or `n` is 0.
#[cfg(feature = "net")]
unsafe fn header_pairs(headers: *const RawVec, n: usize) -> Vec<(String, String)> {
    let mut pairs = Vec::with_capacity(n);
    for i in 0..n {
        // SAFETY: the caller's contract: two `String`s per pair.
        let (k, v) = unsafe {
            let k = &*headers.add(2 * i);
            let v = &*headers.add(2 * i + 1);
            (bytes_at(k.ptr, k.len), bytes_at(v.ptr, v.len))
        };
        pairs.push((
            String::from_utf8_lossy(k).into_owned(),
            String::from_utf8_lossy(v).into_owned(),
        ));
    }
    pairs
}

/// `Request`'s methods, on the current request: `op` 0 `path`, 1 `method`
/// and 2 `body`, each a `String` at `out`; 3 `header(name)`, 1 with its
/// value at `out` or 0; 4 `headers` and 5 `query`, a `Vec[(String,
/// String)]` at `out`.
///
/// # Safety
///
/// `name` describes `name_len` readable bytes, and `out` is writable for
/// the result.
#[cfg(feature = "net")]
#[no_mangle]
pub unsafe extern "C" fn karac_mir_http_request(
    op: i32,
    name: *const u8,
    name_len: u64,
    out: *mut RawVec,
) -> i32 {
    let name_len = name_len as usize;
    with_request(|r| {
        // SAFETY: the caller's contract.
        unsafe {
            match op {
                0 => string_of(out, r.path().as_bytes()),
                1 => string_of(out, r.method.as_bytes()),
                2 => string_of(out, &r.body),
                3 => {
                    let name = String::from_utf8_lossy(bytes_at(name, name_len));
                    match r.header(&name) {
                        Some(v) => string_of(out, v.as_bytes()),
                        None => return 0,
                    }
                }
                4 => pairs_of(out, &r.headers),
                _ => pairs_of(out, &r.query()),
            }
        }
        1
    })
}

/// The `len` bytes at `p`, or none when `p` is null.
///
/// # Safety
///
/// `p` is null or describes `len` readable bytes.
#[cfg(feature = "net")]
unsafe fn bytes_at<'a>(p: *const u8, len: usize) -> &'a [u8] {
    if p.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: the caller's contract.
        unsafe { std::slice::from_raw_parts(p, len) }
    }
}

/// Writes a fresh `Vec[(String, String)]` of `pairs` at `out`.
///
/// # Safety
///
/// `out` is writable for a `Vec`.
#[cfg(feature = "net")]
unsafe fn pairs_of(out: *mut RawVec, pairs: &[(String, String)]) {
    let n = pairs.len();
    let items = if n == 0 {
        std::ptr::null_mut()
    } else {
        karac_realloc_or_panic(std::ptr::null_mut(), n * 2 * std::mem::size_of::<RawVec>())
    };
    for (i, (k, v)) in pairs.iter().enumerate() {
        // SAFETY: `items` has room for `n` pairs.
        unsafe {
            let at = items.cast::<RawVec>().add(2 * i);
            string_of(at, k.as_bytes());
            string_of(at.add(1), v.as_bytes());
        }
    }
    // SAFETY: the caller's contract.
    unsafe { out.write(RawVec::new(items, n, n)) };
}
