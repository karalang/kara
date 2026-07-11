//! Sequence-method dispatch — the bodies of all read/write methods
//! on `Slice[T]` / `Vec[T]` / `Array[T,N]` (and a few `Map`/`String`
//! arms that share the same dispatch site): `len`/`chars`/`as_slice`/
//! `push`/`push_back`/`push_front`/`pop_back`/`pop_front`/`is_empty`/
//! `first`/`last`/`get`/`get_unchecked`/`contains`/`contains_key`/
//! `binary_search`/`split_at`/`chunks`/`windows`/`sort*`/`sorted*`/
//! `reverse`/`fill`/`swap`/`clone`. Lifted out of `eval_method_call`.

use std::sync::Arc;

use crate::ast::*;
use crate::token::Span;

use super::helpers::{eval_http_get, value_compare, value_compare_u64};
use super::value::{try_write_or_panic, EnumData, IteratorSource, OrdValue, Value};
use crate::interpreter::deep_clone_value;

impl<'a> super::Interpreter<'a> {
    pub(super) fn try_eval_seq_method(
        &mut self,
        method: &str,
        object: &Expr,
        obj: Value,
        args: &[CallArg],
        span: &Span,
        // Close-paren leaf span. The typechecker stashes the `Vec[T]` ELEMENT
        // type here for `sort` / `sorted` (B-2026-07-04-8) — `sort()`'s `Unit`
        // result (and `sorted()`'s `Vec[T]`) clobbers the receiver span, so this
        // non-aliased leaf is the reliable channel to element signedness.
        args_close_span: &Span,
    ) -> Option<Value> {
        match method {
            "len" => {
                return Some(match &obj {
                    Value::Array(rc) => Value::Int(rc.read().unwrap().len() as i64),
                    Value::Slice { len, .. } => Value::Int(*len as i64),
                    Value::String(s) => Value::Int(s.len() as i64),
                    // `CStr.len()` / `CString.len()` — source byte count,
                    // excluding the trailing NUL (design.md § C-String
                    // Literals). Both carry NUL-excluded bytes.
                    Value::CStr(b) => Value::Int(b.len() as i64),
                    Value::CString(b) => Value::Int(b.len() as i64),
                    Value::Map(m) => Value::Int(m.len() as i64),
                    Value::SortedSet(s) => Value::Int(s.len() as i64),
                    Value::SortedMap(m) => Value::Int(m.len() as i64),
                    Value::Set(s) => Value::Int(s.len() as i64),
                    // Note: Map also handled via Map.len() match above
                    _ => unreachable!(
                        "len() receiver at {}:{} was Value::{}; \
                         either an interpreter codepath produced the wrong receiver variant \
                         or the typechecker accepted .len() on a type without one",
                        span.line,
                        span.column,
                        obj.variant_name()
                    ),
                });
            }
            "chars" => {
                // `String.chars() -> Iterator[char]`. Snapshot the chars
                // eagerly into a Value::Iterator so adaptor chains (`map`,
                // `filter`, …) and `for c in s.chars()` go through the same
                // step-machine as other collections. Peer of design.md
                // § Character type (line 2299): the design pins `for c in s`
                // and `s.chars()` as semantic peers; both route here in the
                // tree-walk interpreter (the `for` site dispatches on
                // `Value::String` directly via the same `s.chars()` shape).
                return Some(match &obj {
                    Value::String(s) => {
                        let items: Vec<Value> = s.chars().map(Value::Char).collect();
                        Value::Iterator {
                            source: IteratorSource::Eager { items, cursor: 0 },
                            steps: Vec::new(),
                        }
                    }
                    _ => unreachable!(
                        "chars() receiver at {}:{} was Value::{} not String; \
                         either an interpreter codepath produced the wrong receiver variant \
                         or the typechecker accepted .chars() on a non-String",
                        span.line,
                        span.column,
                        obj.variant_name()
                    ),
                });
            }
            "bytes" => {
                // `String.bytes() -> Slice[u8]`. design.md § Character
                // type points programmers at `s.bytes()[i]` for O(1)
                // byte-positional access (vs `s.char_at(i)` for O(n)
                // Unicode-aware access). The tree-walk interpreter is
                // type-erased so each byte materializes as
                // `Value::Int(b as i64)`; codegen views the String's
                // existing `{ptr, len, cap}` buffer through a `{ptr, i64}`
                // slice header (zero-copy). Returning a fresh Slice with
                // its own storage matches the interpreter's existing
                // pattern for type-erased byte views; mutation through
                // the slice does not propagate back to the source String
                // (the return type is read-only `Slice[u8]`).
                return Some(match &obj {
                    Value::String(s) => {
                        let items: Vec<Value> =
                            s.as_bytes().iter().map(|b| Value::Int(*b as i64)).collect();
                        let len = items.len();
                        Value::Slice {
                            storage: Arc::new(std::sync::RwLock::new(items)),
                            start: 0,
                            len,
                            mutable: false,
                        }
                    }
                    _ => unreachable!(
                        "bytes() receiver at {}:{} was Value::{} not String; \
                         either an interpreter codepath produced the wrong receiver variant \
                         or the typechecker accepted .bytes() on a non-String",
                        span.line,
                        span.column,
                        obj.variant_name()
                    ),
                });
            }
            "starts_with" | "ends_with" => {
                // `String.starts_with(prefix) / ends_with(suffix) -> bool`. The
                // typechecker arm in `infer_str_method` enforces the String-arg
                // shape; this dispatch trusts the receiver is a Value::String and
                // the single arg evaluates to one too.
                if let (Value::String(s), [arg]) = (&obj, args) {
                    let needle_val = self.eval_expr_inner(&arg.value);
                    if let Value::String(needle) = needle_val {
                        let r = if method == "ends_with" {
                            s.ends_with(needle.as_str())
                        } else {
                            s.starts_with(needle.as_str())
                        };
                        return Some(Value::Bool(r));
                    }
                }
                return None;
            }
            "split" => {
                // `String.split(sep) -> Vec[String]` — split the receiver on
                // every non-overlapping occurrence of `sep` (a String or a
                // single char), returning the pieces incl. leading/trailing
                // empties (Rust `str::split` semantics). Used by the Weave CSV
                // ETL (examples/weave) to tokenize rows. Typechecker arm in
                // `stdlib_seq.rs::infer_str_method`; codegen arm in
                // `vec_method.rs`.
                if let Value::String(s) = &obj {
                    if let [a] = args {
                        let sep = match self.eval_expr_inner(&a.value) {
                            Value::String(sep) => sep,
                            Value::Char(c) => c.to_string(),
                            _ => return None,
                        };
                        let pieces: Vec<Value> = if sep.is_empty() {
                            // Empty separator: Rust yields "" between every char
                            // plus bookends; keep it simple and total — return
                            // the whole string as a single piece.
                            vec![Value::String(s.clone())]
                        } else {
                            s.split(sep.as_str())
                                .map(|piece| Value::String(piece.to_string()))
                                .collect()
                        };
                        return Some(Value::Array(Arc::new(std::sync::RwLock::new(pieces))));
                    }
                }
                return None;
            }
            "substring" => {
                // `String.substring(start) -> String` — bytes from `start` to end.
                // `String.substring(start, end) -> String` — bytes in `[start, end)`.
                // Byte offsets (matching `bytes()`); out-of-range / negative /
                // inverted bounds saturate to an empty String. Extraction is
                // byte-level (from_utf8_lossy) so a non-boundary index never panics.
                if let Value::String(s) = &obj {
                    let len = s.len() as i64;
                    let (start, end) = match args {
                        [a] => {
                            if let Value::Int(start) = self.eval_expr_inner(&a.value) {
                                (start, len)
                            } else {
                                return None;
                            }
                        }
                        [a, b] => {
                            let sa = self.eval_expr_inner(&a.value);
                            let sb = self.eval_expr_inner(&b.value);
                            if let (Value::Int(start), Value::Int(end)) = (sa, sb) {
                                (start, end)
                            } else {
                                return None;
                            }
                        }
                        _ => return None,
                    };
                    // One-arg contract: negative / past-end start → empty.
                    if start < 0 || start > len {
                        return Some(Value::String(String::new()));
                    }
                    let end = end.clamp(start, len);
                    let bytes = &s.as_bytes()[start as usize..end as usize];
                    return Some(Value::String(String::from_utf8_lossy(bytes).into_owned()));
                }
                return None;
            }
            "char_count" => {
                // `String.char_count() -> i64` — O(n) count of Unicode scalar
                // values (design.md § String), the Unicode-aware companion of
                // `len()`'s O(1) byte count. Codegen routes through
                // `karac_runtime_string_char_count`.
                if let Value::String(s) = &obj {
                    return Some(Value::Int(s.chars().count() as i64));
                }
                return None;
            }
            "char_at" => {
                // `String.char_at(i) -> Option[char]` — the i-th Unicode scalar
                // value, `None` past the end (or for a negative index). O(n);
                // codegen routes through `karac_runtime_string_char_at`.
                if let Value::String(s) = &obj {
                    if let [a] = args {
                        let idx = match self.eval_expr_inner(&a.value) {
                            Value::Int(i) => i,
                            _ => return None,
                        };
                        let hit = if idx >= 0 {
                            s.chars().nth(idx as usize)
                        } else {
                            None
                        };
                        return Some(match hit {
                            Some(c) => Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "Some".to_string(),
                                data: EnumData::Tuple(vec![Value::Char(c)]),
                            },
                            None => Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "None".to_string(),
                                data: EnumData::Unit,
                            },
                        });
                    }
                }
                return None;
            }
            "find" => {
                // `String.find(needle) -> Option[i64]` — byte offset of the
                // first occurrence of `needle` (String or char), else `None`.
                // Rust `str::find` returns the byte index, matching our
                // byte-offset contract (peer of `bytes()` / `substring`).
                if let Value::String(s) = &obj {
                    if let [a] = args {
                        let needle = match self.eval_expr_inner(&a.value) {
                            Value::String(n) => n,
                            Value::Char(c) => c.to_string(),
                            _ => return None,
                        };
                        return Some(match s.find(&needle) {
                            Some(b) => Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "Some".to_string(),
                                data: EnumData::Tuple(vec![Value::Int(b as i64)]),
                            },
                            None => Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "None".to_string(),
                                data: EnumData::Unit,
                            },
                        });
                    }
                }
                return None;
            }
            "slice" => {
                // `String.slice(start, end) -> StringSlice` — a borrowed view
                // over the half-open byte range `[start, end)`. In the
                // tree-walk interpreter a borrowed view is modeled as an owned
                // `String` copy of the range (clone semantics — reads are
                // byte-identical to the codegen borrow; same parity approach the
                // `Option[ref T]` accessors use). Bounds saturate like
                // `substring`: negative/past-end start → empty.
                if let Value::String(s) = &obj {
                    if let [a, b] = args {
                        let sa = self.eval_expr_inner(&a.value);
                        let sb = self.eval_expr_inner(&b.value);
                        if let (Value::Int(start), Value::Int(end)) = (sa, sb) {
                            let len = s.len() as i64;
                            if start < 0 || start > len {
                                return Some(Value::String(String::new()));
                            }
                            let end = end.clamp(start, len);
                            let bytes = &s.as_bytes()[start as usize..end as usize];
                            return Some(Value::String(
                                String::from_utf8_lossy(bytes).into_owned(),
                            ));
                        }
                    }
                }
                return None;
            }
            "repeat" => {
                // `String.repeat(n) -> String` — receiver concatenated `n`
                // times; `n <= 0` yields empty. Mirrors Rust's `str::repeat`
                // (and the codegen arm's malloc + n× memcpy).
                if let Value::String(s) = &obj {
                    if let [a] = args {
                        if let Value::Int(n) = self.eval_expr_inner(&a.value) {
                            let count = n.max(0) as usize;
                            return Some(Value::String(s.repeat(count)));
                        }
                    }
                }
                return None;
            }
            "trim" | "to_lowercase" | "to_uppercase" => {
                // Allocating String→String (typed in stdlib_seq.rs). Direct Rust
                // stdlib so the interpreter and the codegen runtime helpers
                // (`karac_string_{trim,to_lowercase,to_uppercase}`) compute the
                // identical full-Unicode result. Only a String receiver reaches
                // here (the SIMD vector path runs earlier; non-String falls
                // through to None).
                if let Value::String(s) = &obj {
                    let r = match method {
                        "trim" => s.trim().to_string(),
                        "to_lowercase" => s.to_lowercase(),
                        "to_uppercase" => s.to_uppercase(),
                        _ => unreachable!(),
                    };
                    return Some(Value::String(r));
                }
                return None;
            }
            "replace" => {
                // `String.replace(from, to) -> String` (typed in stdlib_seq.rs):
                // every non-overlapping `from` replaced with `to`, Rust
                // `str::replace`. The SIMD `Vector.replace` is dispatched by
                // `try_eval_vector_method`, which runs BEFORE this handler, so a
                // Vector receiver never reaches here — the String guard plus the
                // None fall-through keeps a non-String `replace` unhandled.
                if let Value::String(s) = &obj {
                    if let [from_a, to_a] = args {
                        let from_v = self.eval_expr_inner(&from_a.value);
                        let to_v = self.eval_expr_inner(&to_a.value);
                        if let (Value::String(from), Value::String(to)) = (from_v, to_v) {
                            return Some(Value::String(s.replace(&from, &to)));
                        }
                    }
                }
                return None;
            }
            "as_slice" | "as_slice_mut" => {
                // Slice 3 — produce a Value::Slice that shares the
                // source's `Arc<RwLock<Vec<Value>>>` storage. Mutation
                // through a `mut Slice[T]` propagates back to the source
                // because the storage is the same handle, and the
                // runtime guard fires on aliased writes via
                // try_write_or_panic.
                let mutable = method == "as_slice_mut";
                return Some(match &obj {
                    Value::Array(rc) => {
                        let len = rc.read().unwrap().len();
                        Value::Slice {
                            storage: rc.clone(),
                            start: 0,
                            len,
                            mutable,
                        }
                    }
                    Value::Slice {
                        storage,
                        start,
                        len,
                        ..
                    } => Value::Slice {
                        storage: storage.clone(),
                        start: *start,
                        len: *len,
                        mutable,
                    },
                    _ => unreachable!(
                        "{}() receiver at {}:{} was Value::{}; \
                         either an interpreter codepath produced the wrong receiver variant \
                         or the typechecker accepted .{}() on a non-sliceable type",
                        method,
                        span.line,
                        span.column,
                        obj.variant_name(),
                        method
                    ),
                });
            }
            "push" => {
                if let Value::Array(rc) = &obj {
                    let val = if let Some(arg) = args.first() {
                        self.eval_expr_inner(&arg.value)
                    } else {
                        Value::Unit
                    };
                    let label = match &object.kind {
                        ExprKind::Identifier(n) => n.clone(),
                        _ => "<value>".to_string(),
                    };
                    try_write_or_panic(rc, &label).push(val);
                    return Some(Value::Unit);
                }
                // String.push(char): append a single Unicode scalar's
                // UTF-8 bytes to the receiver. Receiver must be a named
                // binding so we can rebind it through `env.set` — the
                // codegen path mutates the {ptr,len,cap} struct in place,
                // the interpreter mirrors that semantically by rebuilding
                // the String. Tree-walk perf isn't a v1 goal so the O(L)
                // per call here doesn't repeat the kata-71 O(n²)
                // observation that motivated the surface.
                if let Value::String(s) = &obj {
                    let val = args.first().map(|arg| self.eval_expr_inner(&arg.value));
                    let c = match val {
                        Some(Value::Char(c)) => c,
                        _ => return Some(Value::Unit),
                    };
                    let mut next = s.clone();
                    next.push(c);
                    self.write_back_receiver(object, Value::String(next));
                    return Some(Value::Unit);
                }
            }
            // String.push_str(other: String): mutating-append other's
            // bytes to the receiver. Codegen path lives in
            // src/codegen/vec_method.rs (push_str arm) — same shape as
            // Vec.extend_from_slice. Surfaced as a gap during the kata 71
            // push(char) work — push_str typechecked + codegened cleanly
            // but the interpreter `karac run` path was missing dispatch
            // and panicked on the unreachable arm.
            "push_str" => {
                if let Value::String(s) = &obj {
                    let val = args.first().map(|arg| self.eval_expr_inner(&arg.value));
                    let other = match val {
                        Some(Value::String(other)) => other,
                        _ => return Some(Value::Unit),
                    };
                    let mut next = s.clone();
                    next.push_str(&other);
                    self.write_back_receiver(object, Value::String(next));
                    return Some(Value::Unit);
                }
            }
            // `extend_from_slice(other: Slice[T] / Vec[T] / Array[T,N])`
            // — bulk-append source elements to self. Mirrors codegen's
            // memcpy shape. Uses `deep_clone_value` per element so
            // nested-collection sources (Vec[Vec[T]]) don't alias the
            // source's inner storage into the destination — analog of
            // `Vec.filled`'s nested-independent-storage fix.
            "extend_from_slice" => {
                if let Value::Array(rc) = &obj {
                    let src_val = if let Some(arg) = args.first() {
                        self.eval_expr_inner(&arg.value)
                    } else {
                        Value::Unit
                    };
                    let elements: Vec<Value> = match src_val {
                        Value::Array(src_rc) => src_rc.read().unwrap().clone(),
                        Value::Slice {
                            storage,
                            start,
                            len,
                            ..
                        } => storage.read().unwrap()[start..start + len].to_vec(),
                        _ => Vec::new(),
                    };
                    let label = match &object.kind {
                        ExprKind::Identifier(n) => n.clone(),
                        _ => "<value>".to_string(),
                    };
                    let mut dest = try_write_or_panic(rc, &label);
                    for e in elements {
                        dest.push(deep_clone_value(&e));
                    }
                    return Some(Value::Unit);
                }
            }
            // ── VecDeque[T] surface (design.md). The runtime shape is
            //    the same `Value::Array` storage as `Vec[T]`; front-end
            //    ops translate to `Vec::insert(0, …)` / `Vec::remove(0)`
            //    (O(n) — acceptable for the tree-walk interpreter). The
            //    typechecker is permissive, so `Vec[T]` receivers can
            //    also reach these arms; valid Kāra source guards via
            //    receiver type. Routed through helpers to keep
            //    `eval_method_call`'s debug-mode stack frame compact.
            //
            //    `pop` is an alias for `pop_back` (Vec[T]'s stack-style
            //    pop), matching the codegen path in
            //    `src/codegen/vec_method.rs:300` which collapses
            //    `pop | pop_back | pop_front` into one arm and picks
            //    front-vs-back by name.
            "push_back" | "push_front" | "pop" | "pop_back" | "pop_front" => {
                if matches!(&obj, Value::Array(_)) {
                    return Some(self.eval_vec_deque_method(method, &obj, object, args));
                }
            }
            // `Vec[T].remove(idx: i64) -> T` — remove the element at `idx`,
            // shift the tail down by one, return the removed value. Mirrors
            // codegen's `vec_method.rs` remove arm (load + memmove + len--)
            // and the typechecker contract at `expr_method_call.rs` (returns
            // `T`, not `Option[T]`). Mutates the shared `Arc`-backed storage
            // in place, so a `mut ref Vec[T]` receiver writes back to the
            // caller's vector — the same aliasing the `push` arm relies on.
            // The design pins out-of-bounds as UB (no graceful Option), but
            // the tree-walk interpreter would otherwise panic deep inside
            // `Vec::remove`; we surface a clean runtime error at the call
            // site instead, matching the `index out of bounds` shape.
            "remove" => {
                if let Value::Array(rc) = &obj {
                    let idx_val = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    let idx = match idx_val {
                        Value::Int(i) => i as usize,
                        _ => return Some(Value::Unit),
                    };
                    let label = match &object.kind {
                        ExprKind::Identifier(n) => n.clone(),
                        _ => "<value>".to_string(),
                    };
                    let mut guard = try_write_or_panic(rc, &label);
                    let len = guard.len();
                    if idx >= len {
                        return Some(self.record_runtime_error(
                            format!("Vec.remove: index {} out of bounds (len {})", idx, len),
                            span,
                        ));
                    }
                    return Some(guard.remove(idx));
                }
            }
            // ── Slice[T] / Vec[T] / Array[T,N] shared read-only methods ──────────
            // The interpreter uses Value::Array for all sequence types (Vec,
            // Array, Slice). Each arm only returns when `obj` IS a
            // Value::Array; otherwise it falls through to the impl-block
            // lookup so user-defined structs with the same method name
            // (`struct Counter { fn get(self) ... }`) still resolve correctly.
            "is_empty" => {
                if let Value::Array(ref rc) = obj {
                    return Some(Value::Bool(rc.read().unwrap().is_empty()));
                }
                if let Value::Slice { len, .. } = &obj {
                    return Some(Value::Bool(*len == 0));
                }
                if let Value::String(ref s) = obj {
                    return Some(Value::Bool(s.is_empty()));
                }
                if let Value::SortedSet(ref s) = obj {
                    return Some(Value::Bool(s.is_empty()));
                }
                if let Value::Set(ref s) = obj {
                    return Some(Value::Bool(s.is_empty()));
                }
                if let Value::Map(ref m) = obj {
                    return Some(Value::Bool(m.is_empty()));
                }
                if let Value::SortedMap(ref m) = obj {
                    return Some(Value::Bool(m.is_empty()));
                }
                if let Value::CStr(ref b) = obj {
                    return Some(Value::Bool(b.is_empty()));
                }
                if let Value::CString(ref b) = obj {
                    return Some(Value::Bool(b.is_empty()));
                }
            }
            "as_bytes" => {
                // `CStr.as_bytes() -> Slice[u8]` — the N source bytes,
                // excluding the trailing NUL. Type-erased `Value::Int`
                // bytes in fresh storage, the same pattern as
                // `String.bytes()` above (the return type is read-only
                // `Slice[u8]`, so the copy is unobservable).
                if let Value::CStr(ref b) = obj {
                    let items: Vec<Value> = b.iter().map(|b| Value::Int(*b as i64)).collect();
                    let len = items.len();
                    return Some(Value::Slice {
                        storage: Arc::new(std::sync::RwLock::new(items)),
                        start: 0,
                        len,
                        mutable: false,
                    });
                }
                // `CString.as_bytes()` — same NUL-excluded byte view as `CStr`.
                if let Value::CString(ref b) = obj {
                    let items: Vec<Value> = b.iter().map(|b| Value::Int(*b as i64)).collect();
                    let len = items.len();
                    return Some(Value::Slice {
                        storage: Arc::new(std::sync::RwLock::new(items)),
                        start: 0,
                        len,
                        mutable: false,
                    });
                }
            }
            "as_ptr" => {
                // `CStr.as_ptr() -> *const u8` — the tree-walk interpreter
                // has no raw-pointer representation, and nothing in
                // interpreted mode can consume one (extern "C" / host fn
                // bodies are link-time constructs). Reject loudly at the
                // producer rather than letting a meaningless integer flow
                // into FFI-shaped code. design.md § Interpreter parity
                // scope: the interpreter validates semantics; raw-pointer
                // identity is a compiled-mode (memory representation)
                // concern.
                if let Value::CStr(_) | Value::CString(_) = obj {
                    panic!(
                        "{}.as_ptr() at {}:{} is not supported under `karac run`: \
                         the tree-walk interpreter has no raw-pointer representation \
                         (pointers exist for FFI/host-fn boundaries, which interpreted \
                         mode cannot call). Compile with `karac build` instead.",
                        obj.variant_name(),
                        span.line,
                        span.column
                    );
                }
            }
            "first" => {
                let elem = match &obj {
                    Value::Array(rc) => rc.read().unwrap().first().cloned(),
                    Value::Slice {
                        storage,
                        start,
                        len,
                        ..
                    } => {
                        if *len > 0 {
                            Some(storage.read().unwrap()[*start].clone())
                        } else {
                            None
                        }
                    }
                    // Not a seq receiver — fall through to the impl-block /
                    // missing-dispatch tail instead of swallowing to Unit.
                    _ => return None,
                };
                return Some(match elem {
                    Some(v) => Value::EnumVariant {
                        enum_name: "Option".to_string(),
                        variant: "Some".to_string(),
                        data: EnumData::Tuple(vec![v]),
                    },
                    None => Value::EnumVariant {
                        enum_name: "Option".to_string(),
                        variant: "None".to_string(),
                        data: EnumData::Unit,
                    },
                });
            }
            "last" => {
                let elem = match &obj {
                    Value::Array(rc) => rc.read().unwrap().last().cloned(),
                    Value::Slice {
                        storage,
                        start,
                        len,
                        ..
                    } => {
                        if *len > 0 {
                            Some(storage.read().unwrap()[*start + *len - 1].clone())
                        } else {
                            None
                        }
                    }
                    // Not a seq receiver — fall through to the impl-block /
                    // missing-dispatch tail instead of swallowing to Unit.
                    _ => return None,
                };
                return Some(match elem {
                    Some(v) => Value::EnumVariant {
                        enum_name: "Option".to_string(),
                        variant: "Some".to_string(),
                        data: EnumData::Tuple(vec![v]),
                    },
                    None => Value::EnumVariant {
                        enum_name: "Option".to_string(),
                        variant: "None".to_string(),
                        data: EnumData::Unit,
                    },
                });
            }
            "get_unchecked" => {
                // Direct-index read with no bounds check. Codegen returns
                // garbage on OOB; the interpreter mirror panics with the
                // standard out-of-bounds message rather than return Value::Unit,
                // so misuse surfaces immediately under `karac run` even though
                // the codegen path is "UB" by design.
                let array_view: Option<Vec<Value>> = match &obj {
                    Value::Array(rc) => Some(rc.read().unwrap().clone()),
                    Value::Slice {
                        storage,
                        start,
                        len,
                        ..
                    } => Some(storage.read().unwrap()[*start..*start + *len].to_vec()),
                    _ => None,
                };
                if let Some(v) = array_view {
                    let idx = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Int(0));
                    if let Value::Int(i) = idx {
                        let i = i as usize;
                        if i >= v.len() {
                            panic!(
                                "Vec.get_unchecked: index {} out of bounds (len={}) — \
                                 caller broke the unsafe precondition",
                                i,
                                v.len()
                            );
                        }
                        return Some(v[i].clone());
                    }
                    return Some(Value::Unit);
                }
                // Not a seq receiver — fall through (see `first`).
                return None;
            }
            "get" => {
                let array_view: Option<Vec<Value>> = match &obj {
                    Value::Array(rc) => Some(rc.read().unwrap().clone()),
                    Value::Slice {
                        storage,
                        start,
                        len,
                        ..
                    } => Some(storage.read().unwrap()[*start..*start + *len].to_vec()),
                    _ => None,
                };
                if let Some(v) = array_view {
                    let idx = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Int(0));
                    return Some(if let Value::Int(i) = idx {
                        let i = i as usize;
                        if i < v.len() {
                            Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "Some".to_string(),
                                data: EnumData::Tuple(vec![v[i].clone()]),
                            }
                        } else {
                            Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "None".to_string(),
                                data: EnumData::Unit,
                            }
                        }
                    } else {
                        Value::Unit
                    });
                }
                if let Value::Map(ref m) = obj {
                    let key = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    return Some(match m.iter().find(|(k, _)| *k == key) {
                        Some((_, v)) => Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "Some".to_string(),
                            data: EnumData::Tuple(vec![v.clone()]),
                        },
                        None => Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "None".to_string(),
                            data: EnumData::Unit,
                        },
                    });
                }
                if let Value::SortedMap(ref m) = obj {
                    let key = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    return Some(match m.get(&OrdValue(key)) {
                        Some(v) => Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "Some".to_string(),
                            data: EnumData::Tuple(vec![v.clone()]),
                        },
                        None => Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "None".to_string(),
                            data: EnumData::Unit,
                        },
                    });
                }
                if let Value::Struct { ref name, .. } = obj {
                    if name == "Client" {
                        let url = args
                            .first()
                            .map(|a| match self.eval_expr_inner(&a.value) {
                                Value::String(s) => s,
                                _ => String::new(),
                            })
                            .unwrap_or_default();
                        return Some(eval_http_get(&url));
                    }
                }
            }
            "contains" => {
                if let Value::Array(ref rc) = obj {
                    let v = rc.read().unwrap();
                    let needle = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    return Some(Value::Bool(v.contains(&needle)));
                }
                if let Value::String(ref s) = obj {
                    let needle = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    if let Value::String(sub) = needle {
                        return Some(Value::Bool(s.contains(sub.as_str())));
                    }
                    return Some(Value::Bool(false));
                }
                if let Value::SortedSet(ref s) = obj {
                    let needle = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    return Some(Value::Bool(s.contains_key(&OrdValue(needle))));
                }
                if let Value::Set(ref s) = obj {
                    let needle = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    return Some(Value::Bool(s.contains(&needle)));
                }
            }
            "contains_key" => {
                if let Value::Map(ref m) = obj {
                    let key = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    return Some(Value::Bool(m.iter().any(|(k, _)| *k == key)));
                }
                if let Value::SortedMap(ref m) = obj {
                    let key = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    return Some(Value::Bool(m.contains_key(&OrdValue(key))));
                }
            }
            "binary_search" => {
                if let Value::Array(ref rc) = obj {
                    let v = rc.read().unwrap();
                    let needle = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Unit);
                    return Some(
                        match v.binary_search_by(|probe| value_compare(probe, &needle)) {
                            Ok(i) => Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "Some".to_string(),
                                data: EnumData::Tuple(vec![Value::Int(i as i64)]),
                            },
                            Err(_) => Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "None".to_string(),
                                data: EnumData::Unit,
                            },
                        },
                    );
                }
            }
            "split_at" => {
                if let Value::Array(ref rc) = obj {
                    let v = rc.read().unwrap();
                    let idx = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Int(0));
                    return Some(if let Value::Int(i) = idx {
                        let i = (i as usize).min(v.len());
                        let left = Value::array_of(v[..i].to_vec());
                        let right = Value::array_of(v[i..].to_vec());
                        Value::Tuple(vec![left, right])
                    } else {
                        Value::Unit
                    });
                }
            }
            "chunks" => {
                if let Value::Array(ref rc) = obj {
                    let v = rc.read().unwrap();
                    let n = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Int(1));
                    if let Value::Int(n) = n {
                        let n = if n > 0 { n as usize } else { 1 };
                        let chunks: Vec<Value> =
                            v.chunks(n).map(|c| Value::array_of(c.to_vec())).collect();
                        return Some(Value::array_of(chunks));
                    }
                }
                // Iterator-trait variant — lazy chunks; wraps the
                // receiver into an `IteratorSource::Chunks`. Each
                // pull yields a freshly allocated `Vec[T]`. n is
                // clamped to `n.max(1)`, matching `step_by`'s policy.
                if matches!(obj, Value::Iterator { .. }) {
                    let Some(arg) = args.first() else {
                        return Some(self.record_runtime_error(
                            "Iterator.chunks() requires an integer argument".to_string(),
                            span,
                        ));
                    };
                    let n = match self.eval_expr_inner(&arg.value) {
                        Value::Int(n) => n.max(1) as usize,
                        v => {
                            return Some(self.record_runtime_error(
                                format!("Iterator.chunks() expects an integer; got {}", v),
                                span,
                            ));
                        }
                    };
                    return Some(Value::Iterator {
                        source: IteratorSource::Chunks {
                            inner: Box::new(obj),
                            n,
                            exhausted: false,
                        },
                        steps: Vec::new(),
                    });
                }
            }
            "windows" => {
                if let Value::Array(ref rc) = obj {
                    let v = rc.read().unwrap();
                    let n = args
                        .first()
                        .map(|a| self.eval_expr_inner(&a.value))
                        .unwrap_or(Value::Int(1));
                    if let Value::Int(n) = n {
                        let n = if n > 0 && (n as usize) <= v.len() {
                            n as usize
                        } else {
                            return Some(Value::array_of(vec![]));
                        };
                        let wins: Vec<Value> =
                            v.windows(n).map(|w| Value::array_of(w.to_vec())).collect();
                        return Some(Value::array_of(wins));
                    }
                }
                // Iterator-trait variant — lazy sliding window; each
                // pull yields a freshly cloned buffer of size n. n=0
                // and n>source-length both produce zero windows; we
                // clamp to n.max(1) at the dispatch site so the
                // first-prime-pull naturally trips the
                // sticky-exhausted path on a too-small source.
                if matches!(obj, Value::Iterator { .. }) {
                    let Some(arg) = args.first() else {
                        return Some(self.record_runtime_error(
                            "Iterator.windows() requires an integer argument".to_string(),
                            span,
                        ));
                    };
                    let n = match self.eval_expr_inner(&arg.value) {
                        Value::Int(n) => n.max(1) as usize,
                        v => {
                            return Some(self.record_runtime_error(
                                format!("Iterator.windows() expects an integer; got {}", v),
                                span,
                            ));
                        }
                    };
                    return Some(Value::Iterator {
                        source: IteratorSource::Windows {
                            inner: Box::new(obj),
                            n,
                            buffer: Vec::with_capacity(n),
                            primed: false,
                            exhausted: false,
                        },
                        steps: Vec::new(),
                    });
                }
            }
            "sort" => {
                if let Value::Array(ref rc) = obj {
                    let label = match &object.kind {
                        ExprKind::Identifier(n) => n.clone(),
                        _ => "<value>".to_string(),
                    };
                    // `Vec[u64]` / `Vec[usize]` orders unsigned so a value ≥ 2⁶³
                    // sorts after the positives, not to the front as a negative
                    // i64 (B-2026-07-04-8). `sort()` is typed `Unit`, which
                    // clobbers the receiver span, so element signedness comes
                    // from the element type the typechecker stashes at the
                    // non-aliased close-paren leaf.
                    let cmp = if self.span_type_is_unsigned64(args_close_span) {
                        value_compare_u64
                    } else {
                        value_compare
                    };
                    try_write_or_panic(rc, &label).sort_by(cmp);
                    return Some(Value::Unit);
                }
            }
            "sort_by" => {
                // sort_by(|a, b| -> Ordering) — snapshot the vec so the user
                // closure can re-enter the interpreter freely (std::sync::RwLock
                // is non-reentrant on the same thread), sort the snapshot via
                // the user comparator, then write the result back.
                if args.len() != 1 {
                    panic!(
                        "sort_by expects 1 argument (comparator closure), got {}",
                        args.len()
                    );
                }
                let cmp_val = self.eval_expr_inner(&args[0].value);
                if let Value::Array(ref rc) = obj {
                    let label = match &object.kind {
                        ExprKind::Identifier(n) => n.clone(),
                        _ => "<value>".to_string(),
                    };
                    let mut snapshot = rc.read().unwrap().clone();
                    snapshot.sort_by(|a, b| {
                        self.invoke_value_comparator(&cmp_val, a.clone(), b.clone(), "sort_by")
                    });
                    *try_write_or_panic(rc, &label) = snapshot;
                    return Some(Value::Unit);
                }
            }
            "sorted" => {
                if let Value::String(ref s) = obj {
                    let mut chars: Vec<char> = s.chars().collect();
                    chars.sort_unstable();
                    return Some(Value::String(chars.into_iter().collect()));
                }
                if let Value::Array(ref rc) = obj {
                    let mut v = rc.read().unwrap().clone();
                    // Unsigned order for `Vec[u64]` / `Vec[usize]` (B-2026-07-04-8);
                    // element signedness from the stashed close-paren leaf.
                    let cmp = if self.span_type_is_unsigned64(args_close_span) {
                        value_compare_u64
                    } else {
                        value_compare
                    };
                    v.sort_by(cmp);
                    return Some(Value::array_of(v));
                }
            }
            "sorted_by" => {
                // sorted_by(|a, b| -> Ordering) — same snapshot-then-sort
                // pattern as `sort_by`, but returns a new collection instead
                // of mutating in place. The `.clone()` releases the read
                // guard before the comparator runs, so the user closure can
                // re-enter the interpreter freely.
                if args.len() != 1 {
                    panic!(
                        "sorted_by expects 1 argument (comparator closure), got {}",
                        args.len()
                    );
                }
                let cmp_val = self.eval_expr_inner(&args[0].value);
                if let Value::String(ref s) = obj {
                    let mut chars: Vec<char> = s.chars().collect();
                    chars.sort_by(|a, b| {
                        self.invoke_value_comparator(
                            &cmp_val,
                            Value::Char(*a),
                            Value::Char(*b),
                            "sorted_by",
                        )
                    });
                    return Some(Value::String(chars.into_iter().collect()));
                }
                if let Value::Array(ref rc) = obj {
                    let mut v = rc.read().unwrap().clone();
                    v.sort_by(|a, b| {
                        self.invoke_value_comparator(&cmp_val, a.clone(), b.clone(), "sorted_by")
                    });
                    return Some(Value::array_of(v));
                }
            }
            "sort_by_key" => {
                // sort_by_key(|t| -> K) where K: Ord — precompute keys once
                // (Rust's `sort_by_key` semantics: each element's key is
                // computed exactly once, not on every comparator invocation),
                // sort the (key, value) pairs by key via `value_compare`, write
                // the values back. Snapshot-then-replace mirrors `sort_by` to
                // keep the user closure free to re-enter the interpreter.
                if args.len() != 1 {
                    panic!(
                        "sort_by_key expects 1 argument (key closure), got {}",
                        args.len()
                    );
                }
                let key_val = self.eval_expr_inner(&args[0].value);
                if let Value::Array(ref rc) = obj {
                    let label = match &object.kind {
                        ExprKind::Identifier(n) => n.clone(),
                        _ => "<value>".to_string(),
                    };
                    let snapshot = rc.read().unwrap().clone();
                    let mut keyed: Vec<(Value, Value)> = snapshot
                        .into_iter()
                        .map(|v| {
                            let k = self.invoke_function_value(key_val.clone(), vec![v.clone()]);
                            (k, v)
                        })
                        .collect();
                    keyed.sort_by(|(k1, _), (k2, _)| value_compare(k1, k2));
                    let sorted: Vec<Value> = keyed.into_iter().map(|(_, v)| v).collect();
                    *try_write_or_panic(rc, &label) = sorted;
                    return Some(Value::Unit);
                }
            }
            "sorted_by_key" => {
                // sorted_by_key(|t| -> K) where K: Ord — same precompute-keys
                // pattern as `sort_by_key`, but returns a new Vec instead of
                // mutating in place.
                if args.len() != 1 {
                    panic!(
                        "sorted_by_key expects 1 argument (key closure), got {}",
                        args.len()
                    );
                }
                let key_val = self.eval_expr_inner(&args[0].value);
                if let Value::Array(ref rc) = obj {
                    let snapshot = rc.read().unwrap().clone();
                    let mut keyed: Vec<(Value, Value)> = snapshot
                        .into_iter()
                        .map(|v| {
                            let k = self.invoke_function_value(key_val.clone(), vec![v.clone()]);
                            (k, v)
                        })
                        .collect();
                    keyed.sort_by(|(k1, _), (k2, _)| value_compare(k1, k2));
                    let sorted: Vec<Value> = keyed.into_iter().map(|(_, v)| v).collect();
                    return Some(Value::array_of(sorted));
                }
            }
            "reverse" => {
                if let Value::Array(ref rc) = obj {
                    let label = match &object.kind {
                        ExprKind::Identifier(n) => n.clone(),
                        _ => "<value>".to_string(),
                    };
                    try_write_or_panic(rc, &label).reverse();
                    return Some(Value::Unit);
                }
            }
            "fill" => {
                let fill_val = args
                    .first()
                    .map(|a| self.eval_expr_inner(&a.value))
                    .unwrap_or(Value::Unit);
                if let Value::Array(ref rc) = obj {
                    let label = match &object.kind {
                        ExprKind::Identifier(n) => n.clone(),
                        _ => "<value>".to_string(),
                    };
                    let mut guard = try_write_or_panic(rc, &label);
                    for elem in guard.iter_mut() {
                        *elem = fill_val.clone();
                    }
                    return Some(Value::Unit);
                }
            }
            "swap" => {
                let i = args
                    .first()
                    .map(|a| self.eval_expr_inner(&a.value))
                    .unwrap_or(Value::Int(0));
                let j = args
                    .get(1)
                    .map(|a| self.eval_expr_inner(&a.value))
                    .unwrap_or(Value::Int(0));
                if let (Value::Int(i_val), Value::Int(j_val)) = (i, j) {
                    if let Value::Array(ref rc) = obj {
                        let label = match &object.kind {
                            ExprKind::Identifier(n) => n.clone(),
                            _ => "<value>".to_string(),
                        };
                        let mut guard = try_write_or_panic(rc, &label);
                        let i = i_val as usize;
                        let j = j_val as usize;
                        if i < guard.len() && j < guard.len() {
                            guard.swap(i, j);
                        }
                        return Some(Value::Unit);
                    }
                } else {
                    // consume obj to avoid borrow-after-move
                    let _ = obj;
                }
            }
            // clone() — Sender creates an additional producer sharing the
            // same queue Arc. For collection types (Array/String/Map/Set/
            // SortedSet) the canonical Clone impl is a structural deep
            // copy: each `Value` variant is itself `Clone` so
            // `obj.clone()` does the right thing without per-type
            // unrolling. Non-Clone payloads (closures, iterators, refs,
            // entries, shared cells) fall through; the typechecker
            // rejects `clone()` on those receivers via `clone_self_type_for`.
            "clone" => {
                if let Value::Sender(ref queue) = obj {
                    return Some(Value::Sender(Arc::clone(queue)));
                }
                match &obj {
                    Value::Array(rc) => {
                        // Deep copy — clone the inner Vec into a fresh
                        // shared cell so the clone has independent
                        // storage. Slice 3: this matches the v1
                        // value-semantics rule that `arr.clone()`
                        // produces a structurally independent array.
                        return Some(Value::array_of(rc.read().unwrap().clone()));
                    }
                    Value::Slice {
                        storage,
                        start,
                        len,
                        ..
                    } => {
                        return Some(Value::array_of(
                            storage.read().unwrap()[*start..*start + *len].to_vec(),
                        ));
                    }
                    Value::String(s) => return Some(Value::String(s.clone())),
                    Value::Map(m) => return Some(Value::Map(m.clone())),
                    Value::Set(s) => return Some(Value::Set(s.clone())),
                    Value::SortedSet(s) => return Some(Value::SortedSet(s.clone())),
                    Value::SortedMap(m) => return Some(Value::SortedMap(m.clone())),
                    _ => {}
                }
            }

            _ => {}
        }
        None
    }

    /// Instance methods on `Value::Vector` (design.md § Portable SIMD, slices
    /// 2 / 2b): `dot` + the `reduce_*` Vector→scalar folds. Returns `Some(scalar)` when
    /// `method` matches and the receiver is a vector; `None` otherwise (fall
    /// through). Folds reuse `eval_binary` so each lane uses the exact scalar
    /// Int/Float semantics — keeping interpreter output identical to codegen.
    pub(super) fn try_eval_vector_method(
        &mut self,
        method: &str,
        object: &Expr,
        obj: &Value,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        // Borrow-check the receiver first so a non-Vector receiver (e.g. an
        // `Array`/`String` on the dispatch hot path) is rejected WITHOUT a
        // deep clone (B-2026-06-07-4a); clone the confirmed (N-lane) Vector
        // only when it matches.
        let Value::Vector(lanes) = obj else {
            return None;
        };
        let lanes = lanes.clone();
        match method {
            // Horizontal folds: combine all lanes with the matching scalar op.
            // The typechecker guarantees N >= 1 (and an integer element for the
            // bitwise folds), so `lanes` is non-empty.
            "reduce_sum" | "reduce_product" | "reduce_and" | "reduce_or" | "reduce_xor" => {
                let fold_op = match method {
                    "reduce_sum" => BinOp::Add,
                    "reduce_product" => BinOp::Mul,
                    "reduce_and" => BinOp::BitAnd,
                    "reduce_or" => BinOp::BitOr,
                    _ => BinOp::BitXor, // reduce_xor
                };
                let mut acc = lanes.first().cloned()?;
                for lane in lanes.into_iter().skip(1) {
                    acc = self.eval_binary(&fold_op, acc, lane, span, false);
                }
                Some(acc)
            }
            // Horizontal min/max. Element is numeric (signed-int / unsigned-int
            // / float). The signed/ordered `<`/`>` compare via `eval_binary`
            // matches codegen for signed and float lanes. For an unsigned
            // element the type-erased `Value::Int(i64)` lanes must compare as
            // `u64` — a signed compare would invert order for lanes with the
            // high bit set — so read the element signedness off the receiver's
            // recorded type and pick the `u64` compare there.
            "reduce_min" | "reduce_max" => {
                let cmp_op = if method == "reduce_min" {
                    BinOp::Lt
                } else {
                    BinOp::Gt
                };
                let is_unsigned = self
                    .typecheck_result
                    .expr_types
                    .get(&crate::resolver::SpanKey::from_span(&object.span))
                    .is_some_and(|t| {
                        matches!(t, crate::typechecker::Type::Vector { element, .. }
                            if matches!(**element, crate::typechecker::Type::UInt(_)))
                    });
                let mut acc = lanes.first().cloned()?;
                for lane in lanes.into_iter().skip(1) {
                    let keep_acc = if is_unsigned {
                        match (&acc, &lane) {
                            (Value::Int(a), Value::Int(b)) => {
                                let (ua, ub) = (*a as u64, *b as u64);
                                if method == "reduce_min" {
                                    ua < ub
                                } else {
                                    ua > ub
                                }
                            }
                            _ => false,
                        }
                    } else {
                        matches!(
                            self.eval_binary(&cmp_op, acc.clone(), lane.clone(), span, false),
                            Value::Bool(true)
                        )
                    };
                    acc = if keep_acc { acc } else { lane };
                }
                Some(acc)
            }
            // Dot product: element-wise product of the two vectors, summed.
            "dot" => {
                let other = self.eval_expr_inner(&args[0].value);
                let Value::Vector(rhs) = other else {
                    return Some(
                        self.record_runtime_error(
                            "dot expects a vector argument".to_string(),
                            span,
                        ),
                    );
                };
                let mut acc: Option<Value> = None;
                for (x, y) in lanes.into_iter().zip(rhs) {
                    let prod = self.eval_binary(&BinOp::Mul, x, y, span, false);
                    acc = Some(match acc {
                        None => prod,
                        Some(a) => self.eval_binary(&BinOp::Add, a, prod, span, false),
                    });
                }
                acc
            }
            // Cross product (3D only — the typechecker guarantees N == 3 and
            // a same-typed argument). `c = a × b`:
            //   c0 = a1*b2 - a2*b1,  c1 = a2*b0 - a0*b2,  c2 = a0*b1 - a1*b0
            // Each lane uses `eval_binary` so the scalar Int/Float semantics
            // match codegen exactly.
            "cross" => {
                let other = self.eval_expr_inner(&args[0].value);
                let Value::Vector(rhs) = other else {
                    return Some(self.record_runtime_error(
                        "cross expects a vector argument".to_string(),
                        span,
                    ));
                };
                if lanes.len() != 3 || rhs.len() != 3 {
                    return Some(self.record_runtime_error(
                        "cross is defined only for 3-lane vectors".to_string(),
                        span,
                    ));
                }
                // c_lane = p*q - r*s
                let comp = |me: &mut Self, p: Value, q: Value, r: Value, s: Value| -> Value {
                    let pq = me.eval_binary(&BinOp::Mul, p, q, span, false);
                    let rs = me.eval_binary(&BinOp::Mul, r, s, span, false);
                    me.eval_binary(&BinOp::Sub, pq, rs, span, false)
                };
                let (a0, a1, a2) = (lanes[0].clone(), lanes[1].clone(), lanes[2].clone());
                let (b0, b1, b2) = (rhs[0].clone(), rhs[1].clone(), rhs[2].clone());
                let c0 = comp(self, a1.clone(), b2.clone(), a2.clone(), b1.clone());
                let c1 = comp(self, a2, b0.clone(), a0.clone(), b2);
                let c2 = comp(self, a0, b1, a1, b0);
                Some(Value::Vector(vec![c0, c1, c2]))
            }
            // `mask.select(a, b)` — `lanes` is the mask (a `Value::Bool` per
            // lane, produced by a vector comparison); pick `a[i]` where the
            // lane is true, else `b[i]`. The typechecker guarantees both args
            // are same-typed vectors with the mask's lane count.
            "select" => {
                let a = self.eval_expr_inner(&args[0].value);
                let b = self.eval_expr_inner(&args[1].value);
                let (Value::Vector(av), Value::Vector(bv)) = (a, b) else {
                    return Some(self.record_runtime_error(
                        "select expects two vector arguments".to_string(),
                        span,
                    ));
                };
                let out: Vec<Value> = lanes
                    .into_iter()
                    .zip(av.into_iter().zip(bv))
                    .map(|(m, (x, y))| if matches!(m, Value::Bool(true)) { x } else { y })
                    .collect();
                Some(Value::Vector(out))
            }
            // Lane permutations (design.md § Portable SIMD, "Lane shuffling").
            // Pure index permutations — same mapping as codegen so the two
            // backends agree lane-for-lane. `reverse`: lane i ← N-1-i;
            // `rotate_lanes_left(k)`: lane i ← (i+k) mod N; `rotate_lanes_right`:
            // lane i ← (i+N-k) mod N. The rotate amount is a non-negative
            // integer literal (typechecker-guaranteed).
            "reverse" => {
                let mut out = lanes;
                out.reverse();
                Some(Value::Vector(out))
            }
            "rotate_lanes_left" | "rotate_lanes_right" => {
                let nn = lanes.len();
                if nn == 0 {
                    return Some(Value::Vector(lanes));
                }
                let amt = match &args[0].value.kind {
                    ExprKind::Integer(v, _) => *v,
                    _ => {
                        return Some(self.record_runtime_error(
                            format!("{method} amount must be a compile-time integer literal"),
                            span,
                        ))
                    }
                };
                let shift = amt.rem_euclid(nn as i64) as usize;
                let out: Vec<Value> = (0..nn)
                    .map(|i| {
                        let src = if method == "rotate_lanes_left" {
                            (i + shift) % nn
                        } else {
                            (i + nn - shift) % nn
                        };
                        lanes[src].clone()
                    })
                    .collect();
                Some(Value::Vector(out))
            }
            // `v.replace(i, x)` — return a new vector with lane `i` set to `x`.
            // Runtime-bounds-checked (parity with codegen's panic on OOB).
            "replace" => {
                let idx_v = self.eval_expr_inner(&args[0].value);
                let x = self.eval_expr_inner(&args[1].value);
                let Value::Int(i) = idx_v else {
                    return Some(self.record_runtime_error(
                        "replace index must be an integer".to_string(),
                        span,
                    ));
                };
                let mut out = lanes;
                if i < 0 || (i as usize) >= out.len() {
                    return Some(self.record_runtime_error(
                        "vector lane index out of bounds".to_string(),
                        span,
                    ));
                }
                out[i as usize] = x;
                Some(Value::Vector(out))
            }
            // `v.shuffle([i0..i_{M-1}])` — gather source lanes by a compile-time
            // index list into a fresh M-lane vector (parity with codegen; the
            // typechecker has range-checked each literal index into `[0, N)`).
            "shuffle" => {
                let ExprKind::ArrayLiteral(items) = &args[0].value.kind else {
                    return Some(self.record_runtime_error(
                        "shuffle requires a compile-time array literal of lane indices".to_string(),
                        span,
                    ));
                };
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    let src = match &it.kind {
                        ExprKind::Integer(v, _) if *v >= 0 => *v as usize,
                        _ => {
                            return Some(self.record_runtime_error(
                                "shuffle index must be a non-negative integer literal".to_string(),
                                span,
                            ))
                        }
                    };
                    if src >= lanes.len() {
                        return Some(self.record_runtime_error(
                            "vector lane index out of bounds".to_string(),
                            span,
                        ));
                    }
                    out.push(lanes[src].clone());
                }
                Some(Value::Vector(out))
            }
            // `v.store_masked(slice, mask)` — write each active lane through the
            // mutable slice's shared storage (parity with codegen). Lane `i`
            // active iff `mask[i]`; an active lane past the slice length panics,
            // an inactive lane leaves the slice untouched. Returns unit.
            "store_masked" => {
                let slice_v = self.eval_expr_inner(&args[0].value);
                let mask_v = self.eval_expr_inner(&args[1].value);
                // The destination is a `mut Slice[T]`. When a `mut` array is
                // bound to that param, the interpreter forwards it as a
                // `Value::Array` (shared `Arc` storage, offset 0) rather than a
                // `Value::Slice`; accept both so writes reach the backing store.
                let (storage, start, slen) = match slice_v {
                    Value::Slice {
                        storage,
                        start,
                        len,
                        ..
                    } => (storage, start, len),
                    Value::Array(rc) => {
                        let len = rc.read().unwrap().len();
                        (rc, 0, len)
                    }
                    other => {
                        return Some(self.record_runtime_error(
                            format!(
                                "store_masked expects a mut Slice argument, got `{}`",
                                other.variant_name()
                            ),
                            span,
                        ))
                    }
                };
                let Value::Vector(mask) = mask_v else {
                    return Some(self.record_runtime_error(
                        "store_masked expects a Vector[bool, N] mask".to_string(),
                        span,
                    ));
                };
                let mut guard = storage.write().unwrap();
                for (i, lane) in lanes.iter().enumerate() {
                    if matches!(mask.get(i), Some(Value::Bool(true))) {
                        if i >= slen {
                            drop(guard);
                            return Some(self.record_runtime_error(
                                "store_masked: active lane index out of bounds".to_string(),
                                span,
                            ));
                        }
                        guard[start + i] = lane.clone();
                    }
                }
                Some(Value::Unit)
            }
            // `v.scatter(slice, indices)` — write each lane `v[i]` to
            // `slice[indices[i]]` through the mutable slice's shared storage
            // (parity with codegen; the write mirror of `gather`). Every lane
            // is active; each index is bounds-checked (panic on OOB).
            "scatter" => {
                let slice_v = self.eval_expr_inner(&args[0].value);
                let indices_v = self.eval_expr_inner(&args[1].value);
                let (storage, start, slen) = match slice_v {
                    Value::Slice {
                        storage,
                        start,
                        len,
                        ..
                    } => (storage, start, len),
                    Value::Array(rc) => {
                        let len = rc.read().unwrap().len();
                        (rc, 0, len)
                    }
                    other => {
                        return Some(self.record_runtime_error(
                            format!(
                                "scatter expects a mut Slice argument, got `{}`",
                                other.variant_name()
                            ),
                            span,
                        ))
                    }
                };
                let Value::Vector(indices) = indices_v else {
                    return Some(self.record_runtime_error(
                        "scatter expects an integer index vector".to_string(),
                        span,
                    ));
                };
                let mut guard = storage.write().unwrap();
                for (lane, idx_v) in lanes.iter().zip(indices.iter()) {
                    let Value::Int(idx) = idx_v else {
                        drop(guard);
                        return Some(self.record_runtime_error(
                            "scatter index lane must be an integer".to_string(),
                            span,
                        ));
                    };
                    if *idx < 0 || (*idx as usize) >= slen {
                        drop(guard);
                        return Some(self.record_runtime_error(
                            "scatter: index out of bounds".to_string(),
                            span,
                        ));
                    }
                    guard[start + *idx as usize] = lane.clone();
                }
                Some(Value::Unit)
            }
            _ => None,
        }
    }
}
