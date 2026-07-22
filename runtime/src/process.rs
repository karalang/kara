//! `std.process` runtime shim — Command/Child spawning over
//! `std::process` (phase-8 P1, codegen leg).
//!
//! Companion to `runtime/src/file.rs`'s shape: every fallible entry
//! point writes a `KaracIoResult` into a caller-provided out-param
//! (same 32-byte ABI, same IoError kind-tag table), so codegen reuses
//! the shared `lower_kara_io_result` unpacker. The interpreter twin is
//! `src/interpreter/method_call_process.rs` — semantics here must stay
//! in lockstep with it (pid-keyed tables, reap-on-wait, stream taken
//! at most once).
//!
//! ## Tables
//!
//! `CHILD_TABLE` maps OS pid → live `std::process::Child`; `spawn`
//! inserts, `wait` removes up front (reap), `try_wait` removes once the
//! child has exited, `kill` leaves the entry (caller still waits to
//! reap). The three `*_STREAM_TABLE`s hold captured pipe handles moved
//! off the `Child` by `take_stream`; `read_to_string` / `close` remove
//! them. Pid keys are safe for the same reason as the interpreter: the
//! OS won't reuse a pid while the child is un-reaped in our table.
//! Mutex (not thread-local): a `Child` handle is a plain copyable Kāra
//! struct and may cross task/thread boundaries.
//!
//! ## Packed Ok payloads
//!
//! `wait` / `try_wait` return multi-field Ok payloads through the
//! single `KaracIoResult::value` word, bit-packed (codegen decodes in
//! `FileOkKind::{ExitStatusPacked, OptionExitStatusPacked}`):
//!
//!   - `wait`:      `value = (code << 1) | success`
//!   - `try_wait`:  `value = (code << 2) | (success << 1) | present`
//!     (`present == 0` ⇒ Ok(None), remaining bits zero)
//!
//! `code` is the i32 exit code sign-extended (or `-1` when the child
//! was signal-killed), so the shifts never overflow i64.
//!
//! ## String views
//!
//! Kāra `String` arguments arrive as `*const RuntimeKaracString`
//! (a pointer to the caller's 24-byte descriptor, NOT a raw
//! `(ptr, len)` pair) and are decoded through the SSO-aware
//! `RuntimeKaracString::as_bytes`, so the surface stays correct when
//! inline-string construction switches on. `Vec[String]` /
//! `Vec[EnvVar]` arguments arrive as the Vec's raw data pointer +
//! element count; the runtime strides the buffer natively
//! (`RuntimeKaracString` is 24 bytes, `KaracEnvVarView` 48 — both
//! layout-pinned by the Kāra `{ptr, len, cap}` ABI).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{LazyLock, Mutex};

use crate::file::{err, ok, ok_string, KaracIoResult};
use crate::RuntimeKaracString;

static CHILD_TABLE: LazyLock<Mutex<HashMap<i64, std::process::Child>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static STDOUT_TABLE: LazyLock<Mutex<HashMap<i64, std::process::ChildStdout>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static STDERR_TABLE: LazyLock<Mutex<HashMap<i64, std::process::ChildStderr>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static STDIN_TABLE: LazyLock<Mutex<HashMap<i64, std::process::ChildStdin>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Layout-compatible view of the Kāra `EnvVar { key: String, value:
/// String }` struct as it sits inside a `Vec[EnvVar]` buffer — two
/// consecutive 24-byte String descriptors. All fields are 8-byte
/// aligned words, so there is no padding to model.
#[repr(C)]
pub struct KaracEnvVarView {
    pub key: RuntimeKaracString,
    pub value: RuntimeKaracString,
}

/// `IoError.NotFound` result — the "no such child / handle" answer,
/// matching the interpreter's `io_not_found()`.
fn not_found() -> KaracIoResult {
    KaracIoResult {
        value: 0,
        error_kind: 1,
        _pad: 0,
        error_msg_ptr: std::ptr::null_mut(),
        error_msg_len: 0,
    }
}

/// Decode a `Stdio` enum tag (declaration order: Inherit=0, Null=1,
/// Piped=2) to the `std::process::Stdio` to apply, or `None` to leave
/// the stream at `std::process`'s own default (inherit) — the same
/// only-act-on-Null/Piped rule as the interpreter's `stdio_for_field`.
fn stdio_from_tag(tag: i64) -> Option<std::process::Stdio> {
    match tag {
        1 => Some(std::process::Stdio::null()),
        2 => Some(std::process::Stdio::piped()),
        _ => None,
    }
}

