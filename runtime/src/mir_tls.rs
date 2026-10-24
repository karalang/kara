//! The TLS server on the MIR backend: `TlsListener` and `TlsStream`
//! (docs/library/io.md § TLS server), over the connection the MIR
//! interpreter runs too (`src/tls_conn.rs`, shared by `#[path]`). A
//! listener is a non-blocking TCP listener with its server config beside
//! it; an accepted stream is a non-blocking socket with its rustls
//! connection, both kept here by descriptor. Calls wait as `mir_sock.rs`'s
//! do: a task's readiness check ([`karac_mir_ready_tls`]) asks the
//! connection, and a call outside a task waits on the socket itself.

#[path = "../../src/tls_conn.rs"]
mod tls_conn;

use crate::mir_sock as sock;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{FromRawFd, IntoRawFd, RawFd};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tls_conn::TlsConn;

/// An accepted stream's state.
struct Conn {
    tls: TlsConn,
    /// `set_read_timeout`'s.
    timeout: Option<Duration>,
}

/// The listeners' configs and the streams' connections, by descriptor.
#[derive(Default)]
struct Table {
    configs: HashMap<RawFd, Arc<rustls::ServerConfig>>,
    conns: HashMap<RawFd, Conn>,
}

static TABLE: Mutex<Option<Table>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Table) -> T) -> T {
    let mut t = TABLE.lock().unwrap_or_else(|p| p.into_inner());
    f(t.get_or_insert_with(Table::default))
}

/// Runs `f` on `fd`'s connection, taken out of the table for the call so
/// a wait inside it holds no lock; `None` when `fd` has none.
fn on_conn<T>(fd: RawFd, f: impl FnOnce(&mut Conn) -> T) -> Option<T> {
    let mut c = with(|t| t.conns.remove(&fd))?;
    let r = f(&mut c);
    with(|t| t.conns.insert(fd, c));
    Some(r)
}

/// The stream behind `fd`, borrowed: it is not closed when it goes.
fn stream(fd: RawFd) -> std::mem::ManuallyDrop<TcpStream> {
    // SAFETY: `fd` is an open socket the program owns.
    std::mem::ManuallyDrop::new(unsafe { TcpStream::from_raw_fd(fd) })
}

/// The `NetError` code of a TLS call's failure, as `karac_mir_tcp`
/// numbers them, with 8 `Tls` (its `TlsFailure` in the payload).
fn cause(e: &tls_conn::Error) -> (i32, i64) {
    match e {
        tls_conn::Error::Tls(f) => (8, i64::from(f.code())),
        tls_conn::Error::Io(e) => io_cause(e),
    }
}

fn io_cause(e: &Error) -> (i32, i64) {
    match e.kind() {
        ErrorKind::Interrupted => (0, 0),
        ErrorKind::AddrInUse => (2, 0),
        ErrorKind::ConnectionRefused => (3, 0),
        ErrorKind::PermissionDenied => (4, 0),
        ErrorKind::WouldBlock | ErrorKind::TimedOut => (5, 0),
        _ => (1, i64::from(e.raw_os_error().unwrap_or(-1))),
    }
}

/// A wait for `fd` until `deadline`: `Ok(true)` once it is readable
/// (`out` false) or writable, `Ok(false)` once the deadline passed.
fn waiter(fd: RawFd, deadline: Option<Instant>) -> impl FnMut(bool) -> std::io::Result<bool> {
    move |out| {
        let left = match deadline {
            None => -1,
            Some(at) => {
                let left = at.saturating_duration_since(Instant::now()).as_millis() as i64;
                if left == 0 {
                    return Ok(false);
                }
                left
            }
        };
        Ok(sock::karac_mir_io_wait(i64::from(fd), u8::from(out), left) == 0)
    }
}

/// The bytes at `p`.
///
/// # Safety
/// `p` is null or describes `len` readable bytes.
unsafe fn bytes<'a>(p: *const u8, len: i64) -> &'a [u8] {
    if p.is_null() || len <= 0 {
        &[]
    } else {
        // SAFETY: the caller's contract.
        unsafe { std::slice::from_raw_parts(p, len as usize) }
    }
}

