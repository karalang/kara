//! Iterator-method dispatch — the bodies of the `iter`/`into_iter`/
//! `next`/`map`/`filter`/`enumerate`/`take`/`skip`/`take_while`/
//! `skip_while`/`flat_map`/`step_by`/`cycle`/`inspect`/`scan`/
//! `chunk_by`/`peekable`/`peek`/`chain`/`zip`/`count`/`collect`/
//! `fold`/`any`/`all` arms lifted out of `eval_method_call`.
//!
//! Returns `Some(Value)` if `method` matches an iterator-adapter name
//! and the dispatch completes; returns `None` otherwise so the parent
//! dispatcher can fall through to the next category.

use crate::ast::*;
use crate::token::Span;

use super::value::{EnumData, IteratorSource, IteratorStep, Value};

impl<'a> super::Interpreter<'a> {
    /// Evaluate an iterator adaptor/terminal CLOSURE argument with
    /// live-capture semantics (B-2026-07-14-20): a bare closure literal
    /// wraps ALL its free captures in `Value::SharedCell` aliases, so the
    /// predicate/mapper reads the LIVE outer binding on every invocation —
    /// matching codegen's fused-adaptor inlining (which reads the live
    /// variable each iteration) and design.md § Closures Rule 2 (read →
    /// capture by reference). Without this the interpreter's construction-
    /// time snapshot diverged from codegen whenever the loop body mutated
    /// a captured variable (`let mut lim = 5; for x in
    /// xs.iter().filter(|v| v < lim) { lim = lim - 1; … }`). Engages only
    /// for a closure LITERAL argument — any other expression evaluates
    /// normally (a stored closure value keeps its creation-time
    /// semantics), and explicit `own`/`ref`/`mut ref` prefixes keep their
    /// pinned modes (the flag only widens the BARE-closure wrap set).
    fn eval_iter_closure_arg(&mut self, expr: &Expr) -> Value {
        if !matches!(expr.kind, ExprKind::Closure { .. }) {
            return self.eval_expr_inner(expr);
        }
        let saved = self.wrap_all_closure_captures;
        self.wrap_all_closure_captures = true;
        let v = self.eval_expr_inner(expr);
        self.wrap_all_closure_captures = saved;
        v
    }

    /// Expand one inner iterable value (an element of a `flatten()` receiver)
    /// into its elements in order. Handles the collection/iterator shapes the
    /// typechecker admits as flattenable: an owned `Vec`/array (`Value::Array`),
    /// a `Slice` view, and a nested `Value::Iterator` (drained). Any other value
    /// is a runtime type error (the typechecker rejects non-iterable elements,
    /// so this is a defensive backstop).
    fn flatten_one_inner(&mut self, v: Value) -> Result<Vec<Value>, String> {
        match v {
            Value::Array(rc) => Ok(rc.read().unwrap().clone()),
            Value::Slice {
                storage,
                start,
                len,
                ..
            } => {
                let g = storage.read().unwrap();
                Ok(g[start..start + len].to_vec())
            }
            Value::Iterator { .. } => {
                let mut inner = v;
                let mut items = Vec::new();
                while let Some(x) = self.iterator_step(&mut inner) {
                    items.push(x);
                }
                Ok(items)
            }
            other => Err(format!(
                "Iterator.flatten() expects iterable elements; got {}",
                other
            )),
        }
    }