/// SSO-aware `&str` view of a String descriptor pointer. Null pointer
/// or invalid UTF-8 read as the empty string (matching the
/// interpreter's lenient field readers — the typechecker guarantees
/// UTF-8 Strings, so the fallback is belt-and-braces).
unsafe fn str_view<'a>(s: *const RuntimeKaracString) -> &'a str {
    if s.is_null() {
        return "";
    }
    std::str::from_utf8((*s).as_bytes()).unwrap_or("")
}

/// Pack an `ExitStatus` into the `wait` value word.
fn pack_exit_status(status: std::process::ExitStatus) -> i64 {
    let code = status.code().unwrap_or(-1) as i64;
    let success = status.success() as i64;
    (code << 1) | success
}

/// Spawn a child process. Ok payload: `value` = OS pid (also the key
/// for every subsequent `Child` operation).
///
/// # Safety
///
/// `prog` must point at a live String descriptor; `args_ptr` /
/// `env_ptr` must point at `args_len` / `env_len` contiguous elements
/// of the indicated view types (or be null with count 0). All borrows
/// are read-only and end before return.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_process_spawn(
    out: *mut KaracIoResult,
    prog: *const RuntimeKaracString,
    args_ptr: *const RuntimeKaracString,
    args_len: i64,
    env_ptr: *const KaracEnvVarView,
    env_len: i64,
    stdin_tag: i64,
    stdout_tag: i64,
    stderr_tag: i64,
) {
    let mut cmd = std::process::Command::new(str_view(prog));
    if !args_ptr.is_null() {
        for i in 0..args_len.max(0) as usize {
            cmd.arg(str_view(args_ptr.add(i)));
        }
    }
    if !env_ptr.is_null() {
        for i in 0..env_len.max(0) as usize {
            let ev = &*env_ptr.add(i);
            let key = std::str::from_utf8(ev.key.as_bytes()).unwrap_or("");
            if key.is_empty() {
                continue;
            }
            cmd.env(key, std::str::from_utf8(ev.value.as_bytes()).unwrap_or(""));
        }
    }
    if let Some(cfg) = stdio_from_tag(stdin_tag) {
        cmd.stdin(cfg);
    }
    if let Some(cfg) = stdio_from_tag(stdout_tag) {
        cmd.stdout(cfg);
    }
    if let Some(cfg) = stdio_from_tag(stderr_tag) {
        cmd.stderr(cfg);
    }
    let result = match cmd.spawn() {
        Ok(child) => {
            let pid = child.id() as i64;
            if let Ok(mut table) = CHILD_TABLE.lock() {
                table.insert(pid, child);
                ok(pid)
            } else {
                not_found()
            }
        }
        Err(e) => err(&e),
    };
    if !out.is_null() {
        *out = result;
    }
}

/// Block until the child exits and reap it (entry removed up front,
/// matching the interpreter). Ok payload: `value = (code << 1) |
/// success` (see module doc).
///
/// # Safety
///
/// `out` must point at writable `KaracIoResult` storage.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_process_wait(out: *mut KaracIoResult, pid: i64) {
    let child = CHILD_TABLE.lock().ok().and_then(|mut t| t.remove(&pid));
    let result = match child {
        Some(mut child) => match child.wait() {
            Ok(status) => ok(pack_exit_status(status)),
            Err(e) => err(&e),
        },
        None => not_found(),
    };
    if !out.is_null() {
        *out = result;
    }
}

/// Non-blocking poll. Ok payload:
/// `value = (code << 2) | (success << 1) | present`;
/// `present == 0` means the child is still running (Ok(None)).
/// Reaps the table entry when the child has exited.
///
/// # Safety
///
/// `out` must point at writable `KaracIoResult` storage.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_process_try_wait(out: *mut KaracIoResult, pid: i64) {
    let result = match CHILD_TABLE.lock() {
        Ok(mut table) => match table.get_mut(&pid) {
            Some(child) => match child.try_wait() {
                Ok(Some(status)) => {
                    table.remove(&pid);
                    let code = status.code().unwrap_or(-1) as i64;
                    let success = status.success() as i64;
                    ok((code << 2) | (success << 1) | 1)
                }
                Ok(None) => ok(0),
                Err(e) => err(&e),
            },
            None => not_found(),
        },
        Err(_) => not_found(),
    };
    if !out.is_null() {
        *out = result;
    }
}

