//! The MIR backend's socket calls under the reactor contract (M4 item 2,
//! the header of `mir_task.rs`): every socket is non-blocking from birth,
//! and a call that would block either answers [`IO_NOT_READY`], when the
//! readiness loop armed it ([`take_armed`]), or waits on the descriptor
//! with [`karac_mir_io_wait`] and tries again, so a program that runs no
//! task sees blocking calls as before.

use std::io::{Error, ErrorKind};
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};

/// A call that would block, inside a readiness loop: it did nothing, and
/// the loop waits for the descriptor and calls again.
pub const IO_NOT_READY: i32 = 100;
/// A `connect` under way: the loop waits for the descriptor (in `value`)
/// to be writable and then calls `connect_finish`.
pub const IO_IN_PROGRESS: i32 = 101;

thread_local! {
    /// Set by the readiness loop ([`karac_mir_io_arm`]) and taken by the
    /// call that follows: only a call made from inside a
    /// readiness loop may answer `IO_NOT_READY`. A call the loop does not
    /// wrap (a body that is no coroutine, or no task at all) blocks.
    static ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Arms the next socket call on this thread (see [`ARMED`]): the
/// readiness loop calls it just before the call it wraps, once it
/// handles `IO_NOT_READY`.
#[no_mangle]
pub extern "C" fn karac_mir_io_arm() {
    ARMED.with(|a| a.set(true));
}

/// Whether the readiness loop armed this call; clears the bit.
pub(crate) fn take_armed() -> bool {
    ARMED.with(|a| a.replace(false))
}

/// Waits until `fd` is readable (`write` 0) or writable: 0 when it is, 1
/// when `timeout_ms` passed first (-1 waits for ever). One descriptor,
/// blocking the thread; the reactor's executor never calls it for a task
/// it can park.
#[no_mangle]
pub extern "C" fn karac_mir_io_wait(fd: i64, write: u8, timeout_ms: i64) -> i32 {
    let events = if write != 0 {
        libc::POLLOUT
    } else {
        libc::POLLIN
    };
    let deadline = (timeout_ms >= 0)
        .then(|| std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64));
    loop {
        let left = match deadline {
            None => -1,
            Some(at) => {
                let now = std::time::Instant::now();
                if now >= at {
                    return 1;
                }
                (at - now).as_millis().min(i32::MAX as u128) as i32
            }
        };
        let mut p = libc::pollfd {
            fd: fd as RawFd,
            events,
            revents: 0,
        };
        // SAFETY: one initialized entry.
        match unsafe { libc::poll(&mut p, 1, left) } {
            0 if deadline.is_some() => return 1,
            n if n > 0 => return 0,
            // EINTR, or a spurious zero with no deadline: wait again.
            _ => {}
        }
    }
}

/// Sets or clears `O_NONBLOCK` on `fd`, a socket's or a pipe's.
pub(crate) fn set_blocking(fd: RawFd, blocking: bool) -> std::io::Result<()> {
    // SAFETY: `fcntl` on a descriptor the caller holds open.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(Error::last_os_error());
    }
    let want = if blocking {
        flags & !libc::O_NONBLOCK
    } else {
        flags | libc::O_NONBLOCK
    };
    // SAFETY: as above.
    if want != flags && unsafe { libc::fcntl(fd, libc::F_SETFL, want) } < 0 {
        return Err(Error::last_os_error());
    }
    Ok(())
}

/// A non-blocking pipe end read or written through `std::io` (a
/// `BufReader` over a child's stdout, say): a call that would block waits
/// for the descriptor, so the stream reads as a blocking one.
pub(crate) struct Waiting<T>(pub T);

impl<T: std::io::Read + std::os::fd::AsFd> std::io::Read for Waiting<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match self.0.read(buf) {
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    karac_mir_io_wait(i64::from(self.0.as_fd().as_raw_fd()), 0, -1);
                }
                r => return r,
            }
        }
    }
}

impl<T: std::io::Write + std::os::fd::AsFd> std::io::Write for Waiting<T> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        loop {
            match self.0.write(buf) {
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    karac_mir_io_wait(i64::from(self.0.as_fd().as_raw_fd()), 1, -1);
                }
                r => return r,
            }
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// A descriptor that turns readable once the child `pid` has exited, for
/// the readiness loop around `Child.wait`: a pidfd on Linux, a kqueue
/// watching the process (`EVFILT_PROC`, `NOTE_EXIT`) on macOS and the
/// BSDs. -1 when there is none to be had (an old kernel, another unix, or
/// a child already gone); a wait then blocks.
pub(crate) fn child_wait_fd(pid: u32) -> RawFd {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // SAFETY: a plain syscall; the pidfd it opens is close-on-exec.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
        if fd < 0 {
            -1
        } else {
            fd as RawFd
        }
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "openbsd"
    ))]
    {
        // SAFETY: plain calls on the kqueue this opens.
        unsafe {
            let kq = libc::kqueue();
            if kq < 0 {
                return -1;
            }
            libc::fcntl(kq, libc::F_SETFD, libc::FD_CLOEXEC);
            let mut ev: libc::kevent = std::mem::zeroed();
            ev.ident = pid as _;
            ev.filter = libc::EVFILT_PROC;
            ev.flags = libc::EV_ADD;
            ev.fflags = libc::NOTE_EXIT;
            if libc::kevent(kq, &ev, 1, std::ptr::null_mut(), 0, std::ptr::null()) < 0 {
                libc::close(kq);
                return -1;
            }
            kq
        }
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "openbsd"
    )))]
    {
        let _ = pid;
        -1
    }
}

