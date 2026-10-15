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

/// A `BufReader` or `BufWriter` over the `File` its constructor took.
pub enum KaracBuf {
    Reader(std::io::BufReader<std::fs::File>),
    Writer(std::io::BufWriter<std::fs::File>),
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
/// (`len` the capacity, `<= 0` for the default); then, on the buffered
/// handle at `handle`: `read_line` (2) and `read_to_string` (3), appending
/// to the `String` at `ptr`; `read` (4) into `ptr[..len]`; a `LinesIter`'s
/// `next` (5), the line without its `\n` / `\r\n` as a fresh `String` at
/// `msg`, or `-2` at the end; `write` (6) and `write_all` (7) of
/// `ptr[..len]`; `flush` (8); `consume` (9) of `len` bytes.
///
/// # Safety
/// `handle` is a live file handle for ops 0-1 and a live buffered handle
/// otherwise; `ptr` holds what its op reads or writes; `value` and `msg` as
/// [`io_code`] has them.
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
    if op <= 1 {
        // SAFETY: a live handle from `karac_runtime_file_open` & co., which
        // this takes.
        let file = unsafe { crate::file::take_file(handle.cast()) };
        let cap = usize::try_from(len).ok().filter(|&n| n > 0).unwrap_or(8192);
        let buf = if op == 0 {
            KaracBuf::Reader(std::io::BufReader::with_capacity(cap, file))
        } else {
            KaracBuf::Writer(std::io::BufWriter::with_capacity(cap, file))
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
            0 => built(ev::tcp_bind(ptr, len, false)),
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
    use std::net::{TcpListener, TcpStream, UdpSocket};
    use std::os::fd::IntoRawFd;
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
    let count = |r: std::io::Result<usize>| r.map(|n| n as i64).map_err(|e| net_cause(&e));
    let unit = |r: std::io::Result<()>| r.map(|()| 0).map_err(|e| net_cause(&e));
    // SAFETY: the caller's contract.
    let res: Result<i64, (i32, i64)> = unsafe {
        match op {
            0 => addr_out(borrowed::<TcpListener>(fd).local_addr()),
            1 => timeout().and_then(|t| unit(borrowed::<TcpStream>(fd).set_read_timeout(t))),
            2 => match text(ptr, len) {
                None => Err((7, 0)),
                Some(at) => UdpSocket::bind(at)
                    .map(|s| i64::from(s.into_raw_fd()))
                    .map_err(|e| net_cause(&e)),
            },
            3 => match text(addr, addr_len) {
                None => Err((7, 0)),
                Some(at) => count(borrowed::<UdpSocket>(fd).send_to(bytes(), at)),
            },
            4 => match borrowed::<UdpSocket>(fd).recv_from(bytes_mut()) {
                Ok((n, from)) => addr_out(Ok(from)).map(|_| n as i64),
                Err(e) => Err(net_cause(&e)),
            },
            5 => match text(addr, addr_len) {
                None => Err((7, 0)),
                Some(at) => unit(borrowed::<UdpSocket>(fd).connect(at)),
            },
            6 => count(borrowed::<UdpSocket>(fd).send(bytes())),
            7 => count(borrowed::<UdpSocket>(fd).recv(bytes_mut())),
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

// `Server.serve` on the MIR backend: the blocking one-request-at-a-time
// server the MIR interpreter runs too (`src/http1.rs`, shared by `#[path]`).
#[cfg(feature = "net")]
#[path = "../../src/http1.rs"]
mod http1;

#[cfg(feature = "net")]
thread_local! {
    /// The request the handler is answering, which `Request`'s methods read.
    static MIR_REQUEST: std::cell::RefCell<http1::Request> = Default::default();
}

/// Binds `addr` for `Server.serve`: the server, printing `BOUND_PORT=<n>`
/// on stderr first, where a test harness reads it and the program's own
/// output does not carry it (B-2026-10-15-69), or null with the
/// `HttpError` message as a `String` at `msg`.
///
/// # Safety
///
/// `addr` describes `len` readable bytes and `msg` is writable for a
/// `String`.
#[cfg(feature = "net")]
#[no_mangle]
pub unsafe extern "C" fn karac_mir_http_bind(
    addr: *const u8,
    len: usize,
    msg: *mut RawVec,
) -> *mut http1::Server {
    // SAFETY: the caller's contract.
    let addr = unsafe { bytes_at(addr, len) };
    match http1::Server::bind(&String::from_utf8_lossy(addr)) {
        Ok((server, port)) => {
            eprintln!("BOUND_PORT={port}");
            Box::into_raw(Box::new(server))
        }
        Err(m) => {
            // SAFETY: the caller's contract.
            unsafe { string_of(msg, m.as_bytes()) };
            std::ptr::null_mut()
        }
    }
}

/// Waits for the next request and makes it the one `Request`'s methods
/// read: 1, or 0 with the `HttpError` message as a `String` at `msg`, when
/// the server can accept no more. A server that fails is freed.
///
/// # Safety
///
/// `server` came from [`karac_mir_http_bind`] and has not failed, and `msg`
/// is writable for a `String`.
#[cfg(feature = "net")]
#[no_mangle]
pub unsafe extern "C" fn karac_mir_http_next(server: *mut http1::Server, msg: *mut RawVec) -> i32 {
    // SAFETY: the caller's contract.
    match unsafe { (*server).accept_request() } {
        Ok(req) => {
            MIR_REQUEST.with(|r| *r.borrow_mut() = req);
            1
        }
        Err(m) => {
            // SAFETY: the caller's contract; nothing uses the server again.
            unsafe {
                string_of(msg, m.as_bytes());
                drop(Box::from_raw(server));
            }
            0
        }
    }
}

/// Answers the current request with `status`, the body's bytes and
/// `headers` pairs of `String`s (`headers_len` of them; null for none).
///
/// # Safety
///
/// `server` came from [`karac_mir_http_bind`], `body` describes `body_len`
/// readable bytes, and `headers` holds `headers_len` `(String, String)`s.
#[cfg(feature = "net")]
#[no_mangle]
pub unsafe extern "C" fn karac_mir_http_respond(
    server: *mut http1::Server,
    status: i64,
    body: *const u8,
    body_len: usize,
    headers: *const RawVec,
    headers_len: usize,
) {
    let mut pairs = Vec::with_capacity(headers_len);
    for i in 0..headers_len {
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
    // SAFETY: the caller's contract.
    unsafe { (*server).respond(status, bytes_at(body, body_len), &pairs) };
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
    name_len: usize,
    out: *mut RawVec,
) -> i32 {
    MIR_REQUEST.with(|r| {
        let r = r.borrow();
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
    unsafe {
        out.write(RawVec {
            ptr: items,
            len: n,
            cap: n,
        })
    };
}
