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
use std::sync::{Arc, RwLock};

use crate::ast::*;
use crate::token::Span;

use super::helpers::{io_err_value, io_error_from_std, io_ok};
use super::value::Value;

impl<'a> super::Interpreter<'a> {
    pub(super) fn try_eval_bufreader_method(
        &mut self,
        method: &str,
        obj: &Value,
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
            "fill_buf" => {
                // `fill_buf() -> Result[Slice[u8], IoError]`: fill the internal
                // buffer (if empty) from the underlying reader and return its
                // currently-buffered, not-yet-consumed bytes (empty at EOF).
                // The returned slice is a fresh *snapshot copy* of the buffer
                // — the tree-walk interpreter can't hand back a `Slice` that
                // aliases std::io::BufReader's private buffer — so it stays
                // valid across a following `consume`; re-call `fill_buf` to
                // observe the post-consume buffer.
                self.track_effect("reads(FileSystem)");
                let fill_result = {
                    let mut guard = reader_arc.lock().unwrap();
                    // Copy out before releasing the borrow on `guard`.
                    guard.fill_buf().map(|bytes| bytes.to_vec())
                };
                match fill_result {
                    Ok(bytes) => {
                        let len = bytes.len();
                        let storage: Vec<Value> =
                            bytes.into_iter().map(|b| Value::Int(b as i64)).collect();
                        Some(io_ok(Value::Slice {
                            storage: Arc::new(RwLock::new(storage)),
                            start: 0,
                            len,
                            mutable: false,
                        }))
                    }
                    Err(e) => Some(io_err_value(io_error_from_std(&e))),
                }
            }
            "consume" => {
                // `consume(n: usize)`: mark `n` already-peeked bytes (from a
                // preceding `fill_buf`) as consumed so they aren't returned
                // again by `fill_buf` / `read`. No I/O, no effect; returns Unit
                // (mirrors Rust's `BufRead::consume`). Clamped to the available
                // buffered length — std::io::BufReader saturates an over-count,
                // but being explicit avoids relying on that.
                let Some(n_arg) = args.first() else {
                    return Some(self.record_runtime_error(
                        "BufReader.consume expects a `usize` count argument".to_string(),
                        span,
                    ));
                };
                let n = match self.eval_expr_inner(&n_arg.value) {
                    Value::Int(n) => n.max(0) as usize,
                    other => {
                        return Some(self.record_runtime_error(
                            format!(
                                "BufReader.consume expects a `usize` count, got `{}`",
                                other.variant_name()
                            ),
                            span,
                        ));
                    }
                };
                let mut guard = reader_arc.lock().unwrap();
                let avail = guard.buffer().len();
                guard.consume(n.min(avail));
                Some(Value::Unit)
            }
            _ => None,
        }
    }
}
