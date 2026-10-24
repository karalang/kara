//! `std.process` for the MIR backend (B-2026-10-15-6, docs/library/io.md
//! § Processes). A `Stdio`, a child and each pipe taken off it are boxes
//! behind one-word handles, which compiled code drops through the `_free`
//! entries. The reference semantics are the MIR interpreter's
//! `process_method` (`src/mir/interp.rs`), which keeps the same state in
//! tables. Results report as `karac_mir_fs` does ([`crate::mir_io`]).

use std::io::{Read, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout};

use crate::mir::RawVec;
use crate::mir_io::{io_error_code, push_bytes, string_of};

/// What a `Stdio` connects a stream to. A pipe goes to the first child
/// spawned with it.
enum StdioCfg {
    Inherit,
    Null,
    Piped,
    File(std::fs::File),
    Out(Option<ChildStdout>),
    Err(Option<ChildStderr>),
}

/// A spawned child, and the descriptor its `wait` readiness loop waits on
/// ([`karac_mir_child`] op 8): -2 until asked, -1 when there is none.
/// `child` is `None` once `wait_with_output` took it.
struct Kid {
    child: Option<Child>,
    wait_fd: i32,
}

impl Drop for Kid {
    fn drop(&mut self) {
        #[cfg(all(feature = "net", unix))]
        if self.wait_fd >= 0 {
            crate::mir_sock::close(self.wait_fd);
        }
    }
}

/// A pipe taken off a child. A closed stdin is `In(None)`.
enum Pipe {
    Out(ChildStdout),
    Err(ChildStderr),
    In(Option<ChildStdin>),
}

/// A closed stdin pipe given to a `BufWriter`: every write fails, as a
/// write to the pipe itself does.
struct ClosedPipe;

impl Write for ClosedPipe {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from(std::io::ErrorKind::InvalidInput))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::InvalidInput))
    }
}

/// The stream of the pipe at `handle`, which this takes (B-2026-10-15-44:
/// `BufReader.new(child.stdout().unwrap())`): a stdout or stderr pipe as a
/// reader, a stdin pipe as a writer.
///
/// # Safety
/// `handle` came from [`karac_mir_child`] and is not used again.
pub(crate) unsafe fn take_pipe(
    handle: *mut std::ffi::c_void,
) -> Result<Box<dyn Read>, Box<dyn Write>> {
    // SAFETY: the caller's contract.
    match *unsafe { Box::from_raw(handle.cast::<Pipe>()) } {
        Pipe::Out(o) => Ok(waiting(o)),
        Pipe::Err(e) => Ok(waiting(e)),
        Pipe::In(Some(w)) => Err(waiting_out(w)),
        Pipe::In(None) => Err(Box::new(ClosedPipe)),
    }
}

/// A read pipe end as a blocking stream: the descriptor is non-blocking
/// (the reactor contract, `mir_task.rs`), so a read that would block
/// waits for it.
fn waiting<T: Read + 'static + WaitFd>(r: T) -> Box<dyn Read> {
    #[cfg(all(feature = "net", unix))]
    return Box::new(crate::mir_sock::Waiting(r));
    #[cfg(not(all(feature = "net", unix)))]
    Box::new(r)
}

/// [`waiting`] for the write end.
fn waiting_out<T: Write + 'static + WaitFd>(w: T) -> Box<dyn Write> {
    #[cfg(all(feature = "net", unix))]
    return Box::new(crate::mir_sock::Waiting(w));
    #[cfg(not(all(feature = "net", unix)))]
    Box::new(w)
}

/// A pipe end with a descriptor to wait on, where there is a reactor.
#[cfg(all(feature = "net", unix))]
trait WaitFd: std::os::fd::AsFd {}
#[cfg(all(feature = "net", unix))]
impl<T: std::os::fd::AsFd> WaitFd for T {}
#[cfg(not(all(feature = "net", unix)))]
trait WaitFd {}
#[cfg(not(all(feature = "net", unix)))]
impl<T> WaitFd for T {}

