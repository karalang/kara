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

use super::helpers::{eval_http_get, value_compare};
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
    ) -> Option<Value> {
        match method {
            "len" => {
                return Some(match &obj {
                    Value::Array(rc) => Value::Int(rc.read().unwrap().len() as i64),
                    Value::Slice { len, .. } => Value::Int(*len as i64),
                    Value::String(s) => Value::Int(s.len() as i64),
                    Value::Map(m) => Value::Int(m.len() as i64),
                    Value::SortedSet(s) => Value::Int(s.len() as i64),
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
            "starts_with" => {
                // `String.starts_with(prefix: String) -> bool`. The
                // typechecker arm in `infer_str_method` enforces the
                // String-arg shape; this dispatch trusts the receiver
                // is a Value::String and the single arg evaluates to
                // one too.
                if let (Value::String(s), [arg]) = (&obj, args) {
                    let prefix_val = self.eval_expr_inner(&arg.value);
                    if let Value::String(prefix) = prefix_val {
                        return Some(Value::Bool(s.starts_with(prefix.as_str())));
                    }
                }
                return None;
            }
            "substring" => {
                // `String.substring(start: i64) -> String`. Returns a
                // fresh owned String of the receiver's bytes from byte
                // offset `start` to the end. Out-of-range / negative
                // starts saturate to an empty String.
                if let (Value::String(s), [arg]) = (&obj, args) {
                    let start_val = self.eval_expr_inner(&arg.value);
                    if let Value::Int(start) = start_val {
                        let len = s.len() as i64;
                        if start < 0 || start >= len {
                            return Some(Value::String(String::new()));
                        }
                        return Some(Value::String(s[start as usize..].to_string()));
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
                    if let ExprKind::Identifier(name) = &object.kind {
                        self.env.set(name, Value::String(next));
                    }
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
                    if let ExprKind::Identifier(name) = &object.kind {
                        self.env.set(name, Value::String(next));
                    }
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
                    _ => return Some(Value::Unit),
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
                    _ => return Some(Value::Unit),
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
                return Some(Value::Unit);
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
                    try_write_or_panic(rc, &label).sort_by(value_compare);
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
                    v.sort_by(value_compare);
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
        obj: Value,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        let Value::Vector(lanes) = obj else {
            return None;
        };
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
                    acc = self.eval_binary(&fold_op, acc, lane, span);
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
                            self.eval_binary(&cmp_op, acc.clone(), lane.clone(), span),
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
                    let prod = self.eval_binary(&BinOp::Mul, x, y, span);
                    acc = Some(match acc {
                        None => prod,
                        Some(a) => self.eval_binary(&BinOp::Add, a, prod, span),
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
                    let pq = me.eval_binary(&BinOp::Mul, p, q, span);
                    let rs = me.eval_binary(&BinOp::Mul, r, s, span);
                    me.eval_binary(&BinOp::Sub, pq, rs, span)
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
            _ => None,
        }
    }
}