/// What a socket call came to.
pub(crate) enum Got {
    Done(i64),
    NotReady,
    Fail(Error),
}

/// Makes `fd` non-blocking.
pub(crate) fn set_nonblocking(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: `fd` is an open descriptor for the call.
    let s = unsafe { BorrowedFd::borrow_raw(fd) };
    socket2::SockRef::from(&s).set_nonblocking(true)
}

/// `fd`'s read timeout in milliseconds, or -1 for none.
fn read_timeout_ms(fd: RawFd) -> i64 {
    // SAFETY: `fd` is an open socket for the call.
    let s = unsafe { BorrowedFd::borrow_raw(fd) };
    match socket2::SockRef::from(&s).read_timeout() {
        Ok(Some(t)) => t.as_millis().clamp(1, i64::MAX as u128) as i64,
        _ => -1,
    }
}

/// Runs `call` on `fd` until it completes. One that would block answers
/// [`Got::NotReady`] when `armed`; otherwise, and always for a read with a
/// timeout, it waits for `fd` and calls again. A read whose timeout
/// passes fails with `TimedOut`, as does one whose task's suspended wait
/// reached the timeout's deadline (`mir_task::karac_mir_ready_fd`).
pub(crate) fn drive(
    fd: RawFd,
    write: bool,
    armed: bool,
    call: impl FnMut() -> std::io::Result<i64>,
) -> Got {
    if !write && crate::mir_task::take_timed_out(fd) {
        return Got::Fail(Error::from(ErrorKind::TimedOut));
    }
    let timeout = if write { -1 } else { read_timeout_ms(fd) };
    drive_for(fd, write, armed, timeout, call)
}

/// [`drive`] on any descriptor, a socket's or a pipe's, with the timeout
/// in milliseconds (-1 for none) given.
pub(crate) fn drive_for(
    fd: RawFd,
    write: bool,
    armed: bool,
    timeout: i64,
    mut call: impl FnMut() -> std::io::Result<i64>,
) -> Got {
    let deadline = (timeout >= 0)
        .then(|| std::time::Instant::now() + std::time::Duration::from_millis(timeout as u64));
    loop {
        match call() {
            Ok(n) => return Got::Done(n),
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if armed && deadline.is_none() {
                    crate::mir_task::set_again();
                    return Got::NotReady;
                }
                let left = match deadline {
                    None => -1,
                    Some(at) => at
                        .saturating_duration_since(std::time::Instant::now())
                        .as_millis() as i64,
                };
                if left == 0 || karac_mir_io_wait(i64::from(fd), u8::from(write), left) == 1 {
                    return Got::Fail(Error::from(ErrorKind::TimedOut));
                }
            }
            Err(e) => return Got::Fail(e),
        }
    }
}

/// A non-blocking TCP `connect` to `at`: the connected socket, or one
/// whose connect is under way ([`Got::NotReady`] standing for
/// `IO_IN_PROGRESS`, with the descriptor in `fd`). Not `armed`, it waits
/// for the connect to finish.
pub(crate) fn connect(at: std::net::SocketAddr, armed: bool, fd: &mut i64) -> Got {
    use socket2::{Domain, SockAddr, Socket, Type};
    use std::os::fd::IntoRawFd;
    let sock = match Socket::new(Domain::for_address(at), Type::STREAM, None) {
        Ok(s) => s,
        Err(e) => return Got::Fail(e),
    };
    if let Err(e) = sock.set_nonblocking(true) {
        return Got::Fail(e);
    }
    match sock.connect(&SockAddr::from(at)) {
        Ok(()) => return Got::Done(i64::from(sock.into_raw_fd())),
        Err(e)
            if e.raw_os_error() == Some(libc::EINPROGRESS) || e.kind() == ErrorKind::WouldBlock => {
        }
        Err(e) => return Got::Fail(e),
    }
    let raw = sock.into_raw_fd();
    *fd = i64::from(raw);
    if armed {
        crate::mir_task::set_again();
        return Got::NotReady;
    }
    karac_mir_io_wait(i64::from(raw), 1, -1);
    connect_finish(raw)
}

/// Ends the connect under way on `fd`: [`Got::NotReady`] while it still
/// is, then the socket, or the connect's failure with the socket closed.
pub(crate) fn connect_finish(fd: RawFd) -> Got {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: one initialized entry.
    if unsafe { libc::poll(&mut p, 1, 0) } == 0 {
        crate::mir_task::set_again();
        return Got::NotReady;
    }
    // SAFETY: `fd` is the open socket of the connect.
    let s = unsafe { BorrowedFd::borrow_raw(fd) };
    match socket2::SockRef::from(&s).take_error() {
        Ok(None) => Got::Done(i64::from(fd)),
        Ok(Some(e)) | Err(e) => {
            close(fd);
            Got::Fail(e)
        }
    }
}