/// Makes a spawned child's pipe ends non-blocking, as the reactor
/// contract has every descriptor the natives hand out.
#[cfg(all(feature = "net", unix))]
fn nonblocking_pipes(c: &Child) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let fds = [
        c.stdin.as_ref().map(|p| p.as_raw_fd()),
        c.stdout.as_ref().map(|p| p.as_raw_fd()),
        c.stderr.as_ref().map(|p| p.as_raw_fd()),
    ];
    for fd in fds.into_iter().flatten() {
        crate::mir_sock::set_blocking(fd, false)?;
    }
    Ok(())
}

/// Makes a child's pipe ends blocking again, for std's own reads.
#[cfg(all(feature = "net", unix))]
fn blocking_pipes(c: &Child) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let fds = [
        c.stdout.as_ref().map(|p| p.as_raw_fd()),
        c.stderr.as_ref().map(|p| p.as_raw_fd()),
    ];
    for fd in fds.into_iter().flatten() {
        crate::mir_sock::set_blocking(fd, true)?;
    }
    Ok(())
}

/// A pipe given to another child as its stream blocks again: the two
/// ends share one open file description, and the child expects a
/// blocking one.
#[cfg(all(feature = "net", unix))]
fn blocking<T: std::os::fd::AsRawFd>(p: T) -> std::io::Result<T> {
    crate::mir_sock::set_blocking(p.as_raw_fd(), true)?;
    Ok(p)
}
#[cfg(not(all(feature = "net", unix)))]
fn blocking<T>(p: T) -> std::io::Result<T> {
    Ok(p)
}

/// An `EnvVar` as compiled code lays it out: `op` 0 sets `key`, 1 removes
/// it, 2 clears the environment.
#[repr(C)]
pub struct EnvVar {
    key: RawVec,
    value: RawVec,
    op: i64,
}

/// The text of a `String`.
///
/// # Safety
/// `s` is a live `String`.
unsafe fn text(s: *const RawVec) -> String {
    // SAFETY: the caller's contract.
    let s = unsafe { &*s };
    if s.ptr.is_null() || s.len == 0 {
        return String::new();
    }
    // SAFETY: a live `String` holds `len` bytes.
    let b = unsafe { std::slice::from_raw_parts(s.ptr, s.len) };
    String::from_utf8_lossy(b).into_owned()
}

/// `ExitStatus` packed into a word: `(code << 16) | (signal << 2) |
/// (exited << 1)`, the low bit left for `try_wait`'s "present".
fn pack(s: std::process::ExitStatus) -> i64 {
    #[cfg(unix)]
    let (exited, code, signal) = {
        use std::os::unix::process::ExitStatusExt;
        match s.code() {
            Some(c) => (1, i64::from(c), 0),
            None => (0, -1, i64::from(s.signal().unwrap_or(0))),
        }
    };
    #[cfg(not(unix))]
    let (exited, code, signal) = (1, i64::from(s.code().unwrap_or(-1)), 0);
    (code << 16) | ((signal & 0x3fff) << 2) | (exited << 1)
}

/// The result of `f`: success writes its value and returns -1.
///
/// # Safety
/// `value` is writable for an `i64` and `msg` for a `String`.
unsafe fn report(r: std::io::Result<i64>, value: *mut i64, msg: *mut RawVec) -> i32 {
    match r {
        Ok(v) => {
            // SAFETY: the caller's contract.
            unsafe { value.write(v) };
            -1
        }
        // SAFETY: the caller's contract.
        Err(e) => unsafe { io_error_code(&e, msg) },
    }
}

/// A new `Stdio`: `kind` 0 inherit, 1 null, 2 piped, 3 the `File` at
/// `handle`, 4 the `ChildStdout` or `ChildStderr` pipe at `handle`. It
/// takes the handle.
///
/// # Safety
/// `handle` is a live handle of the kind's type for kinds 3 and 4.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_stdio(
    kind: i32,
    handle: *mut std::ffi::c_void,
) -> *mut std::ffi::c_void {
    let cfg = match kind {
        0 => StdioCfg::Inherit,
        1 => StdioCfg::Null,
        2 => StdioCfg::Piped,
        // SAFETY: the caller's contract.
        3 => StdioCfg::File(unsafe { crate::file::take_file(handle.cast()) }),
        // SAFETY: the caller's contract.
        _ => match *unsafe { Box::from_raw(handle.cast::<Pipe>()) } {
            Pipe::Out(o) => StdioCfg::Out(Some(o)),
            Pipe::Err(e) => StdioCfg::Err(Some(e)),
            Pipe::In(_) => StdioCfg::Null,
        },
    };
    Box::into_raw(Box::new(cfg)).cast()
}