/// Send SIGKILL (or platform equivalent). The child is NOT reaped —
/// the entry stays in the table until `wait` / `try_wait` removes it.
/// Ok payload: Unit (`value` zero).
///
/// # Safety
///
/// `out` must point at writable `KaracIoResult` storage.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_process_kill(out: *mut KaracIoResult, pid: i64) {
    let result = match CHILD_TABLE.lock() {
        Ok(mut table) => match table.get_mut(&pid) {
            Some(child) => match child.kill() {
                Ok(()) => ok(0),
                Err(e) => err(&e),
            },
            None => not_found(),
        },
        Err(_) => not_found(),
    };
    if !out.is_null() {
        *out = result;
    }
}

/// `Child.{stdout,stderr,stdin}()` — move the captured pipe handle
/// (if any) off the live `Child` into the matching stream table.
/// `which`: 0 = stdout, 1 = stderr, 2 = stdin. Returns `pid` when a
/// handle was taken (→ `Option.Some(handle)` on the codegen side), 0
/// otherwise (not piped / already taken / unknown child → `None`).
#[no_mangle]
pub extern "C" fn karac_runtime_process_take_stream(pid: i64, which: i64) -> i64 {
    let Ok(mut children) = CHILD_TABLE.lock() else {
        return 0;
    };
    let Some(child) = children.get_mut(&pid) else {
        return 0;
    };
    let taken = match which {
        0 => child
            .stdout
            .take()
            .map(|h| STDOUT_TABLE.lock().map(|mut t| t.insert(pid, h)).is_ok()),
        1 => child
            .stderr
            .take()
            .map(|h| STDERR_TABLE.lock().map(|mut t| t.insert(pid, h)).is_ok()),
        2 => child
            .stdin
            .take()
            .map(|h| STDIN_TABLE.lock().map(|mut t| t.insert(pid, h)).is_ok()),
        _ => None,
    };
    match taken {
        Some(true) => pid,
        _ => 0,
    }
}

/// `ChildStdout.read_to_string()` / `ChildStderr.read_to_string()` —
/// drain the captured read handle to EOF (blocks until the child
/// closes its write end), removing the now-exhausted entry. `which`:
/// 0 = stdout, 1 = stderr. Absent handle → `Err(IoError.NotFound)`.
/// Ok payload: String bytes through the `error_msg_ptr`/`_len` buffer
/// reuse (the `FileOkKind::StringPayload` protocol).
///
/// # Safety
///
/// `out` must point at writable `KaracIoResult` storage.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_process_read_to_string(
    out: *mut KaracIoResult,
    pid: i64,
    which: i64,
) {
    let read: Option<std::io::Result<String>> = match which {
        0 => STDOUT_TABLE
            .lock()
            .ok()
            .and_then(|mut t| t.remove(&pid))
            .map(|mut h| {
                let mut buf = String::new();
                h.read_to_string(&mut buf).map(|_| buf)
            }),
        1 => STDERR_TABLE
            .lock()
            .ok()
            .and_then(|mut t| t.remove(&pid))
            .map(|mut h| {
                let mut buf = String::new();
                h.read_to_string(&mut buf).map(|_| buf)
            }),
        _ => None,
    };
    let result = match read {
        Some(Ok(s)) => ok_string(&s),
        Some(Err(e)) => err(&e),
        None => not_found(),
    };
    if !out.is_null() {
        *out = result;
    }
}

/// `ChildStdin.write(data)` — write the String's bytes to the captured
/// stdin pipe (blocks while the OS buffer is full). Absent handle →
/// `Err(IoError.NotFound)`. Ok payload: Unit.
///
/// # Safety
///
/// `out` must point at writable `KaracIoResult` storage; `data` at a
/// live String descriptor (borrow read-only, ends before return).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_process_stdin_write(
    out: *mut KaracIoResult,
    pid: i64,
    data: *const RuntimeKaracString,
) {
    let bytes: &[u8] = if data.is_null() {
        &[]
    } else {
        (*data).as_bytes()
    };
    let result = match STDIN_TABLE.lock() {
        Ok(mut table) => match table.get_mut(&pid) {
            Some(h) => match h.write_all(bytes) {
                Ok(()) => ok(0),
                Err(e) => err(&e),
            },
            None => not_found(),
        },
        Err(_) => not_found(),
    };
    if !out.is_null() {
        *out = result;
    }
}

