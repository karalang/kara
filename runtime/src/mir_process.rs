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

/// A pipe taken off a child. A closed stdin is `In(None)`.
enum Pipe {
    Out(ChildStdout),
    Err(ChildStderr),
    In(Option<ChildStdin>),
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
        StdioCfg::Out(o) => Stdio::from(o.take().ok_or_else(given)?),
        StdioCfg::Err(e) => Stdio::from(e.take().ok_or_else(given)?),
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
    let res = cmd.spawn().and_then(|child| {
        if output == 0 {
            return Ok(Box::into_raw(Box::new(Some(child))) as i64);
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
/// and taking its stdin (5), stdout (6) or stderr (7) pipe, a handle in
/// `*value` or 0.
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
    let slot = unsafe { &mut *handle.cast::<Option<Child>>() };
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
            child.wait().map(pack)
        }
        1 => child.try_wait().map(|s| s.map_or(0, |s| pack(s) | 1)),
        2 => child.kill().map(|()| 0),
        3 => {
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
        _ => Ok(pipe(child.stderr.take().map(Pipe::Err))),
    };
    // SAFETY: the caller's contract.
    unsafe { report(res, value, msg) }
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
        drop(unsafe { Box::from_raw(handle.cast::<Option<Child>>()) });
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
    let res: std::io::Result<i64> = match pipe {
        // SAFETY: the caller's contract.
        Pipe::Out(o) if op <= 2 => unsafe { read_op(o, op, ptr, n) },
        // SAFETY: the caller's contract.
        Pipe::Err(e) if op <= 2 => unsafe { read_op(e, op, ptr, n) },
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

/// A read pipe's `read` (op 0), `read_to_end` (1) or `read_to_string` (2),
/// as [`karac_mir_pipe`] has them.
///
/// # Safety
/// As [`karac_mir_pipe`].
unsafe fn read_op(r: &mut dyn Read, op: i32, ptr: *mut u8, n: usize) -> std::io::Result<i64> {
    if op == 0 {
        if ptr.is_null() || n == 0 {
            return r.read(&mut []).map(|k| k as i64);
        }
        // SAFETY: the caller's contract.
        return r
            .read(unsafe { std::slice::from_raw_parts_mut(ptr, n) })
            .map(|k| k as i64);
    }
    let mut b = Vec::new();
    let k = r.read_to_end(&mut b)?;
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
        // SAFETY: the caller's contract.
        drop(unsafe { Box::from_raw(handle.cast::<Pipe>()) });
    }
}