/// Closes `fd`, deregistering it from the reactor first, and dropping
/// any TLS session on it, as `karac_runtime_tcp_close` does.
pub(crate) fn close(fd: RawFd) {
    crate::mir_task::karac_mir_io_forget(i64::from(fd));
    #[cfg(feature = "tls")]
    crate::tls::forget_session_for_fd(fd);
    // SAFETY: the caller owns `fd` and does not use it again.
    unsafe { libc::close(fd) };
}

/// Closes a socket's descriptor, for the `Drop` of a `TcpListener`,
/// `TcpStream` or `UdpSocket`.
#[no_mangle]
pub extern "C" fn karac_mir_sock_close(fd: i64) {
    if fd >= 0 {
        close(fd as RawFd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};

    /// An armed read with nothing to read answers not-ready at once; an
    /// unarmed one waits for the bytes.
    #[test]
    fn a_read_that_would_block_is_not_ready_only_when_armed() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let at = l.local_addr().unwrap();
        let mut fd = -1;
        let c = match connect(at, false, &mut fd) {
            Got::Done(c) => c as RawFd,
            _ => panic!("connect"),
        };
        let (mut peer, _) = l.accept().unwrap();
        // SAFETY: `c` is the connected socket, owned here.
        let mut s = unsafe { std::net::TcpStream::from_raw_fd(c) };
        let mut buf = [0u8; 4];
        assert!(matches!(
            drive(c, false, true, || s.read(&mut buf).map(|n| n as i64)),
            Got::NotReady
        ));
        let writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            peer.write_all(b"ping").unwrap();
            peer
        });
        let fd = s.as_raw_fd();
        let got = drive(fd, false, false, || s.read(&mut buf).map(|n| n as i64));
        assert!(matches!(got, Got::Done(4)));
        assert_eq!(&buf, b"ping");
        drop(writer.join().unwrap());
    }

    /// An armed connect is under way until the descriptor is writable, and
    /// `connect_finish` says so rather than reading a zero `SO_ERROR` early.
    #[test]
    fn an_armed_connect_finishes_once_writable() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut fd = -1;
        let got = connect(l.local_addr().unwrap(), true, &mut fd);
        let c = match got {
            Got::Done(c) => c as RawFd,
            Got::NotReady => {
                karac_mir_io_wait(fd, 1, -1);
                match connect_finish(fd as RawFd) {
                    Got::Done(c) => c as RawFd,
                    _ => panic!("connect_finish"),
                }
            }
            Got::Fail(e) => panic!("{e}"),
        };
        assert!(l.accept().is_ok());
        close(c);
    }

    /// A child's wait descriptor stays quiet while it runs and turns
    /// readable once it exits, before anything reaps it.
    #[test]
    fn a_child_wait_fd_turns_readable_on_exit() {
        let mut c = std::process::Command::new("sleep")
            .arg("0.2")
            .spawn()
            .unwrap();
        let fd = child_wait_fd(c.id());
        assert!(fd >= 0, "no wait descriptor on this platform");
        assert_eq!(karac_mir_io_wait(i64::from(fd), 0, 0), 1);
        assert_eq!(karac_mir_io_wait(i64::from(fd), 0, 5000), 0);
        assert!(c.try_wait().unwrap().is_some());
        close(fd);
    }

    /// A non-blocking pipe end read through `Waiting` waits for its bytes.
    #[test]
    fn a_waiting_pipe_reads_as_a_blocking_one() {
        let mut c = std::process::Command::new("sh")
            .args(["-c", "sleep 0.1; echo hi"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let out = c.stdout.take().unwrap();
        set_blocking(out.as_raw_fd(), false).unwrap();
        let mut got = String::new();
        Waiting(out).read_to_string(&mut got).unwrap();
        assert_eq!(got, "hi\n");
        c.wait().unwrap();
    }

    /// A read whose timeout passes fails with `TimedOut`, armed or not.
    #[test]
    fn a_read_past_its_timeout_times_out() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut fd = -1;
        let Got::Done(c) = connect(l.local_addr().unwrap(), false, &mut fd) else {
            panic!("connect")
        };
        let _peer = l.accept().unwrap();
        // SAFETY: `c` is the connected socket, owned here.
        let mut s = unsafe { std::net::TcpStream::from_raw_fd(c as RawFd) };
        s.set_read_timeout(Some(std::time::Duration::from_millis(30)))
            .unwrap();
        let mut buf = [0u8; 1];
        let got = drive(c as RawFd, false, true, || {
            s.read(&mut buf).map(|n| n as i64)
        });
        assert!(matches!(got, Got::Fail(e) if e.kind() == ErrorKind::TimedOut));
    }
}