/// `TlsListener` / `TlsStream` by `op`: 0 `bind_tls(ptr, len)` serving
/// the chain at `a` with the key at `b`, 1 `accept(fd)`, 2 `read(fd, ptr,
/// len)`, 3 `write(fd, ptr, len)`, 4 `shutdown_write(fd)` and 5
/// `set_read_timeout(fd)`, `len` ms when `a_len` is 1 and none when it is
/// 0. Results as `karac_mir_tcp`'s: negative for success with the
/// descriptor or count in `value`, else the `NetError` code, 8 being
/// `Tls` with its `TlsFailure` in `value`. `TlsListener.local_addr` is
/// `karac_mir_net`'s op 0.
///
/// # Safety
///
/// `ptr` and `len` describe the address or buffer the op reads or writes,
/// `a` / `b` and their lengths PEM text for op 0, `fd` is a socket of the
/// op's kind, and `value` is writable.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn karac_mir_tls(
    op: i32,
    fd: i64,
    ptr: *mut u8,
    len: i64,
    a: *const u8,
    a_len: i64,
    b: *const u8,
    b_len: i64,
    value: *mut i64,
) -> i32 {
    // Every TLS call waits as it must: only the readiness check runs
    // without blocking.
    let _ = sock::take_armed();
    let raw = fd as RawFd;
    let missing = || (1, 9); // EBADF, as the interpreter has it
    let res: Result<i64, (i32, i64)> = match op {
        0 => {
            // SAFETY: the caller's contract.
            let (addr, cert, key) = unsafe { (bytes(ptr, len), bytes(a, a_len), bytes(b, b_len)) };
            let at = std::str::from_utf8(addr)
                .ok()
                .and_then(|t| t.parse::<std::net::SocketAddr>().ok());
            match (at, tls_conn::server_config(cert, key)) {
                (Some(at), Some(config)) => match TcpListener::bind(at).and_then(|l| {
                    l.set_nonblocking(true)?;
                    Ok(l)
                }) {
                    Ok(l) => {
                        if at.port() == 0 && std::env::var_os("KARAC_ANNOUNCE_PORT").is_some() {
                            if let Ok(local) = l.local_addr() {
                                eprintln!("BOUND_PORT={}", local.port());
                            }
                        }
                        let fd = l.into_raw_fd();
                        with(|t| t.configs.insert(fd, config));
                        Ok(i64::from(fd))
                    }
                    Err(e) => Err(io_cause(&e)),
                },
                _ => Err((7, 0)),
            }
        }
        1 => match with(|t| t.configs.get(&raw).cloned()) {
            None => Err(missing()),
            Some(config) => {
                let got = sock::drive(raw, false, false, || {
                    // SAFETY: `fd` is a listener (the caller's contract).
                    let l = std::mem::ManuallyDrop::new(unsafe { TcpListener::from_raw_fd(raw) });
                    let (c, _) = l.accept()?;
                    let c = c.into_raw_fd();
                    if let Err(e) = sock::set_nonblocking(c) {
                        sock::close(c);
                        return Err(e);
                    }
                    Ok(i64::from(c))
                });
                match got {
                    sock::Got::Done(c) => match TlsConn::new(&config) {
                        Some(tls) => {
                            with(|t| t.conns.insert(c as RawFd, Conn { tls, timeout: None }));
                            Ok(c)
                        }
                        None => {
                            sock::close(c as RawFd);
                            Err((8, 0))
                        }
                    },
                    sock::Got::NotReady => Err((5, 0)),
                    sock::Got::Fail(e) => Err(io_cause(&e)),
                }
            }
        },
        2 => {
            let timed = crate::mir_task::take_timed_out(raw);
            on_conn(raw, |c| {
                if timed {
                    return Err((5, 0));
                }
                let deadline = c.timeout.map(|d| Instant::now() + d);
                // SAFETY: the caller's contract.
                let buf = unsafe {
                    if ptr.is_null() || len <= 0 {
                        &mut [][..]
                    } else {
                        std::slice::from_raw_parts_mut(ptr, len as usize)
                    }
                };
                let mut s = stream(raw);
                c.tls
                    .read(&mut s, buf, &mut waiter(raw, deadline))
                    .map(|n| n as i64)
                    .map_err(|e| cause(&e))
            })
            .unwrap_or_else(|| Err(missing()))
        }
        3 => on_conn(raw, |c| {
            let mut s = stream(raw);
            // SAFETY: the caller's contract.
            let buf = unsafe { bytes(ptr, len) };
            c.tls
                .write(&mut s, buf, &mut waiter(raw, None))
                .map(|n| n as i64)
                .map_err(|e| cause(&e))
        })
        .unwrap_or_else(|| Err(missing())),
        4 => on_conn(raw, |c| {
            c.tls.close(&mut stream(raw));
            Ok(0)
        })
        .unwrap_or_else(|| Err(missing())),
        _ => match (a_len, len) {
            (0, _) | (_, 1..) => on_conn(raw, |c| {
                c.timeout = (a_len != 0).then(|| Duration::from_millis(len as u64));
                Ok(0)
            })
            .unwrap_or_else(|| Err(missing())),
            _ => Err((7, 0)),
        },
    };
    let (code, v) = match res {
        Ok(v) => (-1, v),
        Err(c) => c,
    };
    // SAFETY: the caller's contract.
    unsafe { *value = v };
    code
}

/// `__ready` of a `TlsStream` read (`write` 0) or write: whether it would
/// complete now, after moving the connection on. A read with a read
/// timeout also ends at the deadline its first ask sets, and the read
/// that follows then fails `TimedOut`, as a `TcpStream`'s does.
#[no_mangle]
pub extern "C" fn karac_mir_ready_tls(fd: i64, write: u8) -> u8 {
    let raw = fd as RawFd;
    let got = on_conn(raw, |c| {
        let (ready, out) = c.tls.ready(&mut stream(raw), write != 0);
        (ready, out, if write != 0 { None } else { c.timeout })
    });
    match got {
        Some((ready, out, by)) => crate::mir_task::ready_io(raw, ready, out, by),
        // Not a TLS stream: the call reports it.
        None => 1,
    }
}

/// Closes a `TlsListener`'s or `TlsStream`'s descriptor, for its `Drop`:
/// a stream sends `close_notify` first, as far as the socket takes it.
#[no_mangle]
pub extern "C" fn karac_mir_tls_close(fd: i64) {
    if fd < 0 {
        return;
    }
    let raw = fd as RawFd;
    let conn = with(|t| {
        t.configs.remove(&raw);
        t.conns.remove(&raw)
    });
    if let Some(mut c) = conn {
        c.tls.close(&mut stream(raw));
    }
    sock::close(raw);
}
