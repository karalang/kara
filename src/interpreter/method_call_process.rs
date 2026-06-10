//! Process-method dispatch — `Command.spawn`, `Child.wait` / `try_wait`
//! / `kill`. Uses `std::process` under the hood.
//!
//! Storage: `Interpreter.child_table: HashMap<i64, std::process::Child>`
//! keyed by OS pid. `spawn` populates it; `wait` removes on success;
//! `try_wait` removes when the child has exited; `kill` leaves the
//! entry in place (caller still needs to `wait` to reap the process).
//!
//! Pid as the key is safe in the single-threaded tree-walk interpreter
//! because each `spawn` produces a fresh entry before any code can
//! observe a pid reuse — the OS guarantees pid uniqueness while the
//! child is in our table.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::Command as StdCommand;

use crate::ast::*;
use crate::token::Span;

use super::value::{EnumData, Value};

impl<'a> super::Interpreter<'a> {
    pub(super) fn try_eval_process_method(
        &mut self,
        method: &str,
        obj: &Value,
        _args: &[CallArg],
        _span: &Span,
    ) -> Option<Value> {
        // Borrow the receiver and clone only inside the arm that actually
        // handles `method` — the match returns `None` for any non-process
        // method (e.g. a `Map`'s `get`/`insert`) without cloning at all.
        // See the dispatch-clone fix in `method_call.rs` (B-2026-06-07-4).
        match method {
            "spawn" => self.eval_command_spawn(obj.clone()),
            "wait" => self.eval_child_wait(obj.clone()),
            "try_wait" => self.eval_child_try_wait(obj.clone()),
            "kill" => self.eval_child_kill(obj.clone()),
            // Captured-pipe accessors / handle methods. Each guards on the
            // receiver struct name and returns `None` for any other shape,
            // so a same-named method on an unrelated type (e.g. the
            // `Command.stdout(cfg)` builder, a `File.write`, or the `Stdin`
            // resource's `read_to_string`) falls through to its own
            // dispatcher.
            "stdout" => self.eval_child_take_stream(obj.clone(), StdStream::Out),
            "stderr" => self.eval_child_take_stream(obj.clone(), StdStream::Err),
            "stdin" => self.eval_child_take_stream(obj.clone(), StdStream::In),
            "read_to_string" => self.eval_child_stream_read_to_string(obj.clone()),
            "write" => self.eval_child_stdin_write(obj.clone(), _args),
            "close" => self.eval_child_stdin_close(obj.clone()),
            _ => None,
        }
    }

    fn eval_command_spawn(&mut self, obj: Value) -> Option<Value> {
        let Value::Struct { name, fields } = &obj else {
            return None;
        };
        if name != "Command" {
            return None;
        }
        let program = read_string_field(fields, "program");
        let cmd_args = read_string_vec_field(fields, "cmd_args");
        let cmd_env = read_env_var_vec_field(fields, "cmd_env");

        let mut std_cmd = StdCommand::new(&program);
        for a in &cmd_args {
            std_cmd.arg(a);
        }
        for (k, v) in &cmd_env {
            std_cmd.env(k, v);
        }
        // Stdio redirection (phase-8 std.process). Each field is a
        // `Stdio` enum; only `Null` / `Piped` change behavior —
        // `Inherit` is `std::process`'s own default, so leave it unset.
        if let Some(cfg) = stdio_for_field(fields, "cmd_stdin") {
            std_cmd.stdin(cfg);
        }
        if let Some(cfg) = stdio_for_field(fields, "cmd_stdout") {
            std_cmd.stdout(cfg);
        }
        if let Some(cfg) = stdio_for_field(fields, "cmd_stderr") {
            std_cmd.stderr(cfg);
        }
        match std_cmd.spawn() {
            Ok(child) => {
                let pid = child.id() as i64;
                self.child_table.insert(pid, child);
                let mut child_fields = HashMap::new();
                child_fields.insert("pid".to_string(), Value::Int(pid));
                Some(result_ok(Value::Struct {
                    name: "Child".to_string(),
                    fields: child_fields,
                }))
            }
            Err(e) => Some(result_err(io_error_variant_from(&e))),
        }
    }