/// Drops a `Stdio`.
///
/// # Safety
/// `handle` came from [`karac_mir_stdio`] and is dropped once.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_stdio_free(handle: *mut std::ffi::c_void) {
    if !handle.is_null() {
        // SAFETY: the caller's contract.
        drop(unsafe { Box::from_raw(handle.cast::<StdioCfg>()) });
    }
}

/// The `std::process::Stdio` the `Stdio` at `handle` stands for.
///
/// # Safety
/// `handle` is a live `Stdio`.
unsafe fn to_std(handle: *mut std::ffi::c_void) -> std::io::Result<std::process::Stdio> {
    use std::process::Stdio;
    let given = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "this pipe was already given to a child",
        )
    };
    // SAFETY: the caller's contract.
    Ok(match unsafe { &mut *handle.cast::<StdioCfg>() } {
        StdioCfg::Inherit => Stdio::inherit(),
        StdioCfg::Null => Stdio::null(),
        StdioCfg::Piped => Stdio::piped(),
        StdioCfg::File(f) => Stdio::from(f.try_clone()?),
        StdioCfg::Out(o) => Stdio::from(blocking(o.take().ok_or_else(given)?)?),
        StdioCfg::Err(e) => Stdio::from(blocking(e.take().ok_or_else(given)?)?),
    })
}

/// `Command.spawn` (`output` 0) and `Command.output` (1): the program, its
/// `args_len` arguments at `args`, its `env_len` environment changes at
/// `env`, its working directory (`dir`, null for the parent's) and the
/// `Stdio` of each stream (null when unset: inherited for `spawn`; for
/// `output`, stdout and stderr piped and stdin the null device). `spawn`
/// leaves the child's handle in `*value`; `output` leaves the packed status
/// there and the bytes the child wrote at `out` and `err`.
///
/// # Safety
/// Each pointer is valid for what it holds; `out` and `err` are writable
/// for a `String` each when `output` is 1; `value` and `msg` as
/// `karac_mir_fs` has them.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_command(
    prog: *const RawVec,
    args: *const RawVec,
    args_len: i64,
    env: *const EnvVar,
    env_len: i64,
    dir: *const RawVec,
    stdio: *const *mut std::ffi::c_void,
    output: i32,
    value: *mut i64,
    out: *mut RawVec,
    err: *mut RawVec,
    msg: *mut RawVec,
) -> i32 {
    // SAFETY: the caller's contract.
    let mut cmd = std::process::Command::new(unsafe { text(prog) });
    for i in 0..usize::try_from(args_len).unwrap_or(0) {
        // SAFETY: `args` holds `args_len` `String`s.
        cmd.arg(unsafe { text(args.add(i)) });
    }
    for i in 0..usize::try_from(env_len).unwrap_or(0) {
        // SAFETY: `env` holds `env_len` `EnvVar`s.
        let e = unsafe { &*env.add(i) };
        // SAFETY: an `EnvVar`'s fields are live `String`s.
        let (k, v) = unsafe { (text(&e.key), text(&e.value)) };
        match e.op {
            0 => cmd.env(k, v),
            1 => cmd.env_remove(k),
            _ => cmd.env_clear(),
        };
    }
    if !dir.is_null() {
        // SAFETY: the caller's contract.
        cmd.current_dir(unsafe { text(dir) });
    }
    for i in 0..3 {
        // SAFETY: `stdio` holds three handles.
        let h = unsafe { *stdio.add(i) };
        let cfg = if !h.is_null() {
            // SAFETY: a non-null entry is a live `Stdio`.
            match unsafe { to_std(h) } {
                Ok(s) => s,
                // SAFETY: the caller's contract.
                Err(e) => return unsafe { io_error_code(&e, msg) },
            }
        } else if output == 0 {
            continue;
        } else if i == 0 {
            std::process::Stdio::null()
        } else {
            std::process::Stdio::piped()
        };
        match i {
            0 => cmd.stdin(cfg),
            1 => cmd.stdout(cfg),
            _ => cmd.stderr(cfg),
        };
    }
    // The child writes to the same streams, so what the program printed
    // before it goes out first (docs/library/io.md § Buffering).
    crate::mir::out_flush();
    let res = cmd.spawn().and_then(|child| {
        if output == 0 {
            #[cfg(all(feature = "net", unix))]
            nonblocking_pipes(&child)?;
            let kid = Kid {
                child: Some(child),
                wait_fd: -2,
            };
            return Ok(Box::into_raw(Box::new(kid)) as i64);
        }
        let o = child.wait_with_output()?;
        // SAFETY: the caller's contract.
        unsafe {
            string_of(out, &o.stdout);
            string_of(err, &o.stderr);
        }
        Ok(pack(o.status))
    });
    // SAFETY: the caller's contract.
    unsafe { report(res, value, msg) }
}