    pub(super) fn try_eval_iterator_method(
        &mut self,
        method: &str,
        object: &Expr,
        obj: &Value,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        // `iter()`/`into_iter()` build an iterator from a COLLECTION receiver
        // by borrowing it — never deep-clone the collection (this guard sits on
        // the map-heavy dispatch hot path; B-2026-06-07-4). An Iterator
        // receiver passes through.
        if matches!(method, "iter" | "into_iter") {
            // Snapshot the source elements eagerly into a Value::Iterator.
            // Map yields (k, v) tuples; SortedSet flattens to ascending
            // order; Set/Array yield elements in storage order. The
            // tree-walk interpreter is type-erased so iter() and
            // into_iter() are identical at this layer — the design.md
            // borrow-vs-consume distinction is a typechecker concern.
            //
            // Iterator receivers (e.g. the redundant `(0..10).iter()`
            // call shape now that Range evaluates to `Value::Iterator`)
            // pass through unchanged — calling iter() on an iterator
            // returns the iterator itself.
            if matches!(obj, Value::Iterator { .. }) {
                return Some(obj.clone());
            }
            let items = match obj {
                Value::Array(rc) => rc.read().unwrap().clone(),
                Value::Slice {
                    storage,
                    start,
                    len,
                    ..
                } => storage.read().unwrap()[*start..*start + *len].to_vec(),
                Value::Set(s) => s.clone(),
                Value::SortedSet(s) => s.keys().map(|k| k.0.clone()).collect(),
                Value::Map(m) => m
                    .iter()
                    .map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()]))
                    .collect(),
                Value::SortedMap(m) => m
                    .iter()
                    .map(|(k, v)| Value::Tuple(vec![k.0.clone(), v.clone()]))
                    .collect(),
                _ => unreachable!(
                    "{}() receiver at {}:{} was Value::{}; \
                     either an interpreter codepath produced the wrong receiver variant \
                     or the typechecker accepted .{}() on a non-iterable type",
                    method,
                    span.line,
                    span.column,
                    obj.variant_name(),
                    method
                ),
            };
            return Some(Value::Iterator {
                source: IteratorSource::Eager { items, cursor: 0 },
                steps: Vec::new(),
            });
        }

        // Every remaining adapter/terminal arm operates on an Iterator
        // receiver and consumes it. A non-Iterator receiver (Map/Vec/Set/…) is
        // not handled by this category — return None WITHOUT cloning, so a
        // large collection is never deep-cloned merely to be rejected here.
        // Each arm below already re-checks `matches!(obj, Value::Iterator)`;
        // those guards are now always true (kept verbatim to minimise churn).
        // The owned clone is of the confirmed (small) Iterator, taken once.
        if !matches!(obj, Value::Iterator { .. }) {
            return None;
        }
        let obj = obj.clone();

        match method {
            "next" => {
                // `Iterator.next()` — pull the next item via `iterator_step`,
                // applying any adaptor closures registered in `steps`. When
                // the receiver is a binding, write the advanced state back
                // so subsequent calls see it. The `matches!` guard borrows
                // `obj` so the fall-through path (defensive — typechecker
                // should reject non-Iterator receivers) can keep using it.
                if matches!(obj, Value::Iterator { .. }) {
                    let mut iter_val = obj;
                    let yielded = self.iterator_step(&mut iter_val);
                    let result = match yielded {
                        Some(val) => Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "Some".to_string(),
                            data: EnumData::Tuple(vec![val]),
                        },
                        None => Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "None".to_string(),
                            data: EnumData::Unit,
                        },
                    };
                    self.write_back_receiver(object, iter_val);
                    return Some(result);
                }
            }
            "map" | "filter" | "filter_map" => {
                // Lazy adaptors — append a `MapStep(closure)` /
                // `FilterStep(closure)` / `FilterMapStep(closure)` to the
                // iterator's adaptor chain.
                // The closure is evaluated to a Value::Function once at
                // construction; per-element invocation happens at next()
                // time via `iterator_step`. Per design.md § Iterator
                // Adaptors, transformations are lazy — only terminal ops
                // drive iteration.
                if matches!(obj, Value::Iterator { .. }) {
                    let Some(arg) = args.first() else {
                        return Some(self.record_runtime_error(
                            format!("Iterator.{}() requires a closure argument", method),
                            span,
                        ));
                    };
                    let closure = self.eval_iter_closure_arg(&arg.value);
                    if !matches!(closure, Value::Function { .. }) {
                        return Some(self.record_runtime_error(
                            format!("Iterator.{}() expects a closure; got {}", method, closure),
                            span,
                        ));
                    }
                    let Value::Iterator { source, mut steps } = obj else {
                        unreachable!()
                    };
                    steps.push(match method {
                        "map" => IteratorStep::Map(closure),
                        "filter" => IteratorStep::Filter(closure),
                        "filter_map" => IteratorStep::FilterMap(closure),
                        _ => unreachable!(),
                    });
                    return Some(Value::Iterator { source, steps });
                }
            }
            "enumerate" => {
                // Lazy positional adaptor — append `Enumerate(0)` to the
                // chain. iterator_step wraps each yielded item into
                // `(idx, item)` and bumps the counter.
                if matches!(obj, Value::Iterator { .. }) {
                    let Value::Iterator { source, mut steps } = obj else {
                        unreachable!()
                    };
                    steps.push(IteratorStep::Enumerate(0));
                    return Some(Value::Iterator { source, steps });
                }
            }
            "rev" => {
                // Reversal is NOT a lazy per-element step — it needs the whole
                // sequence. Drain the upstream (firing every existing adaptor
                // closure in forward order), reverse the collected items, and
                // return a fresh EAGER iterator over them, so any downstream
                // adaptor/terminal (map/filter/fold/collect/for) then runs over
                // the reversed order. Correct for any upstream chain and element
                // type. (Codegen defers `rev` — B-2026-07-18-41.)
                if matches!(obj, Value::Iterator { .. }) {
                    let mut iter_val = obj;
                    let mut out = Vec::new();
                    while let Some(v) = self.iterator_step(&mut iter_val) {
                        out.push(v);
                    }
                    out.reverse();
                    return Some(Value::Iterator {
                        source: IteratorSource::Eager {
                            items: out,
                            cursor: 0,
                        },
                        steps: Vec::new(),
                    });
                }
            }
            "flatten" => {
                // Eager flatten (mirrors `rev`): the receiver is an iterator of
                // iterables. Drain the outer (firing every upstream adaptor
                // closure), expand each inner iterable into its elements in
                // order, and return a fresh EAGER iterator so any downstream
                // adaptor/terminal/for-loop runs over the flattened sequence.
                // Equivalent to `flat_map(|x| x)`. (Codegen handles the common
                // for-loop / collect shapes; other shapes defer to `--interp`.)
                if matches!(obj, Value::Iterator { .. }) {
                    let mut iter_val = obj;
                    let mut out = Vec::new();
                    while let Some(inner) = self.iterator_step(&mut iter_val) {
                        match self.flatten_one_inner(inner) {
                            Ok(mut items) => out.append(&mut items),
                            Err(msg) => return Some(self.record_runtime_error(msg, span)),
                        }
                    }
                    return Some(Value::Iterator {
                        source: IteratorSource::Eager {
                            items: out,
                            cursor: 0,
                        },
                        steps: Vec::new(),
                    });
                }
            }
            "take" | "skip" => {
                // Lazy count-bounded adaptors. Negative `n` clamps to
                // zero — `take(-1)` yields nothing; `skip(-1)` skips
                // nothing. The typechecker accepts any i64 so this
                // matters at runtime.
                if matches!(obj, Value::Iterator { .. }) {
                    let Some(arg) = args.first() else {
                        return Some(self.record_runtime_error(
                            format!("Iterator.{}() requires an integer argument", method),
                            span,
                        ));
                    };
                    let n = match self.eval_expr_inner(&arg.value) {
                        Value::Int(n) => n.max(0) as usize,
                        v => {
                            return Some(self.record_runtime_error(
                                format!("Iterator.{}() expects an integer; got {}", method, v),
                                span,
                            ));
                        }
                    };
                    let Value::Iterator { source, mut steps } = obj else {
                        unreachable!()
                    };
                    steps.push(match method {
                        "take" => IteratorStep::Take(n),
                        "skip" => IteratorStep::Skip(n),
                        _ => unreachable!(),
                    });
                    return Some(Value::Iterator { source, steps });
                }
            }
            "take_while" | "skip_while" => {
                // Lazy predicate-bounded adaptors. `take_while` stops
                // on the first false; `skip_while` drops items while
                // pred holds, then yields the rest unconditionally.
                // Both share the closure-validation path of map/filter.
                if matches!(obj, Value::Iterator { .. }) {
                    let Some(arg) = args.first() else {
                        return Some(self.record_runtime_error(
                            format!("Iterator.{}() requires a closure argument", method),
                            span,
                        ));
                    };
                    let closure = self.eval_iter_closure_arg(&arg.value);
                    if !matches!(closure, Value::Function { .. }) {
                        return Some(self.record_runtime_error(
                            format!("Iterator.{}() expects a closure; got {}", method, closure),
                            span,
                        ));
                    }
                    let Value::Iterator { source, mut steps } = obj else {
                        unreachable!()
                    };
                    steps.push(match method {
                        "take_while" => IteratorStep::TakeWhile {
                            pred: closure,
                            done: false,
                        },
                        "skip_while" => IteratorStep::SkipWhile {
                            pred: closure,
                            done: false,
                        },
                        _ => unreachable!(),
                    });
                    return Some(Value::Iterator { source, steps });
                }
            }
            "flat_map" => {
                // Lazy flatten-after-map combinator. Wraps `self` (the
                // outer) plus the closure into a fresh
                // `IteratorSource::FlatMap`. Each pull from the
                // resulting iterator drains the in-flight inner
                // iterator (filling it from `f(outer_item)` when
                // exhausted) and yields one item per pull.
                if matches!(obj, Value::Iterator { .. }) {
                    let Some(arg) = args.first() else {
                        return Some(self.record_runtime_error(
                            "Iterator.flat_map() requires a closure argument".to_string(),
                            span,
                        ));
                    };
                    let closure = self.eval_iter_closure_arg(&arg.value);
                    if !matches!(closure, Value::Function { .. }) {
                        return Some(self.record_runtime_error(
                            format!("Iterator.flat_map() expects a closure; got {}", closure),
                            span,
                        ));
                    }
                    return Some(Value::Iterator {
                        source: IteratorSource::FlatMap {
                            outer: Box::new(obj),
                            f: Box::new(closure),
                            current_inner: None,
                        },
                        steps: Vec::new(),
                    });
                }
            }
            "step_by" => {
                // Lazy stride adaptor — yields every n-th item. Negative
                // or zero `n` clamps to 1 at the runtime layer (the
                // typechecker accepts any i64). n=1 makes step_by an
                // observable no-op; n>len yields just the first item.
                if matches!(obj, Value::Iterator { .. }) {
                    let Some(arg) = args.first() else {
                        return Some(self.record_runtime_error(
                            "Iterator.step_by() requires an integer argument".to_string(),
                            span,
                        ));
                    };
                    let n = match self.eval_expr_inner(&arg.value) {
                        Value::Int(n) => n.max(1) as usize,
                        v => {
                            return Some(self.record_runtime_error(
                                format!("Iterator.step_by() expects an integer; got {}", v),
                                span,
                            ));
                        }
                    };
                    let Value::Iterator { source, mut steps } = obj else {
                        unreachable!()
                    };
                    steps.push(IteratorStep::StepBy {
                        n,
                        remaining_skip: 0,
                    });
                    return Some(Value::Iterator { source, steps });
                }
            }
            "cycle" => {
                // Restart-on-exhaust combinator. Snapshots `self`
                // (deep-clone via Value's derived Clone) into a
                // `template`; each restart re-clones the template
                // into `current`, which resets adaptor counters
                // (Enumerate / Take / Skip / TakeWhile / SkipWhile /
                // StepBy) for that cycle. Downstream adaptors append
                // to the wrapping iterator's empty steps and apply
                // uniformly across cycles.
                if matches!(obj, Value::Iterator { .. }) {
                    if !args.is_empty() {
                        return Some(self.record_runtime_error(
                            format!("Iterator.cycle() takes no arguments, got {}", args.len()),
                            span,
                        ));
                    }
                    let template = obj.clone();
                    return Some(Value::Iterator {
                        source: IteratorSource::Cycle {
                            template: Box::new(template.clone()),
                            current: Box::new(template),
                            exhausted: false,
                        },
                        steps: Vec::new(),
                    });
                }
            }
            "inspect" => {
                // Lazy side-effect adaptor — appends an
                // `IteratorStep::Inspect(closure)` that fires `f` on
                // each yielded item and passes the item through
                // unchanged.
                if matches!(obj, Value::Iterator { .. }) {
                    let Some(arg) = args.first() else {
                        return Some(self.record_runtime_error(
                            "Iterator.inspect() requires a closure argument".to_string(),
                            span,
                        ));
                    };
                    let closure = self.eval_iter_closure_arg(&arg.value);
                    if !matches!(closure, Value::Function { .. }) {
                        return Some(self.record_runtime_error(
                            format!("Iterator.inspect() expects a closure; got {}", closure),
                            span,
                        ));
                    }
                    let Value::Iterator { source, mut steps } = obj else {
                        unreachable!()
                    };
                    steps.push(IteratorStep::Inspect(closure));
                    return Some(Value::Iterator { source, steps });
                }
            }
            "scan" => {
                // Lazy stateful adaptor — appends an
                // `IteratorStep::Scan { f, state, done }`. Closure
                // signature is `Fn(A, T) -> Option<(A, U)>`; the
                // first arg is the initial state, the second is the
                // closure.
                if matches!(obj, Value::Iterator { .. }) {
                    if args.len() != 2 {
                        return Some(self.record_runtime_error(
                            format!("Iterator.scan() requires 2 arguments, got {}", args.len()),
                            span,
                        ));
                    }
                    let init = self.eval_expr_inner(&args[0].value);
                    let closure = self.eval_iter_closure_arg(&args[1].value);
                    if !matches!(closure, Value::Function { .. }) {
                        return Some(self.record_runtime_error(
                            format!("Iterator.scan() expects a closure; got {}", closure),
                            span,
                        ));
                    }
                    let Value::Iterator { source, mut steps } = obj else {
                        unreachable!()
                    };
                    steps.push(IteratorStep::Scan {
                        f: closure,
                        state: init,
                        done: false,
                    });
                    return Some(Value::Iterator { source, steps });
                }
            }
            "chunk_by" => {
                // Lazy buffering adaptor — wraps the receiver into a
                // ChunkBy source. Each pull yields a freshly allocated
                // `Vec[T]` containing the next run of consecutive
                // items whose `key_fn(item)` produces equal keys.
                if matches!(obj, Value::Iterator { .. }) {
                    let Some(arg) = args.first() else {
                        return Some(self.record_runtime_error(
                            "Iterator.chunk_by() requires a closure argument".to_string(),
                            span,
                        ));
                    };
                    let closure = self.eval_iter_closure_arg(&arg.value);
                    if !matches!(closure, Value::Function { .. }) {
                        return Some(self.record_runtime_error(
                            format!("Iterator.chunk_by() expects a closure; got {}", closure),
                            span,
                        ));
                    }
                    return Some(Value::Iterator {
                        source: IteratorSource::ChunkBy {
                            inner: Box::new(obj),
                            key_fn: Box::new(closure),
                            pending_item: None,
                            pending_key: None,
                            exhausted: false,
                        },
                        steps: Vec::new(),
                    });
                }
            }
            "peekable" => {
                // Wraps the receiver into a Peekable source with an
                // empty buffer. Adaptor calls after this return
                // Iterator[U] at the type layer (peekable-ness lost),
                // so the wrapping iterator's `steps` stays empty in
                // well-typed programs and pull_source can route
                // straight to the inner iterator without re-running
                // outer steps.
                if matches!(obj, Value::Iterator { .. }) {
                    if !args.is_empty() {
                        return Some(self.record_runtime_error(
                            format!("Iterator.peekable() takes no arguments, got {}", args.len()),
                            span,
                        ));
                    }
                    return Some(Value::Iterator {
                        source: IteratorSource::Peekable {
                            inner: Box::new(obj),
                            buffered: None,
                        },
                        steps: Vec::new(),
                    });
                }
            }
            "peek" => {
                // Look one element ahead without consuming. Pull from
                // the buffer if present; otherwise pull one item from
                // the inner iterator, store it in the buffer, and
                // return a clone wrapped in `Some`. Sticky-empty
                // (returns None forever once the inner is exhausted
                // and the buffer is empty). Writeback to the binding
                // mirrors `next()` so subsequent calls observe the
                // populated buffer.
                if let Value::Iterator {
                    source: IteratorSource::Peekable { .. },
                    ..
                } = &obj
                {
                    if !args.is_empty() {
                        return Some(self.record_runtime_error(
                            format!("Peekable.peek() takes no arguments, got {}", args.len()),
                            span,
                        ));
                    }
                    let mut iter_val = obj;
                    let result = self.peek_value(&mut iter_val);
                    self.write_back_receiver(object, iter_val);
                    return Some(result);
                }
            }
            "chain" => {
                // Lazy two-source combinator. Wraps `self` and `other`
                // into an `IteratorSource::Chain` so each side keeps
                // its own (already-applied) step chain. Downstream
                // adaptors append to the new wrapper's empty steps.
                if matches!(obj, Value::Iterator { .. }) {
                    let Some(arg) = args.first() else {
                        return Some(self.record_runtime_error(
                            "Iterator.chain() requires an iterator argument".to_string(),
                            span,
                        ));
                    };
                    let other = self.eval_expr_inner(&arg.value);
                    if !matches!(other, Value::Iterator { .. }) {
                        return Some(self.record_runtime_error(
                            format!("Iterator.chain() expects an iterator; got {}", other),
                            span,
                        ));
                    }
                    return Some(Value::Iterator {
                        source: IteratorSource::Chain {
                            parts: vec![obj, other],
                            current: 0,
                        },
                        steps: Vec::new(),
                    });
                }
            }
            "zip" => {
                // Lazy synchronous-pair combinator. Each pull from the
                // resulting iterator pulls one item from each side and
                // yields a `(a, b)` tuple; either side ending stops the
                // zip. Each side retains its own step chain.
                if matches!(obj, Value::Iterator { .. }) {
                    let Some(arg) = args.first() else {
                        return Some(self.record_runtime_error(
                            "Iterator.zip() requires an iterator argument".to_string(),
                            span,
                        ));
                    };
                    let other = self.eval_expr_inner(&arg.value);
                    if !matches!(other, Value::Iterator { .. }) {
                        return Some(self.record_runtime_error(
                            format!("Iterator.zip() expects an iterator; got {}", other),
                            span,
                        ));
                    }
                    return Some(Value::Iterator {
                        source: IteratorSource::Zip {
                            left: Box::new(obj),
                            right: Box::new(other),
                        },
                        steps: Vec::new(),
                    });
                }
            }
            "count" | "len" => {
                // Terminal — drain the iterator (firing all adaptor
                // closures) and count yielded elements. `len` is an alias for
                // `count` (matches the typechecker + codegen), so
                // `s.chars().len()` works under the interpreter too
                // (B-2026-07-11-9 gap 1).
                if matches!(obj, Value::Iterator { .. }) {
                    let mut iter_val = obj;
                    let mut n: i64 = 0;
                    while self.iterator_step(&mut iter_val).is_some() {
                        n += 1;
                    }
                    return Some(Value::Int(n));
                }
            }
            "collect" => {
                // Terminal v1 — drain the iterator into a Vec[T]
                // (Value::Array). FromIterator-driven dispatch into other
                // collections is a follow-up CR.
                if matches!(obj, Value::Iterator { .. }) {
                    let mut iter_val = obj;
                    let mut out = Vec::new();
                    while let Some(v) = self.iterator_step(&mut iter_val) {
                        out.push(v);
                    }
                    return Some(Value::array_of(out));
                }
            }
            "fold" => {
                // Terminal — `fold(init, f)`. Walk via repeated
                // iterator_step pulls, threading the accumulator through
                // the closure on each step.
                if matches!(obj, Value::Iterator { .. }) {
                    if args.len() != 2 {
                        return Some(self.record_runtime_error(
                            format!("Iterator.fold() expects 2 arguments, got {}", args.len()),
                            span,
                        ));
                    }
                    let mut acc = self.eval_expr_inner(&args[0].value);
                    let f = self.eval_iter_closure_arg(&args[1].value);
                    if !matches!(f, Value::Function { .. }) {
                        return Some(self.record_runtime_error(
                            format!("Iterator.fold() expects a closure; got {}", f),
                            span,
                        ));
                    }
                    let mut iter_val = obj;
                    while let Some(item) = self.iterator_step(&mut iter_val) {
                        acc = self.invoke_function_value(f.clone(), vec![acc, item]);
                    }
                    return Some(acc);
                }
            }
            "sum" | "product" => {
                // Numeric terminals — drain the iterator, combining each yielded
                // element into a running accumulator seeded from the first (so
                // the result carries the element's numeric type). `sum` combines
                // with `+` (empty → 0), `product` with `*` (empty → 1); codegen
                // seeds from a type-recorded zero/one. B-2026-07-11-19 (sum),
                // product is the multiplicative sibling.
                if matches!(obj, Value::Iterator { .. }) {
                    if !args.is_empty() {
                        return Some(self.record_runtime_error(
                            format!(
                                "Iterator.{}() takes no arguments, got {}",
                                method,
                                args.len()
                            ),
                            span,
                        ));
                    }
                    let op = if method == "product" {
                        BinOp::Mul
                    } else {
                        BinOp::Add
                    };
                    let mut iter_val = obj;
                    let mut acc: Option<Value> = None;
                    while let Some(item) = self.iterator_step(&mut iter_val) {
                        acc = Some(match acc {
                            None => item,
                            Some(a) => self.eval_binary(&op, a, item, span, false),
                        });
                    }
                    let empty_default = if method == "product" { 1 } else { 0 };
                    return Some(acc.unwrap_or(Value::Int(empty_default)));
                }
            }
            "max" | "min" => {
                // Comparison terminals (B-2026-07-16-14) — drain the iterator
                // keeping the extreme element; `Some(best)`, or `None` for an
                // empty source (Rust semantics — no sentinel seeding). Ordering
                // via the same `eval_binary` Gt/Lt the language's comparison
                // operators use (ints, floats, Strings). Ties keep the FIRST
                // seen (strict compare), matching Rust's `max_by`/`min_by`
                // first-wins-on-equal behavior for max? Rust `Iterator::max`
                // returns the LAST maximum; kept strict-first here and pinned
                // by tests — the difference is observable only for duplicate
                // extremes of non-scalar elements.
                if matches!(obj, Value::Iterator { .. }) {
                    if !args.is_empty() {
                        return Some(self.record_runtime_error(
                            format!(
                                "Iterator.{}() takes no arguments, got {}",
                                method,
                                args.len()
                            ),
                            span,
                        ));
                    }
                    let op = if method == "max" {
                        BinOp::Gt
                    } else {
                        BinOp::Lt
                    };
                    let mut iter_val = obj;
                    let mut best: Option<Value> = None;
                    while let Some(item) = self.iterator_step(&mut iter_val) {
                        best = Some(match best {
                            None => item,
                            Some(b) => {
                                let wins =
                                    self.eval_binary(&op, item.clone(), b.clone(), span, false);
                                if matches!(wins, Value::Bool(true)) {
                                    item
                                } else {
                                    b
                                }
                            }
                        });
                    }
                    return Some(match best {
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
            }
            "reduce" => {
                // Terminal — `reduce(f)`. Folds elements with the first as the
                // seed; returns `Some(acc)`, or `None` for an empty source.
                // B-2026-07-11-19.
                if matches!(obj, Value::Iterator { .. }) {
                    if args.len() != 1 {
                        return Some(self.record_runtime_error(
                            format!("Iterator.reduce() expects 1 argument, got {}", args.len()),
                            span,
                        ));
                    }
                    let f = self.eval_iter_closure_arg(&args[0].value);
                    if !matches!(f, Value::Function { .. }) {
                        return Some(self.record_runtime_error(
                            format!("Iterator.reduce() expects a closure; got {}", f),
                            span,
                        ));
                    }
                    let mut iter_val = obj;
                    let mut acc: Option<Value> = None;
                    while let Some(item) = self.iterator_step(&mut iter_val) {
                        acc = Some(match acc {
                            None => item,
                            Some(a) => self.invoke_function_value(f.clone(), vec![a, item]),
                        });
                        if self.pending_cf.is_some() {
                            return Some(Value::Unit);
                        }
                    }
                    return Some(match acc {
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
            }
            "for_each" => {
                // Terminal — `for_each(f)`. Runs `f` for its side effects on each
                // yielded element and returns unit. A capture-mutating body
                // (`for_each(|x| total = total + x)`) propagates now that bare
                // mut-ref closure capture is inferred (B-2026-07-11-23).
                if matches!(obj, Value::Iterator { .. }) {
                    if args.len() != 1 {
                        return Some(self.record_runtime_error(
                            format!("Iterator.for_each() expects 1 argument, got {}", args.len()),
                            span,
                        ));
                    }
                    let f = self.eval_iter_closure_arg(&args[0].value);
                    if !matches!(f, Value::Function { .. }) {
                        return Some(self.record_runtime_error(
                            format!("Iterator.for_each() expects a closure; got {}", f),
                            span,
                        ));
                    }
                    let mut iter_val = obj;
                    while let Some(item) = self.iterator_step(&mut iter_val) {
                        self.invoke_function_value(f.clone(), vec![item]);
                        if self.pending_cf.is_some() {
                            return Some(Value::Unit);
                        }
                    }
                    return Some(Value::Unit);
                }
            }
            "any" | "all" => {
                // Short-circuit terminals. `any(pred)` returns true the
                // first time `pred` returns true; `all(pred)` returns
                // false the first time `pred` returns false. Both walk
                // the iterator via iterator_step — the loop bails the
                // moment the answer is decided, so upstream adaptor
                // closures only fire for as many elements as it takes.
                if matches!(obj, Value::Iterator { .. }) {
                    let Some(arg) = args.first() else {
                        return Some(self.record_runtime_error(
                            format!("Iterator.{}() requires a closure argument", method),
                            span,
                        ));
                    };
                    let pred = self.eval_iter_closure_arg(&arg.value);
                    if !matches!(pred, Value::Function { .. }) {
                        return Some(self.record_runtime_error(
                            format!("Iterator.{}() expects a closure; got {}", method, pred),
                            span,
                        ));
                    }
                    let want_any = method == "any";
                    let mut iter_val = obj;
                    while let Some(item) = self.iterator_step(&mut iter_val) {
                        let result = self.invoke_function_value(pred.clone(), vec![item]);
                        let truthy = matches!(result, Value::Bool(true));
                        if want_any && truthy {
                            return Some(Value::Bool(true));
                        }
                        if !want_any && !truthy {
                            return Some(Value::Bool(false));
                        }
                    }
                    // Source exhausted with no decisive answer — any
                    // returns false (no element matched), all returns
                    // true (every element matched / source was empty).
                    return Some(Value::Bool(!want_any));
                }
            }
            "position" => {
                // `position(pred) -> Option[i64]` — the 0-based index of the
                // first YIELDED element the predicate holds for, or `None`.
                // Short-circuit like `any`; the index counts post-adaptor
                // elements (each `iterator_step` yield).
                if matches!(obj, Value::Iterator { .. }) {
                    let Some(arg) = args.first() else {
                        return Some(self.record_runtime_error(
                            "Iterator.position() requires a closure argument".to_string(),
                            span,
                        ));
                    };
                    let pred = self.eval_iter_closure_arg(&arg.value);
                    if !matches!(pred, Value::Function { .. }) {
                        return Some(self.record_runtime_error(
                            format!("Iterator.position() expects a closure; got {}", pred),
                            span,
                        ));
                    }
                    let mut iter_val = obj;
                    let mut idx: i64 = 0;
                    while let Some(item) = self.iterator_step(&mut iter_val) {
                        let result = self.invoke_function_value(pred.clone(), vec![item]);
                        if matches!(result, Value::Bool(true)) {
                            return Some(Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "Some".to_string(),
                                data: EnumData::Tuple(vec![Value::Int(idx)]),
                            });
                        }
                        idx += 1;
                    }
                    return Some(Value::EnumVariant {
                        enum_name: "Option".to_string(),
                        variant: "None".to_string(),
                        data: EnumData::Unit,
                    });
                }
            }
            "find" => {
                // `find(pred) -> Option[T]` — the first YIELDED element the
                // predicate holds for, or `None`. Short-circuit like `position`;
                // the matched element is returned by value in the `Some` payload.
                if matches!(obj, Value::Iterator { .. }) {
                    let Some(arg) = args.first() else {
                        return Some(self.record_runtime_error(
                            "Iterator.find() requires a closure argument".to_string(),
                            span,
                        ));
                    };
                    let pred = self.eval_iter_closure_arg(&arg.value);
                    if !matches!(pred, Value::Function { .. }) {
                        return Some(self.record_runtime_error(
                            format!("Iterator.find() expects a closure; got {}", pred),
                            span,
                        ));
                    }
                    let mut iter_val = obj;
                    while let Some(item) = self.iterator_step(&mut iter_val) {
                        let keep = self.invoke_function_value(pred.clone(), vec![item.clone()]);
                        if matches!(keep, Value::Bool(true)) {
                            return Some(Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "Some".to_string(),
                                data: EnumData::Tuple(vec![item]),
                            });
                        }
                    }
                    return Some(Value::EnumVariant {
                        enum_name: "Option".to_string(),
                        variant: "None".to_string(),
                        data: EnumData::Unit,
                    });
                }
            }
            "find_map" => {
                // `find_map(f: Fn(T) -> Option[U]) -> Option[U]` — the first
                // `Some(u)` the closure produces (map + find fusion), or `None`.
                // Short-circuit: apply `f` to each yielded element and return its
                // `Some` payload the moment one appears (correct for scalar AND
                // heap `U`, since the payload is the closure's own produced value).
                if matches!(obj, Value::Iterator { .. }) {
                    let Some(arg) = args.first() else {
                        return Some(self.record_runtime_error(
                            "Iterator.find_map() requires a closure argument".to_string(),
                            span,
                        ));
                    };
                    let f = self.eval_iter_closure_arg(&arg.value);
                    if !matches!(f, Value::Function { .. }) {
                        return Some(self.record_runtime_error(
                            format!("Iterator.find_map() expects a closure; got {}", f),
                            span,
                        ));
                    }
                    let mut iter_val = obj;
                    while let Some(item) = self.iterator_step(&mut iter_val) {
                        let mapped = self.invoke_function_value(f.clone(), vec![item]);
                        if let Value::EnumVariant { variant, data, .. } = &mapped {
                            if variant == "Some" {
                                if let EnumData::Tuple(payload) = data {
                                    if let Some(v) = payload.first() {
                                        return Some(Value::EnumVariant {
                                            enum_name: "Option".to_string(),
                                            variant: "Some".to_string(),
                                            data: EnumData::Tuple(vec![v.clone()]),
                                        });
                                    }
                                }
                            }
                        }
                    }
                    return Some(Value::EnumVariant {
                        enum_name: "Option".to_string(),
                        variant: "None".to_string(),
                        data: EnumData::Unit,
                    });
                }
            }
            "last" => {
                // `last() -> Option[T]` — drain the iterator, return the LAST
                // yielded element (or `None` for an empty source).
                if matches!(obj, Value::Iterator { .. }) {
                    let mut iter_val = obj;
                    let mut last: Option<Value> = None;
                    while let Some(item) = self.iterator_step(&mut iter_val) {
                        last = Some(item);
                    }
                    return Some(match last {
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
            }
            "nth" => {
                // `nth(n) -> Option[T]` — the n-th (0-based) yielded element, or
                // `None` if the source has fewer than n+1 elements. A negative n
                // never matches, so it yields `None`.
                if matches!(obj, Value::Iterator { .. }) {
                    let n = match args.first().map(|a| self.eval_expr_inner(&a.value)) {
                        Some(Value::Int(n)) => n,
                        _ => {
                            return Some(self.record_runtime_error(
                                "Iterator.nth() requires an integer argument".to_string(),
                                span,
                            ));
                        }
                    };
                    let none = Value::EnumVariant {
                        enum_name: "Option".to_string(),
                        variant: "None".to_string(),
                        data: EnumData::Unit,
                    };
                    if n < 0 {
                        return Some(none);
                    }
                    let mut iter_val = obj;
                    let mut idx: i64 = 0;
                    while let Some(item) = self.iterator_step(&mut iter_val) {
                        if idx == n {
                            return Some(Value::EnumVariant {
                                enum_name: "Option".to_string(),
                                variant: "Some".to_string(),
                                data: EnumData::Tuple(vec![item]),
                            });
                        }
                        idx += 1;
                    }
                    return Some(none);
                }
            }
            _ => return None,
        }
        None
    }
}