/// `ChildStdin.close()` — drop the captured stdin handle, closing the
/// pipe (EOF to the child). Idempotent: absent handle is a no-op. The
/// Kāra surface always returns `Ok(Unit)` here (codegen builds it as a
/// constant), so no out-param.
#[no_mangle]
pub extern "C" fn karac_runtime_process_stdin_close(pid: i64) {
    if let Ok(mut table) = STDIN_TABLE.lock() {
        table.remove(&pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kstr(s: &str) -> (RuntimeKaracString, Box<[u8]>) {
        // Heap-descriptor form (cap == 0 static view) backed by a boxed
        // copy the test owns; the runtime only borrows.
        let boxed: Box<[u8]> = s.as_bytes().into();
        (
            RuntimeKaracString {
                data: boxed.as_ptr() as *mut u8,
                len: boxed.len() as i64,
                cap: 0,
            },
            boxed,
        )
    }

    fn fresh() -> KaracIoResult {
        KaracIoResult {
            value: -99,
            error_kind: -99,
            _pad: 0,
            error_msg_ptr: std::ptr::null_mut(),
            error_msg_len: 0,
        }
    }

    #[test]
    fn test_spawn_wait_roundtrip() {
        let (prog, _b) = kstr("true");
        let mut out = fresh();
        unsafe {
            karac_runtime_process_spawn(
                &mut out,
                &prog,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                0,
                0,
                0,
            );
        }
        assert_eq!(out.error_kind, 0, "spawn `true` should succeed");
        let pid = out.value;
        assert!(pid > 0);
        let mut wait_out = fresh();
        unsafe { karac_runtime_process_wait(&mut wait_out, pid) };
        assert_eq!(wait_out.error_kind, 0);
        // code 0, success 1 → packed value 0b01 == 1.
        assert_eq!(wait_out.value, 1);
        // Reaped: second wait is NotFound.
        let mut again = fresh();
        unsafe { karac_runtime_process_wait(&mut again, pid) };
        assert_eq!(again.error_kind, 1);
    }

    #[test]
    fn test_spawn_missing_program_is_not_found() {
        let (prog, _b) = kstr("definitely-not-a-real-binary-kara");
        let mut out = fresh();
        unsafe {
            karac_runtime_process_spawn(
                &mut out,
                &prog,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                0,
                0,
                0,
            );
        }
        assert_eq!(out.error_kind, 1, "missing binary maps to NotFound");
    }

    #[test]
    fn test_piped_stdout_read_to_string() {
        let (prog, _b) = kstr("echo");
        let (arg, _b2) = kstr("kara-pipe");
        let mut out = fresh();
        unsafe {
            // stdout_tag 2 = Piped.
            karac_runtime_process_spawn(&mut out, &prog, &arg, 1, std::ptr::null(), 0, 0, 2, 0);
        }
        assert_eq!(out.error_kind, 0);
        let pid = out.value;
        assert_eq!(karac_runtime_process_take_stream(pid, 0), pid);
        // Second take: already gone.
        assert_eq!(karac_runtime_process_take_stream(pid, 0), 0);
        let mut read_out = fresh();
        unsafe { karac_runtime_process_read_to_string(&mut read_out, pid, 0) };
        assert_eq!(read_out.error_kind, 0);
        let text = unsafe {
            std::str::from_utf8(std::slice::from_raw_parts(
                read_out.error_msg_ptr,
                read_out.error_msg_len as usize,
            ))
            .unwrap()
            .to_string()
        };
        assert_eq!(text, "kara-pipe\n");
        unsafe {
            std::alloc::dealloc(
                read_out.error_msg_ptr,
                std::alloc::Layout::array::<u8>(read_out.error_msg_len as usize).unwrap(),
            );
        }
        let mut wait_out = fresh();
        unsafe { karac_runtime_process_wait(&mut wait_out, pid) };
        assert_eq!(wait_out.error_kind, 0);
        assert_eq!(wait_out.value, 1);
    }

    #[test]
    fn test_try_wait_kill_reap() {
        let (prog, _b) = kstr("sleep");
        let (arg, _b2) = kstr("30");
        let mut out = fresh();
        unsafe {
            karac_runtime_process_spawn(&mut out, &prog, &arg, 1, std::ptr::null(), 0, 0, 0, 0);
        }
        assert_eq!(out.error_kind, 0);
        let pid = out.value;
        let mut tw = fresh();
        unsafe { karac_runtime_process_try_wait(&mut tw, pid) };
        assert_eq!(tw.error_kind, 0);
        assert_eq!(tw.value & 1, 0, "sleep 30 still running → Ok(None)");
        let mut k = fresh();
        unsafe { karac_runtime_process_kill(&mut k, pid) };
        assert_eq!(k.error_kind, 0);
        let mut w = fresh();
        unsafe { karac_runtime_process_wait(&mut w, pid) };
        assert_eq!(w.error_kind, 0);
        // Signal-killed: code -1, success false → (-1 << 1) | 0 == -2.
        assert_eq!(w.value, -2);
    }

    #[test]
    fn test_stdin_write_close_cat_roundtrip() {
        let (prog, _b) = kstr("cat");
        let mut out = fresh();
        unsafe {
            // stdin + stdout piped.
            karac_runtime_process_spawn(
                &mut out,
                &prog,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                2,
                2,
                0,
            );
        }
        assert_eq!(out.error_kind, 0);
        let pid = out.value;
        assert_eq!(karac_runtime_process_take_stream(pid, 2), pid);
        assert_eq!(karac_runtime_process_take_stream(pid, 0), pid);
        let (data, _b2) = kstr("hello kara\n");
        let mut w = fresh();
        unsafe { karac_runtime_process_stdin_write(&mut w, pid, &data) };
        assert_eq!(w.error_kind, 0);
        karac_runtime_process_stdin_close(pid);
        // Idempotent close.
        karac_runtime_process_stdin_close(pid);
        let mut read_out = fresh();
        unsafe { karac_runtime_process_read_to_string(&mut read_out, pid, 0) };
        assert_eq!(read_out.error_kind, 0);
        let text = unsafe {
            std::str::from_utf8(std::slice::from_raw_parts(
                read_out.error_msg_ptr,
                read_out.error_msg_len as usize,
            ))
            .unwrap()
            .to_string()
        };
        assert_eq!(text, "hello kara\n");
        unsafe {
            std::alloc::dealloc(
                read_out.error_msg_ptr,
                std::alloc::Layout::array::<u8>(read_out.error_msg_len as usize).unwrap(),
            );
        }
        let mut wait_out = fresh();
        unsafe { karac_runtime_process_wait(&mut wait_out, pid) };
        assert_eq!(wait_out.error_kind, 0);
        assert_eq!(wait_out.value, 1);
    }

    #[test]
    fn test_env_passes_through() {
        let (prog, _b) = kstr("sh");
        let (a1, _b1) = kstr("-c");
        let (a2, _b2) = kstr("printf %s \"$KARA_PROC_TEST\"");
        let args = [a1, a2];
        let (k, _bk) = kstr("KARA_PROC_TEST");
        let (v, _bv) = kstr("via-env");
        let env = [KaracEnvVarView { key: k, value: v }];
        let mut out = fresh();
        unsafe {
            karac_runtime_process_spawn(
                &mut out,
                &prog,
                args.as_ptr(),
                2,
                env.as_ptr(),
                1,
                0,
                2,
                0,
            );
        }
        assert_eq!(out.error_kind, 0);
        let pid = out.value;
        assert_eq!(karac_runtime_process_take_stream(pid, 0), pid);
        let mut read_out = fresh();
        unsafe { karac_runtime_process_read_to_string(&mut read_out, pid, 0) };
        assert_eq!(read_out.error_kind, 0);
        let text = unsafe {
            std::str::from_utf8(std::slice::from_raw_parts(
                read_out.error_msg_ptr,
                read_out.error_msg_len as usize,
            ))
            .unwrap()
            .to_string()
        };
        assert_eq!(text, "via-env");
        unsafe {
            std::alloc::dealloc(
                read_out.error_msg_ptr,
                std::alloc::Layout::array::<u8>(read_out.error_msg_len as usize).unwrap(),
            );
        }
        let mut wait_out = fresh();
        unsafe { karac_runtime_process_wait(&mut wait_out, pid) };
        assert_eq!(wait_out.error_kind, 0);
    }
}