/// A `Child`'s methods on the child at `handle`: `wait` (op 0) and
/// `try_wait` (1), the status packed into `*value` (for `try_wait`, with
/// its low bit set when the child has ended); `kill` (2);
/// `wait_with_output` (3), as `output` in [`karac_mir_command`]; `id` (4);
/// taking its stdin (5), stdout (6) or stderr (7) pipe, a handle in
/// `*value` or 0; and the descriptor `wait`'s readiness loop waits
/// readable on (8, -1 for none). Armed (`mir_sock.rs`), `wait` answers
/// `IO_NOT_READY` while the child runs.
///
/// # Safety
/// `handle` is a live child; `out` and `err` as in [`karac_mir_command`]
/// for op 3; `value` and `msg` as `karac_mir_fs` has them.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_child(
    op: i32,
    handle: *mut std::ffi::c_void,
    value: *mut i64,
    out: *mut RawVec,
    err: *mut RawVec,
    msg: *mut RawVec,
) -> i32 {
    // SAFETY: the caller's contract.
    let kid = unsafe { &mut *handle.cast::<Kid>() };
    #[cfg(all(feature = "net", unix))]
    let armed = crate::mir_sock::take_armed();
    #[cfg(not(all(feature = "net", unix)))]
    let armed = false;
    let slot = &mut kid.child;
    let Some(child) = slot.as_mut() else {
        let gone = std::io::Error::other("the child was already waited for with its output");
        // SAFETY: the caller's contract.
        return unsafe { io_error_code(&gone, msg) };
    };
    let pipe = |p: Option<Pipe>| -> i64 { p.map_or(0, |p| Box::into_raw(Box::new(p)) as i64) };
    let res = match op {
        0 => {
            // A held stdin closes first, so the child can see the end of
            // its input.
            child.stdin = None;
            if armed {
                match child.try_wait() {
                    Ok(None) => {
                        // SAFETY: the caller's contract.
                        unsafe { *value = 0 };
                        crate::mir_task::set_again();
                        return NOT_READY;
                    }
                    r => r.map(|s| pack(s.expect("matched above"))),
                }
            } else {
                child.wait().map(pack)
            }
        }
        1 => child.try_wait().map(|s| s.map_or(0, |s| pack(s) | 1)),
        2 => child.kill().map(|()| 0),
        3 => {
            // std reads a lone pipe with a plain `read_to_end`, which a
            // non-blocking end would fail with `WouldBlock`.
            #[cfg(all(feature = "net", unix))]
            if let Err(e) = blocking_pipes(child) {
                // SAFETY: the caller's contract.
                return unsafe { io_error_code(&e, msg) };
            }
            let child = slot.take().expect("checked above");
            child.wait_with_output().map(|o| {
                // SAFETY: the caller's contract.
                unsafe {
                    string_of(out, &o.stdout);
                    string_of(err, &o.stderr);
                }
                pack(o.status)
            })
        }
        4 => Ok(i64::from(child.id())),
        5 => Ok(pipe(child.stdin.take().map(|p| Pipe::In(Some(p))))),
        6 => Ok(pipe(child.stdout.take().map(Pipe::Out))),
        7 => Ok(pipe(child.stderr.take().map(Pipe::Err))),
        _ => Ok(wait_fd(kid)),
    };
    // SAFETY: the caller's contract.
    unsafe { report(res, value, msg) }
}

