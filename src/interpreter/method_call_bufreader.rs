//! `BufReader[R]` method dispatch — `read_line` / `read_to_string` /
//! `read` on a `Value::BufReader` receiver. Phase 8 `BufReader[R]` slice.
//!
//! The receiver carries an `Arc<Mutex<std::io::BufReader<std::fs::File>>>`;
//! each method locks the mutex for the read (single-threaded interpreter
//! means contention is moot — the lock exists so the Value variant stays
//! Clone). All three read methods carry `reads(FileSystem)` (the v1
//! concrete binding for `R = File`).
//!
//! `read_line` / `read_to_string` take a `mut String` destination and
//! append into it, returning the byte count (`0` from `read_line` signals
//! EOF). The append-and-write-back mirrors the interpreter's String-
//! mutation idiom (`String.push` in `method_call_seq.rs`): the destination
//! must be a bare identifier so the new value can be written back through
//! `env.set`. `read` takes a `mut Slice[u8]` and writes the bytes read
//! back through the slice's shared storage, exactly like `File.read`.
//!
//! `lines()` / `fill_buf` / `consume` and the cancel-safety annotation are
//! deferred to follow-on slices.

use std::io::{BufRead, Read};

use crate::ast::*;
use crate::token::Span;

use super::helpers::{io_err_value, io_error_from_std, io_ok};
use super::value::Value;

impl<'a> super::Interpreter<'a> {
    pub(super) fn try_eval_bufreader_method(
        &mut self,
        method: &str,
        obj: Value,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        let Value::BufReader(ref reader_arc) = obj else {
            return None;
        };
        match method {
            "read_line" | "read_to_string" => {
                self.track_effect("reads(FileSystem)");
                // Destination is a `mut String`; we read its current
                // value, append the bytes read, and write the new value
                // back through the binding. The append matches Rust's
                // `BufRead::read_line` / `Read::read_to_string` contract
                // (append, not replace), so a non-empty `buf` is preserved.
                let Some(buf_arg) = args.first() else {
                    return Some(self.record_runtime_error(
                        format!("BufReader.{method} expects a `mut String` buffer argument"),
                        span,
                    ));
                };
                let mut s = match self.eval_expr_inner(&buf_arg.value) {
                    Value::String(s) => s,
                    other => {
                        return Some(self.record_runtime_error(
                            format!(
                                "BufReader.{method} expects a `mut String` buffer, got `{}`",
                                other.variant_name()
                            ),
                            span,
                        ));
                    }
                };
                let read_result = {
                    let mut guard = reader_arc.lock().unwrap();
                    if method == "read_line" {
                        guard.read_line(&mut s)
                    } else {
                        guard.read_to_string(&mut s)
                    }
                };
                match read_result {
                    Ok(n) => {
                        // Write the mutated String back through the binding
                        // so the caller observes the appended bytes. Only a
                        // bare identifier destination is supported at v1
                        // (the canonical `let mut line = String.new();
                        // br.read_line(line)` idiom); a non-identifier place
                        // evaluates but the mutation is dropped — same limit
                        // as `String.push`.
                        if let ExprKind::Identifier(name) = &buf_arg.value.kind {
                            self.env.set(name, Value::String(s));
                        }
                        Some(io_ok(Value::Int(n as i64)))
                    }
                    Err(e) => Some(io_err_value(io_error_from_std(&e))),
                }
            }
            "read" => {
                self.track_effect("reads(FileSystem)");
                // `read(buf: mut Slice[u8]) -> Result[usize, IoError]`.
                // Identical shape to `File.read`: read into a temporary
                // byte buffer, then write the bytes back through the
                // slice's shared storage as `Value::Int` words.
                let Some(buf_arg) = args.first() else {
                    return Some(self.record_runtime_error(
                        "BufReader.read expects a `mut Slice[u8]` buffer argument".to_string(),
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
                                "BufReader.read expects a `mut Slice[u8]` buffer, got `{}`",
                                other.variant_name()
                            ),
                            span,
                        ));
                    }
                };
                let mut byte_buf = vec![0u8; slice_len];
                let read_result = {
                    let mut guard = reader_arc.lock().unwrap();
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
            "lines" => {
                // `lines() -> LinesIter[R]`: hand back an iterator over the
                // same wrapped BufReader (Arc-shared). Rust's `lines()`
                // consumes the reader; the interpreter shares it, so draining
                // the iterator advances — and leaves at EOF — the originating
                // `BufReader`. The per-line reads happen during the for-loop
                // drain (`eval_expr.rs`'s `Value::LinesIter` arm), which has
                // no method-call site, so the `reads(FileSystem)` effect is
                // attributed here at the `lines()` call.
                self.track_effect("reads(FileSystem)");
                Some(Value::LinesIter(reader_arc.clone()))
            }
            _ => None,
        }
    }
}
