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
//! `write` / `flush` / `sync_all` / `sync_data`.
//!
//! `seek` landed with B-2026-08-10-3; metadata is still deferred.

use std::io::{Read, Seek, SeekFrom, Write};

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
                    // A fixed `Array[u8, N]` (or a `Vec[u8]`) passed as the
                    // `mut Slice[u8]` buffer — the idiom `examples/relay`
                    // ships for `TcpStream.read`:
                    //
                    //     let mut buf: Array[u8, 4096] = [0u8; 4096];
                    //     f.read(mut buf)
                    //
                    // AOT accepts it (the array coerces to a slice); the
                    // interpreter rejected it, so the same program read fine
                    // under `karac build` and died under `karac run --interp`.
                    // `File.write` directly below already takes `Value::Array`
                    // for exactly this reason ("be permissive at the
                    // interpreter level — the typechecker enforces the declared
                    // shape"); read now matches write. The whole array is the
                    // buffer, so the slice window is the full range.
                    Value::Array(ref rc) => {
                        let len = rc.read().unwrap().len();
                        (rc.clone(), 0usize, len)
                    }
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
            // Durability. Unlike `flush` (a no-op on `std::fs::File` —
            // it pushes userspace buffers, never the page cache), these
            // issue the real fsync/fdatasync and only return once the
            // filesystem reports the bytes durable.
            "sync_all" => {
                self.track_effect("writes(FileSystem)");
                let sync_result = {
                    let guard = file_arc.lock().unwrap();
                    guard.sync_all()
                };
                match sync_result {
                    Ok(()) => Some(io_ok(Value::Unit)),
                    Err(e) => Some(io_err_value(io_error_from_std(&e))),
                }
            }
            "sync_data" => {
                self.track_effect("writes(FileSystem)");
                let sync_result = {
                    let guard = file_arc.lock().unwrap();
                    guard.sync_data()
                };
                match sync_result {
                    Ok(()) => Some(io_ok(Value::Unit)),
                    Err(e) => Some(io_err_value(io_error_from_std(&e))),
                }
            }
            // B-2026-08-10-3 — `seek(whence: SeekFrom, offset: i64) ->
            // Result[i64, IoError]`, returning the NEW absolute position.
            // `reads(FileSystem)` for the same reason `read` is: the cursor
            // moves, the contents do not.
            "seek" => {
                self.track_effect("reads(FileSystem)");
                let (Some(whence_arg), Some(offset_arg)) = (args.first(), args.get(1)) else {
                    return Some(self.record_runtime_error(
                        "File.seek expects (whence: SeekFrom, offset: i64)".to_string(),
                        span,
                    ));
                };
                let whence_val = self.eval_expr_inner(&whence_arg.value);
                let offset_val = self.eval_expr_inner(&offset_arg.value);
                let Value::Int(offset) = offset_val else {
                    return Some(self.record_runtime_error(
                        format!(
                            "File.seek offset must be an i64, got {}",
                            offset_val.variant_name()
                        ),
                        span,
                    ));
                };
                // The variant NAME is the selector — `SeekFrom` is payload-free
                // precisely so this stays a three-way match rather than a
                // payload unpack.
                let pos = match &whence_val {
                    Value::EnumVariant {
                        enum_name, variant, ..
                    } if enum_name == "SeekFrom" => match variant.as_str() {
                        "Start" => {
                            // A negative `Start` offset is an OS-layer error,
                            // not a wrap-around: report it as `Err` rather than
                            // casting it into a huge u64.
                            if offset < 0 {
                                return Some(io_err_value(io_error_from_std(
                                    &std::io::Error::new(
                                        std::io::ErrorKind::InvalidInput,
                                        "invalid seek to a negative position",
                                    ),
                                )));
                            }
                            SeekFrom::Start(offset as u64)
                        }
                        "Current" => SeekFrom::Current(offset),
                        "End" => SeekFrom::End(offset),
                        other => {
                            return Some(self.record_runtime_error(
                                format!("File.seek: unknown SeekFrom variant '{other}'"),
                                span,
                            ));
                        }
                    },
                    _ => {
                        return Some(self.record_runtime_error(
                            format!(
                                "File.seek whence must be a SeekFrom, got {}",
                                whence_val.variant_name()
                            ),
                            span,
                        ));
                    }
                };
                let seek_result = {
                    let mut guard = file_arc.lock().unwrap();
                    guard.seek(pos)
                };
                match seek_result {
                    Ok(p) => Some(io_ok(Value::Int(p as i64))),
                    Err(e) => Some(io_err_value(io_error_from_std(&e))),
                }
            }
            _ => None,
        }
    }
}