/// The descriptor that turns readable once `kid` exits, opened on first
/// ask: -1 when there is none, or once `wait_with_output` took the child.
fn wait_fd(kid: &mut Kid) -> i64 {
    if kid.wait_fd == -2 {
        #[cfg(all(feature = "net", unix))]
        let fd = kid
            .child
            .as_ref()
            .map_or(-1, |c| crate::mir_sock::child_wait_fd(c.id()));
        #[cfg(not(all(feature = "net", unix)))]
        let fd = -1;
        kid.wait_fd = fd;
    }
    i64::from(kid.wait_fd)
}

/// `Child.wait`'s readiness: the descriptor of [`karac_mir_child`]'s op 8,
/// for `__ready` to wait readable on.
///
/// # Safety
/// `handle` is a live child.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_child_wait_fd(handle: *mut std::ffi::c_void) -> i64 {
    // SAFETY: the caller's contract.
    let kid = unsafe { &mut *handle.cast::<Kid>() };
    // The `wait` this guards closes a held stdin first; so does its
    // readiness, or a child reading to the end of its input never exits.
    if let Some(c) = kid.child.as_mut() {
        c.stdin = None;
    }
    wait_fd(kid)
}

/// Drops a child: it neither waits for nor kills the process, and closes
/// the pipes not taken off it.
///
/// # Safety
/// `handle` came from [`karac_mir_command`] and is dropped once.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_child_free(handle: *mut std::ffi::c_void) {
    if !handle.is_null() {
        // SAFETY: the caller's contract.
        drop(unsafe { Box::from_raw(handle.cast::<Kid>()) });
    }
}

/// A pipe's methods on the pipe at `handle`: `read` (op 0) into
/// `ptr[..len]`; `read_to_end` (1) and `read_to_string` (2), appending to
/// the `Vec[u8]` or `String` at `ptr` (nothing, for text that is not
/// UTF-8); `write` (3) and `write_all` (4) of `ptr[..len]`; `flush` (5);
/// `close` (6). A count lands in `*value`.
///
/// # Safety
/// `handle` is a live pipe; `ptr` holds what its op reads or writes;
/// `value` and `msg` as `karac_mir_fs` has them.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pipe(
    op: i32,
    handle: *mut std::ffi::c_void,
    ptr: *mut u8,
    len: i64,
    value: *mut i64,
    msg: *mut RawVec,
) -> i32 {
    // SAFETY: the caller's contract.
    let pipe = unsafe { &mut *handle.cast::<Pipe>() };
    let n = usize::try_from(len).unwrap_or(0);
    // What a waiting `read_to_end`'s readiness already read comes first.
    let mut ahead = if op == 1 || op == 2 {
        take_drained(handle)
    } else {
        Vec::new()
    };
    #[cfg(all(feature = "net", unix))]
    {
        // SAFETY: the caller's contract.
        if let Some(code) = unsafe { pipe_unix(pipe, op, ptr, n, &mut ahead, value, msg) } {
            return code;
        }
    }
    let res: std::io::Result<i64> = match pipe {
        // SAFETY: the caller's contract.
        Pipe::Out(o) if op <= 2 => unsafe { read_op(o, op, ptr, n, &mut ahead) },
        // SAFETY: the caller's contract.
        Pipe::Err(e) if op <= 2 => unsafe { read_op(e, op, ptr, n, &mut ahead) },
        Pipe::In(w) if op == 6 => {
            *w = None;
            Ok(0)
        }
        Pipe::In(w) if (3..=5).contains(&op) => match w.as_mut() {
            None => Err(std::io::Error::from(std::io::ErrorKind::InvalidInput)),
            Some(w) if op == 5 => w.flush().map(|()| 0),
            Some(w) => {
                let b = if ptr.is_null() || n == 0 {
                    &[][..]
                } else {
                    // SAFETY: the caller's contract.
                    unsafe { std::slice::from_raw_parts(ptr, n) }
                };
                if op == 3 {
                    w.write(b).map(|k| k as i64)
                } else {
                    w.write_all(b).map(|()| 0)
                }
            }
        },
        _ => Err(std::io::Error::other("a pipe of the other direction")),
    };
    // SAFETY: the caller's contract.
    unsafe { report(res, value, msg) }
}