    fn eval_child_wait(&mut self, obj: Value) -> Option<Value> {
        let pid = child_pid(&obj)?;
        match self.child_table.remove(&pid) {
            Some(mut child) => match child.wait() {
                Ok(status) => Some(result_ok(exit_status_value(status))),
                Err(e) => Some(result_err(io_error_variant_from(&e))),
            },
            None => Some(result_err(io_not_found())),
        }
    }

    fn eval_child_try_wait(&mut self, obj: Value) -> Option<Value> {
        let pid = child_pid(&obj)?;
        let result = match self.child_table.get_mut(&pid) {
            Some(child) => child.try_wait(),
            None => return Some(result_err(io_not_found())),
        };
        match result {
            Ok(Some(status)) => {
                self.child_table.remove(&pid);
                Some(result_ok(option_some(exit_status_value(status))))
            }
            Ok(None) => Some(result_ok(option_none())),
            Err(e) => Some(result_err(io_error_variant_from(&e))),
        }
    }

    fn eval_child_kill(&mut self, obj: Value) -> Option<Value> {
        let pid = child_pid(&obj)?;
        let result = match self.child_table.get_mut(&pid) {
            Some(child) => child.kill(),
            None => return Some(result_err(io_not_found())),
        };
        match result {
            Ok(()) => Some(result_ok(Value::Unit)),
            Err(e) => Some(result_err(io_error_variant_from(&e))),
        }
    }

    /// `Child.{stdout,stderr,stdin}()` — `take()` the captured pipe handle
    /// off the live `std::process::Child`, move it into the matching handle
    /// table (keyed by pid), and hand back `Option.Some(handle)`. Returns
    /// `Option.None` when the stream wasn't spawned `Stdio.Piped`, was
    /// already taken, or the child is no longer tracked — mirroring
    /// `std::process::Child::{stdout,stderr,stdin}` being `Option`. The
    /// receiver-shape guard (`child_pid` requires a `Child` struct) returns
    /// `None` for any other shape so unrelated `stdout`/`stderr`/`stdin`
    /// methods (the `Command.<stream>(cfg)` builders) fall through.
    fn eval_child_take_stream(&mut self, obj: Value, stream: StdStream) -> Option<Value> {
        let pid = child_pid(&obj)?;
        // The `std::process::Child` is borrowed only inside this block, so
        // the borrow ends before we touch the handle tables (a second
        // `&mut self`). An absent child → no handle to give (`None`).
        match stream {
            StdStream::Out => {
                let taken = {
                    let Some(child) = self.child_table.get_mut(&pid) else {
                        return Some(option_none());
                    };
                    child.stdout.take()
                };
                match taken {
                    Some(h) => {
                        self.child_stdout_table.insert(pid, h);
                        Some(option_some(child_stream_handle("ChildStdout", pid)))
                    }
                    None => Some(option_none()),
                }
            }
            StdStream::Err => {
                let taken = {
                    let Some(child) = self.child_table.get_mut(&pid) else {
                        return Some(option_none());
                    };
                    child.stderr.take()
                };
                match taken {
                    Some(h) => {
                        self.child_stderr_table.insert(pid, h);
                        Some(option_some(child_stream_handle("ChildStderr", pid)))
                    }
                    None => Some(option_none()),
                }
            }
            StdStream::In => {
                let taken = {
                    let Some(child) = self.child_table.get_mut(&pid) else {
                        return Some(option_none());
                    };
                    child.stdin.take()
                };
                match taken {
                    Some(h) => {
                        self.child_stdin_table.insert(pid, h);
                        Some(option_some(child_stream_handle("ChildStdin", pid)))
                    }
                    None => Some(option_none()),
                }
            }
        }
    }

