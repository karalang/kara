//! File handle method dispatch — `read` / `write` / `flush` on a
//! `Value::File` receiver. Phase 8 slice F1.
//!
//! The receiver carries an `Arc<Mutex<std::fs::File>>`; each method
//! locks the mutex for the duration of the syscall (single-threaded
//! interpreter means contention is moot — the lock exists so the
//! Value variant stays Clone). `read` takes a `mut Slice[u8]` and
//! returns `Result[usize, IoError]` where 0 = EOF; `write` takes a
//! `Slice[u8]` and returns the bytes-written count; `flush` returns
//! `Result[Unit, IoError]`. Effect tracking matches the slice-F1
//! design: `reads(FileSystem)` on `read`; `writes(FileSystem)` on
//! `write` / `flush`.
//!
//! Seek / sync_all / metadata are deferred to a follow-on slice.

use std::io::{Read, Write};

use crate::ast::*;
use crate::token::Span;

use super::helpers::{io_err_value, io_error_from_std, io_ok};
use super::value::Value;

impl<'a> super::Interpreter<'a> {
    pub(super) fn try_eval_file_method(
        &mut self,
        method: &str,
        obj: &Value,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        let Value::File(ref file_arc) = obj else {
            return None;
        };
        match method {
            "read" => {
                self.track_effect("reads(FileSystem)");
                // `read(buf: mut Slice[u8]) -> Result[usize, IoError]`.
                // The slice carries the mutable destination; on Ok, we
                // write the read bytes back through its storage. The
                // interpreter's `Value::Slice` exposes the underlying
                // shared `Arc<RwLock<Vec<Value>>>` plus `start` / `len`
                // (see `value::Value::Slice`).
                let Some(buf_arg) = args.first() else {
                    return Some(self.record_runtime_error(
                        "File.read expects a `mut Slice[u8]` buffer argument".to_string(),
                        span,
                    ));
                };
                let buf_val = self.eval_expr_inner(&buf_arg.value);
                let (storage, start, slice_len) = match buf_val {
                    Value::Slice {
                        ref storage,
                        start,
                        len,
                        ..
                    } => (storage.clone(), start, len),
                    other => {
                        return Some(self.record_runtime_error(
                            format!(
                                "File.read expects a `mut Slice[u8]` buffer, got `{}`",
                                other.variant_name()
                            ),
                            span,
                        ));
                    }
                };
                // Read into a temporary byte buffer, then write the
                // bytes back through the slice's storage as
                // `Value::Int` words (Kāra's u8 surface is i64 in the
                // interpreter; codegen will narrow to actual bytes).
                let mut byte_buf = vec![0u8; slice_len];
                let read_result = {
                    let mut guard = file_arc.lock().unwrap();
                    guard.read(&mut byte_buf)
                };
                match read_result {
                    Ok(n) => {
                        let mut storage_guard = storage.write().unwrap();
                        for (i, &b) in byte_buf[..n].iter().enumerate() {
                            storage_guard[start + i] = Value::Int(b as i64);
                        }
                        Some(io_ok(Value::Int(n as i64)))
                    }
                    Err(e) => Some(io_err_value(io_error_from_std(&e))),
                }
            }
            "write" => {
                self.track_effect("writes(FileSystem)");
                // `write(buf: Slice[u8]) -> Result[usize, IoError]`.
                // Reads the slice's bytes into a temporary Vec<u8>,
                // writes through the file's locked handle, returns
                // the byte count.
                let Some(buf_arg) = args.first() else {
                    return Some(self.record_runtime_error(
                        "File.write expects a `Slice[u8]` buffer argument".to_string(),
                        span,
                    ));
                };
                let buf_val = self.eval_expr_inner(&buf_arg.value);
                let bytes: Vec<u8> = match buf_val {
                    Value::Slice {
                        ref storage,
                        start,
                        len,
                        ..
                    } => {
                        let guard = storage.read().unwrap();
                        guard[start..start + len]
                            .iter()
                            .map(|v| match v {
                                Value::Int(n) => *n as u8,
                                _ => 0u8,
                            })
                            .collect()
                    }
                    // `Vec[u8]` also reachable when the user passed a
                    // Vec instead of a Slice — be permissive at the
                    // interpreter level (the typechecker enforces the
                    // declared shape).
                    Value::Array(ref rc) => rc
                        .read()
                        .unwrap()
                        .iter()
                        .map(|v| match v {
                            Value::Int(n) => *n as u8,
                            _ => 0u8,
                        })
                        .collect(),
                    other => {
                        return Some(self.record_runtime_error(
                            format!(
                                "File.write expects a `Slice[u8]` buffer, got `{}`",
                                other.variant_name()
                            ),
                            span,
                        ));
                    }
                };
                let write_result = {
                    let mut guard = file_arc.lock().unwrap();
                    guard.write(&bytes)
                };
                match write_result {
                    Ok(n) => Some(io_ok(Value::Int(n as i64))),
                    Err(e) => Some(io_err_value(io_error_from_std(&e))),
                }
            }
            "flush" => {
                self.track_effect("writes(FileSystem)");
                let flush_result = {
                    let mut guard = file_arc.lock().unwrap();
                    guard.flush()
                };
                match flush_result {
                    Ok(()) => Some(io_ok(Value::Unit)),
                    Err(e) => Some(io_err_value(io_error_from_std(&e))),
                }
            }
            _ => None,
        }
    }
}