/// [`karac_mir_pipe`]'s calls on a non-blocking pipe end, under the
/// reactor contract (`mir_sock.rs`): `read` (0) and `write` (3) answer
/// `IO_NOT_READY` when armed and they would block, and every other call
/// waits for the descriptor as it must. `None` leaves a closed stdin and
/// `close` to the caller.
///
/// # Safety
/// As [`karac_mir_pipe`].
#[cfg(all(feature = "net", unix))]
unsafe fn pipe_unix(
    pipe: &mut Pipe,
    op: i32,
    ptr: *mut u8,
    n: usize,
    ahead: &mut Vec<u8>,
    value: *mut i64,
    msg: *mut RawVec,
) -> Option<i32> {
    use crate::mir_sock::{self as sock, Got, Waiting};
    use std::os::fd::AsRawFd;
    let armed = sock::take_armed();
    // SAFETY: the caller's contract.
    let buf = || unsafe {
        if ptr.is_null() || n == 0 {
            &mut [][..]
        } else {
            std::slice::from_raw_parts_mut(ptr, n)
        }
    };
    let got = match pipe {
        Pipe::Out(o) if op == 0 => sock::drive_for(o.as_raw_fd(), false, armed, -1, || {
            o.read(buf()).map(|k| k as i64)
        }),
        Pipe::Err(e) if op == 0 => sock::drive_for(e.as_raw_fd(), false, armed, -1, || {
            e.read(buf()).map(|k| k as i64)
        }),
        // SAFETY: the caller's contract.
        Pipe::Out(o) if op <= 2 => match unsafe { read_op(&mut Waiting(o), op, ptr, n, ahead) } {
            Ok(k) => Got::Done(k),
            Err(e) => Got::Fail(e),
        },
        // SAFETY: the caller's contract.
        Pipe::Err(e) if op <= 2 => match unsafe { read_op(&mut Waiting(e), op, ptr, n, ahead) } {
            Ok(k) => Got::Done(k),
            Err(e) => Got::Fail(e),
        },
        Pipe::In(Some(w)) if op == 3 => sock::drive_for(w.as_raw_fd(), true, armed, -1, || {
            w.write(buf()).map(|k| k as i64)
        }),
        Pipe::In(Some(w)) if op == 4 => match Waiting(w).write_all(buf()) {
            Ok(()) => Got::Done(0),
            Err(e) => Got::Fail(e),
        },
        _ => return None,
    };
    let res = match got {
        Got::Done(k) => Ok(k),
        Got::NotReady => {
            // SAFETY: the caller's contract.
            unsafe { *value = 0 };
            return Some(NOT_READY);
        }
        Got::Fail(e) => Err(e),
    };
    // SAFETY: the caller's contract.
    Some(unsafe { report(res, value, msg) })
}

/// What a waiting `read_to_end`'s `__ready` drained off a pipe, by the
/// pipe's handle, which that `read_to_end` returns ahead of the rest.
static DRAINED: std::sync::Mutex<Vec<(usize, Vec<u8>)>> = std::sync::Mutex::new(Vec::new());

/// The bytes [`karac_mir_ready_pipe`] drained off the pipe at `handle`.
fn take_drained(handle: *mut std::ffi::c_void) -> Vec<u8> {
    let mut d = DRAINED.lock().unwrap_or_else(|e| e.into_inner());
    match d.iter().position(|(h, _)| *h == handle as usize) {
        Some(i) => d.swap_remove(i).1,
        None => Vec::new(),
    }
}