    /// `ChildStdout.read_to_string()` / `ChildStderr.read_to_string()` —
    /// drain the captured read handle to a `String` (blocks until the child
    /// closes its write end), removing the now-exhausted entry. A handle
    /// that's absent (never taken, or already read) yields
    /// `Err(IoError.NotFound)`. Returns `None` for any non-read-handle
    /// receiver so the `Stdin` / `FileSystem` `read_to_string` resource
    /// methods fall through to their own dispatcher.
    fn eval_child_stream_read_to_string(&mut self, obj: Value) -> Option<Value> {
        let Value::Struct { name, fields } = &obj else {
            return None;
        };
        let pid = match fields.get("pid") {
            Some(Value::Int(p)) => *p,
            _ => return None,
        };
        let read: Option<std::io::Result<String>> = match name.as_str() {
            "ChildStdout" => self.child_stdout_table.remove(&pid).map(|mut h| {
                let mut buf = String::new();
                h.read_to_string(&mut buf).map(|_| buf)
            }),
            "ChildStderr" => self.child_stderr_table.remove(&pid).map(|mut h| {
                let mut buf = String::new();
                h.read_to_string(&mut buf).map(|_| buf)
            }),
            _ => return None,
        };
        match read {
            Some(Ok(s)) => Some(result_ok(Value::String(s))),
            Some(Err(e)) => Some(result_err(io_error_variant_from(&e))),
            None => Some(result_err(io_not_found())),
        }
    }

    /// `ChildStdin.write(data)` — write `data`'s bytes to the captured
    /// stdin pipe (blocks if the OS buffer is full). Absent handle →
    /// `Err(IoError.NotFound)`. `None` for any non-`ChildStdin` receiver.
    fn eval_child_stdin_write(&mut self, obj: Value, args: &[CallArg]) -> Option<Value> {
        let Value::Struct { name, fields } = &obj else {
            return None;
        };
        if name != "ChildStdin" {
            return None;
        }
        let pid = match fields.get("pid") {
            Some(Value::Int(p)) => *p,
            _ => return None,
        };
        let data = match args.first().map(|a| self.eval_expr_inner(&a.value)) {
            Some(Value::String(s)) => s,
            // Receiver is confirmed `ChildStdin` — a non-String arg can't
            // be another dispatcher's `write`, so surface an error rather
            // than falling through.
            _ => return Some(result_err(io_not_found())),
        };
        match self.child_stdin_table.get_mut(&pid) {
            Some(h) => match h.write_all(data.as_bytes()) {
                Ok(()) => Some(result_ok(Value::Unit)),
                Err(e) => Some(result_err(io_error_variant_from(&e))),
            },
            None => Some(result_err(io_not_found())),
        }
    }

    /// `ChildStdin.close()` — drop the captured stdin handle, closing the
    /// pipe and signaling EOF to the child. Idempotent: closing an
    /// already-closed / never-taken handle is a no-op `Ok`. `None` for any
    /// non-`ChildStdin` receiver (so `File.close` etc. fall through).
    fn eval_child_stdin_close(&mut self, obj: Value) -> Option<Value> {
        let Value::Struct { name, fields } = &obj else {
            return None;
        };
        if name != "ChildStdin" {
            return None;
        }
        let pid = match fields.get("pid") {
            Some(Value::Int(p)) => *p,
            _ => return None,
        };
        // Dropping the handle (whether present or not) closes the fd.
        self.child_stdin_table.remove(&pid);
        Some(result_ok(Value::Unit))
    }
}

/// Which standard stream a `Child.<accessor>()` call targets.
#[derive(Clone, Copy)]
enum StdStream {
    Out,
    Err,
    In,
}

/// Build the Kāra captured-pipe handle struct (`ChildStdout` /
/// `ChildStderr` / `ChildStdin`) carrying the owning child's pid.
fn child_stream_handle(struct_name: &str, pid: i64) -> Value {
    let mut fields = HashMap::new();
    fields.insert("pid".to_string(), Value::Int(pid));
    Value::Struct {
        name: struct_name.to_string(),
        fields,
    }
}

// ── Receiver-shape helpers ────────────────────────────────────────

fn child_pid(obj: &Value) -> Option<i64> {
    let Value::Struct { name, fields } = obj else {
        return None;
    };
    if name != "Child" {
        return None;
    }
    match fields.get("pid") {
        Some(Value::Int(p)) => Some(*p),
        _ => None,
    }
}

