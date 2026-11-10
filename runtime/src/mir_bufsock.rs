//! `BufReader` and `BufWriter` over a connection (docs/library/io.md §
//! Buffered I/O; B-2026-11-10-14). The buffer takes the `TcpStream`'s
//! descriptor and closes it when it drops.
//!
//! A read blocks as the connection's own does ([`crate::mir_sock::drive`]),
//! so a program with no tasks reads as before. Inside a task, the readiness
//! loop first asks [`karac_mir_ready_buf`] whether the read can complete:
//! it reads what the socket holds without waiting into the reader's
//! `pending` bytes until there are some (`read`, `fill_buf`), a line
//! (`read_line`, a `LinesIter`'s `next`) or the end (`read_to_string`), and
//! otherwise parks the task on the descriptor, so the read that follows
//! never blocks the thread.

use std::collections::VecDeque;
use std::io::{Error, ErrorKind, Read, Write};
use std::os::fd::RawFd;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::mir_sock::{drive, Got};

/// A reader's connection and the bytes read ahead of the buffer.
pub(crate) struct SockState {
    fd: RawFd,
    pending: VecDeque<u8>,
    eof: bool,
    /// A failure a readiness read took off the socket, for the read.
    failed: Option<Error>,
}

impl Drop for SockState {
    fn drop(&mut self) {
        crate::mir_sock::close(self.fd);
    }
}

fn lock(s: &Mutex<SockState>) -> MutexGuard<'_, SockState> {
    s.lock().unwrap_or_else(|p| p.into_inner())
}

/// One `read(2)` of `fd` into `buf`.
fn raw_read(fd: RawFd, buf: &mut [u8]) -> std::io::Result<i64> {
    // SAFETY: `buf` is writable for its length; `fd` is open.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        Err(Error::last_os_error())
    } else {
        Ok(n as i64)
    }
}

/// The `Read` a socket's `BufReader` reads through.
pub(crate) struct SockRead(Arc<Mutex<SockState>>);

impl Read for SockRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let fd = {
            let mut s = lock(&self.0);
            if !s.pending.is_empty() {
                let n = buf.len().min(s.pending.len());
                for (d, b) in buf.iter_mut().zip(s.pending.drain(..n)) {
                    *d = b;
                }
                return Ok(n);
            }
            if let Some(e) = s.failed.take() {
                return Err(e);
            }
            if s.eof || buf.is_empty() {
                return Ok(0);
            }
            s.fd
        };
        match drive(fd, false, false, || raw_read(fd, buf)) {
            Got::Done(n) => Ok(n as usize),
            Got::Fail(e) => Err(e),
            Got::NotReady => Err(Error::from(ErrorKind::WouldBlock)),
        }
    }
}

/// The `Write` a socket's `BufWriter` writes through; it closes the
/// descriptor when the writer drops, after the writer's last flush.
pub(crate) struct SockWrite(RawFd);

impl Write for SockWrite {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let fd = self.0;
        let call = || {
            // SAFETY: `buf` is readable for its length; `fd` is open.
            let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
            if n < 0 {
                Err(Error::last_os_error())
            } else {
                Ok(n as i64)
            }
        };
        match drive(fd, true, false, call) {
            Got::Done(n) => Ok(n as usize),
            Got::Fail(e) => Err(e),
            Got::NotReady => Err(Error::from(ErrorKind::WouldBlock)),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for SockWrite {
    fn drop(&mut self) {
        crate::mir_sock::close(self.0);
    }
}

/// A reader over the connection `fd`, and the state its readiness reads.
pub(crate) fn reader(fd: RawFd) -> (SockRead, Arc<dyn std::any::Any + Send + Sync>) {
    let s = Arc::new(Mutex::new(SockState {
        fd,
        pending: VecDeque::new(),
        eof: false,
        failed: None,
    }));
    (SockRead(s.clone()), s)
}

/// A writer over the connection `fd`.
pub(crate) fn writer(fd: RawFd) -> SockWrite {
    SockWrite(fd)
}

/// Whether a read of the reader whose state is `state`, with `buffered`
/// bytes in its buffer, can complete now: `need` 0 for some bytes, 1 for a
/// line, 2 for the end. Reads what the socket holds without waiting; the
/// task parks on the descriptor (by its read timeout) when that is not
/// enough.
pub(crate) fn ready(state: &(dyn std::any::Any + Send + Sync), buffered: &[u8], need: u8) -> u8 {
    let Some(state) = state.downcast_ref::<Mutex<SockState>>() else {
        return crate::mir_task::karac_mir_ready(1);
    };
    let mut s = lock(state);
    let fd = s.fd;
    let ready = loop {
        let enough = s.eof
            || s.failed.is_some()
            || match need {
                0 => !buffered.is_empty() || !s.pending.is_empty(),
                1 => buffered.contains(&b'\n') || s.pending.contains(&b'\n'),
                _ => false,
            };
        if enough {
            break true;
        }
        let mut chunk = [0u8; 4096];
        match raw_read(fd, &mut chunk) {
            Ok(0) => s.eof = true,
            Ok(n) => s.pending.extend(&chunk[..n as usize]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => break false,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => s.failed = Some(e),
        }
    };
    drop(s);
    crate::mir_task::ready_read(fd, ready)
}