/// `__ready` of a pipe's `read` (`drain` 0): whether it has bytes or its
/// end to read, as `karac_mir_ready_fd` asks. Of its `read_to_end` /
/// `read_to_string` (`drain` 1): whether it reached its end, reading what
/// comes before it into [`DRAINED`] as it arrives. Off the reactor, and
/// for a stdin pipe, every call is ready.
///
/// # Safety
/// `handle` is a live pipe.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_ready_pipe(handle: *mut std::ffi::c_void, drain: u8) -> u8 {
    #[cfg(all(feature = "net", unix))]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: the caller's contract.
        let pipe = unsafe { &mut *handle.cast::<Pipe>() };
        let fd = match pipe {
            Pipe::Out(o) => o.as_raw_fd(),
            Pipe::Err(e) => e.as_raw_fd(),
            Pipe::In(_) => return crate::mir_task::karac_mir_ready(1),
        };
        loop {
            let ready = crate::mir_task::karac_mir_ready_fd(i64::from(fd), 0);
            if ready == 0 || drain == 0 {
                return ready;
            }
            let mut chunk = [0u8; 64 * 1024];
            // SAFETY: as above; the descriptor is non-blocking.
            let pipe = unsafe { &mut *handle.cast::<Pipe>() };
            let got = match pipe {
                Pipe::Out(o) => o.read(&mut chunk),
                Pipe::Err(e) => e.read(&mut chunk),
                Pipe::In(_) => unreachable!("answered above"),
            };
            match got {
                Ok(n) if n > 0 => {
                    let mut d = DRAINED.lock().unwrap_or_else(|e| e.into_inner());
                    match d.iter_mut().find(|(h, _)| *h == handle as usize) {
                        Some((_, b)) => b.extend_from_slice(&chunk[..n]),
                        None => d.push((handle as usize, chunk[..n].to_vec())),
                    }
                }
                // Off the reactor every answer is ready, so nothing to read
                // yet leaves the wait to the call itself.
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                _ => return ready,
            }
        }
    }
    #[cfg(not(all(feature = "net", unix)))]
    {
        let _ = (handle, drain);
        crate::mir_task::karac_mir_ready(1)
    }
}

/// The status of a call that would block, armed: `IO_NOT_READY`.
#[cfg(all(feature = "net", unix))]
const NOT_READY: i32 = crate::mir_sock::IO_NOT_READY;
#[cfg(not(all(feature = "net", unix)))]
const NOT_READY: i32 = 100;

/// A read pipe's `read` (op 0), `read_to_end` (1) or `read_to_string` (2),
/// as [`karac_mir_pipe`] has them.
///
/// # Safety
/// As [`karac_mir_pipe`].
unsafe fn read_op(
    r: &mut dyn Read,
    op: i32,
    ptr: *mut u8,
    n: usize,
    ahead: &mut Vec<u8>,
) -> std::io::Result<i64> {
    if op == 0 {
        if ptr.is_null() || n == 0 {
            return r.read(&mut []).map(|k| k as i64);
        }
        // SAFETY: the caller's contract.
        return r
            .read(unsafe { std::slice::from_raw_parts_mut(ptr, n) })
            .map(|k| k as i64);
    }
    let mut b = std::mem::take(ahead);
    let k = b.len() + r.read_to_end(&mut b)?;
    if op == 2 {
        std::str::from_utf8(&b)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    }
    // SAFETY: the caller's contract.
    unsafe { push_bytes(ptr.cast(), &b) };
    Ok(k as i64)
}

/// Drops a pipe, closing it.
///
/// # Safety
/// `handle` came from [`karac_mir_child`] and is dropped once.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_pipe_free(handle: *mut std::ffi::c_void) {
    if !handle.is_null() {
        drop(take_drained(handle));
        // SAFETY: the caller's contract.
        drop(unsafe { Box::from_raw(handle.cast::<Pipe>()) });
    }
}