fn read_string_field(fields: &HashMap<String, Value>, key: &str) -> String {
    match fields.get(key) {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Map a `Command` redirection field to the `std::process::Stdio` to
/// apply, or `None` to leave it at `std::process`'s own default. Only an
/// explicit `Stdio.Null` (→ `null()`) or `Stdio.Piped` (→ `piped()`)
/// acts; the `Stdio.Inherit` default and any other / absent shape read as
/// "inherit", which is already the default.
fn stdio_for_field(fields: &HashMap<String, Value>, key: &str) -> Option<std::process::Stdio> {
    match fields.get(key) {
        Some(Value::EnumVariant {
            enum_name, variant, ..
        }) if enum_name == "Stdio" => match variant.as_str() {
            "Null" => Some(std::process::Stdio::null()),
            "Piped" => Some(std::process::Stdio::piped()),
            _ => None,
        },
        _ => None,
    }
}

fn read_string_vec_field(fields: &HashMap<String, Value>, key: &str) -> Vec<String> {
    let Some(Value::Array(arc)) = fields.get(key) else {
        return Vec::new();
    };
    arc.read()
        .unwrap()
        .iter()
        .filter_map(|v| match v {
            Value::String(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn read_env_var_vec_field(fields: &HashMap<String, Value>, key: &str) -> Vec<(String, String)> {
    let Some(Value::Array(arc)) = fields.get(key) else {
        return Vec::new();
    };
    arc.read()
        .unwrap()
        .iter()
        .filter_map(|v| {
            let Value::Struct { name, fields } = v else {
                return None;
            };
            if name != "EnvVar" {
                return None;
            }
            let k = match fields.get("key") {
                Some(Value::String(s)) => s.clone(),
                _ => return None,
            };
            let val = match fields.get("value") {
                Some(Value::String(s)) => s.clone(),
                _ => return None,
            };
            Some((k, val))
        })
        .collect()
}

// ── Kāra-value constructors ───────────────────────────────────────

fn result_ok(v: Value) -> Value {
    Value::EnumVariant {
        enum_name: "Result".to_string(),
        variant: "Ok".to_string(),
        data: EnumData::Tuple(vec![v]),
    }
}

fn result_err(v: Value) -> Value {
    Value::EnumVariant {
        enum_name: "Result".to_string(),
        variant: "Err".to_string(),
        data: EnumData::Tuple(vec![v]),
    }
}

fn option_some(v: Value) -> Value {
    Value::EnumVariant {
        enum_name: "Option".to_string(),
        variant: "Some".to_string(),
        data: EnumData::Tuple(vec![v]),
    }
}

fn option_none() -> Value {
    Value::EnumVariant {
        enum_name: "Option".to_string(),
        variant: "None".to_string(),
        data: EnumData::Unit,
    }
}

fn io_not_found() -> Value {
    Value::EnumVariant {
        enum_name: "IoError".to_string(),
        variant: "NotFound".to_string(),
        data: EnumData::Unit,
    }
}

// Map a `std::io::Error` to the matching `IoError` variant. Falls
// back to `Other(message)` for kinds outside our enum so the user
// always gets a tagged Err rather than a panic.
fn io_error_variant_from(e: &std::io::Error) -> Value {
    use std::io::ErrorKind as K;
    let (variant, payload): (&str, Option<Value>) = match e.kind() {
        K::NotFound => ("NotFound", None),
        K::PermissionDenied => ("PermissionDenied", None),
        K::AlreadyExists => ("AlreadyExists", None),
        K::UnexpectedEof => ("UnexpectedEof", None),
        K::InvalidData => ("InvalidUtf8", None),
        K::Interrupted => ("Interrupted", None),
        _ => ("Other", Some(Value::String(e.to_string()))),
    };
    Value::EnumVariant {
        enum_name: "IoError".to_string(),
        variant: variant.to_string(),
        data: match payload {
            Some(p) => EnumData::Tuple(vec![p]),
            None => EnumData::Unit,
        },
    }
}

fn exit_status_value(status: std::process::ExitStatus) -> Value {
    let code = status.code().unwrap_or(-1) as i64;
    let success = status.success();
    let mut fields = HashMap::new();
    fields.insert("code".to_string(), Value::Int(code));
    fields.insert("success".to_string(), Value::Bool(success));
    Value::Struct {
        name: "ExitStatus".to_string(),
        fields,
    }
}
