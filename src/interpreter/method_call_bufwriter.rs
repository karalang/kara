//! `BufWriter[W]` method dispatch — `write` / `flush` on a
//! `Value::BufWriter` receiver. Phase 8 `BufWriter[W]` slice (the
//! Write-side peer of `BufReader[R]`).
//!
//! The receiver carries an `Arc<Mutex<std::io::BufWriter<std::fs::File>>>`;
//! each method locks the mutex for the call (single-threaded interpreter
//! means contention is moot — the lock exists so the Value variant stays
//! Clone). Both methods carry `writes(FileSystem)` (the v1 concrete
//! binding for `W = File`).
//!
//! `write` takes a `Slice[u8]` source, reads its bytes into a temporary
//! `Vec<u8>`, writes through the buffered writer, and returns the byte
//! count accepted — identical shape to `File.write`. `flush` drains the
//! internal buffer to the underlying writer and returns `Result[Unit,
//! IoError]`. Buffered bytes that are never explicitly flushed still reach
//! the fd when the last `Value::BufWriter` Arc drops (via
//! `std::io::BufWriter`'s own Drop), so the canonical write-then-drop shape
//! is durable; `flush()` exists to surface flush errors and to force the
//! write at a known point.
//!
//! The cancel-safety annotation (the "unflushed bytes" bit) is deferred to
//! a follow-on slice.

use std::io::Write;

use crate::ast::*;
use crate::token::Span;

use super::helpers::{io_err_value, io_error_from_std, io_ok};
use super::value::Value;

impl<'a> super::Interpreter<'a> {
    pub(super) fn try_eval_bufwriter_method(
        &mut self,
        method: &str,
        obj: &Value,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        let Value::BufWriter(ref writer_arc) = obj else {
            return None;
        };
        match method {
            "write" | "write_all" => {
                self.track_effect("writes(FileSystem)");
                // `write(buf: Slice[u8]) -> Result[usize, IoError]` returns
                // the byte count; `write_all(buf) -> Result[Unit, IoError]`
                // loops until the whole buffer is accepted and returns Unit.
                // Both read the slice's bytes into a temporary Vec<u8> and
                // push through the buffered writer's locked handle — only the
                // std call and the Ok payload shape differ.
                let Some(buf_arg) = args.first() else {
                    return Some(self.record_runtime_error(
                        format!("BufWriter.{method} expects a `Slice[u8]` buffer argument"),
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
                    // `Vec[u8]` also reachable when the user passed a Vec
                    // instead of a Slice — be permissive at the interpreter
                    // level (the typechecker enforces the declared shape).
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
                                "BufWriter.{method} expects a `Slice[u8]` buffer, got `{}`",
                                other.variant_name()
                            ),
                            span,
                        ));
                    }
                };
                if method == "write_all" {
                    let write_result = {
                        let mut guard = writer_arc.lock().unwrap();
                        guard.write_all(&bytes)
                    };
                    match write_result {
                        Ok(()) => Some(io_ok(Value::Unit)),
                        Err(e) => Some(io_err_value(io_error_from_std(&e))),
                    }
                } else {
                    let write_result = {
                        let mut guard = writer_arc.lock().unwrap();
                        guard.write(&bytes)
                    };
                    match write_result {
                        Ok(n) => Some(io_ok(Value::Int(n as i64))),
                        Err(e) => Some(io_err_value(io_error_from_std(&e))),
                    }
                }
            }
            "flush" => {
                self.track_effect("writes(FileSystem)");
                // `flush() -> Result[Unit, IoError]`: drain the internal
                // buffer to the underlying writer. `std::io::BufWriter::flush`
                // writes any buffered bytes through then flushes the wrapped
                // writer.
                let flush_result = {
                    let mut guard = writer_arc.lock().unwrap();
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
