//! Control-flow signals, scope/cleanup helpers, and the Env scope chain.
//!
//! Houses the non-local control-flow enum (`ControlFlow`), the
//! cleanup-action types (`CleanupAction`, `ErrDeferEntry`), the
//! block-exit classifier (`ExitPath`), value-deep-clone
//! (`deep_clone_value`), slice-pattern view (`slice_pattern_view`),
//! `option_value_from` / `cancelled_sentinel`, last-use analysis
//! (`compute_block_last_use`, `push_drops_for_stmt`), the scope-chain
//! `Env` struct with its impl, and the free-identifier scanning
//! helpers (`add_pattern_bindings`, `collect_free_idents_block`,
//! `collect_free_idents_expr`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use crate::ast::*;

use super::value::{EnumData, Value};

// ── Control Flow Signals ────────────────────────────────────────

/// Signals for non-local control flow (return, break, continue, exit).
#[derive(Debug)]
pub(crate) enum ControlFlow {
    Return(Value),
    Break {
        label: Option<String>,
        value: Option<Value>,
    },
    Continue {
        label: Option<String>,
    },
    /// process::exit() — defer-respecting, uncatchable exit.
    /// Distinct from Return so future catch_panic cannot swallow it.
    ExitUnwind {
        code: i32,
    },
    /// A user-triggered runtime error. The error details are in
    /// `Interpreter::runtime_errors`; this variant is the unwind signal.
    RuntimeError,
    /// A `par {}` sibling branch observed the shared cancel flag at
    /// a between-statement effect-boundary check. The propagating
    /// branch's `errdefer` phase fires with `e = Cancelled` per
    /// design.md § Drop ordering within a branch. `eval_par_block`
    /// silences this on the result side — the originating branch's
    /// real `Err` is the scope's return value under fail-fast.
    Cancelled,
    /// The active `karac test` invocation observed its per-test
    /// deadline at a between-statement boundary check. Distinct from
    /// `Cancelled` so the test runner can distinguish "timed out"
    /// from `par {}` cancellation, and so user `errdefer` blocks
    /// don't fire (the timeout is a runner-side guardrail, not a
    /// user-visible error path). Classifies as `ExitPath::Normal` —
    /// cleanup actions still fire so any heap state is released, but
    /// no errdefer / Err propagation. The runner reads the
    /// `Interpreter.timed_out` flag after `run_test_function` returns
    /// to surface the timeout outcome as a JSONL event.
    TimedOut,
}

impl ControlFlow {
    /// Whether this signal is an *unwind* — a fault or a scope-wide abort
    /// that is propagating outward — as opposed to ordinary, value-carrying
    /// control flow (`return` / `break` / `continue`).
    ///
    /// The distinction is what makes a fault outrank the statement it
    /// faulted inside of. These four are exactly the variants
    /// `call_function` groups into its "propagate up the stack" arm.
    pub(crate) fn is_unwind(&self) -> bool {
        matches!(
            self,
            ControlFlow::ExitUnwind { .. }
                | ControlFlow::RuntimeError
                | ControlFlow::Cancelled
                | ControlFlow::TimedOut
        )
    }
}

pub(crate) type EvalResult = Result<Value, ControlFlow>;

// ── Unified drop+defer cleanup stack ────────────────────────────

/// One entry in a block's unified drop+defer cleanup stack. Per
/// design.md § Drop ordering within a branch, destructors and
/// `defer` blocks interleave in a single program-order LIFO stack.
pub(crate) enum CleanupAction {
    /// A `defer { ... }` block.
    Defer(Block),
    /// A binding's destructor slot. The action is a no-op today — the
    /// Phase 6 user-`Drop` and Rc/Arc-decrement wiring attaches here
    /// without disturbing program-order LIFO position.
    #[allow(dead_code)]
    Drop { name: String },
    /// B-2026-08-30-51 — a binding SHADOWED by a later `let` of the same name
    /// in the same scope, carrying its own value.
    ///
    /// Every other slot is name-keyed and resolves through the env when it
    /// fires, which is exactly what shadowing breaks: two `let`s of one name
    /// push two slots with the same key, and by drain time the env holds only
    /// the survivor, so both fired on it -- the shadowed value's body never ran
    /// and the survivor's ran twice. Freezing the value into the slot at the
    /// shadowing `let` is what makes the two distinguishable, and it is sound
    /// precisely because nothing can reach the shadowed value again: the name
    /// that addressed it is gone.
    DropShadowed {
        name: String,
        value: super::value::Value,
    },
}

/// One entry in a block's `errdefer` stack (phase-1 cleanup, error
/// paths only). Kept separate from the unified drop+defer stack
/// because `errdefer` always fires before any destructor or `defer`.
pub(crate) struct ErrDeferEntry {
    pub(crate) binding: Option<String>,
    pub(crate) body: Block,
}

/// Classification of a block's exit path, used to drive `errdefer`
/// behavior. Param-less `errdefer` fires on every error path;
/// `errdefer(e)` only binds when a payload is available.
pub(crate) enum ExitPath {
    Normal,
    Err(Value),
    NoneProp,
    Panic,
    /// `par {}` cancellation — sub-step 4 emits this from cancelled
    /// siblings so `errdefer(e)` binds `e` to `Cancelled`.
    #[allow(dead_code)]
    Cancelled(Value),
}

impl ExitPath {
    pub(crate) fn classify(cf: &ControlFlow) -> ExitPath {
        match cf {
            ControlFlow::Return(Value::EnumVariant { variant, data, .. }) if variant == "Err" => {
                let payload = match data {
                    EnumData::Tuple(vs) => vs.first().cloned().unwrap_or(Value::Unit),
                    _ => Value::Unit,
                };
                ExitPath::Err(payload)
            }
            ControlFlow::Return(Value::EnumVariant { variant, .. }) if variant == "None" => {
                ExitPath::NoneProp
            }
            ControlFlow::Cancelled => ExitPath::Cancelled(cancelled_sentinel()),
            ControlFlow::RuntimeError | ControlFlow::ExitUnwind { .. } => ExitPath::Panic,
            // `TimedOut` is a runner-side guardrail, not a user-visible
            // error path — classify as Normal so user `errdefer` blocks
            // do not fire on test timeout. Cleanup actions (Drop /
            // Defer) still drain via the unified stack, so heap state
            // is released even on the timeout path.
            ControlFlow::TimedOut => ExitPath::Normal,
            _ => ExitPath::Normal,
        }
    }

    /// Classify a block's TAIL VALUE (no `ControlFlow` involved) — the
    /// function-tail failure path. `classify` above answers the same question
    /// for a propagating `ControlFlow::Return`; this one answers it for a
    /// value that simply IS the error, which is what a tail `Err(...)` or
    /// `None` produces. Callers gate on `ast::is_error_exit_value` first, so
    /// this only ever sees a syntactic error-exit tail; the `Normal` fallback
    /// covers a value that turned out to be neither variant.
    ///
    /// The variant match is by NAME, so a user enum with its own `Err(..)`
    /// variant classifies as an error exit too. That is harmless rather than
    /// merely unlikely: the typechecker permits `errdefer` only in
    /// `Result`/`Option`-returning functions, so a function returning such an
    /// enum has an EMPTY errdefer list and phase 1 of `run_cleanup` iterates
    /// nothing — the drop+defer drain that actually runs is identical either
    /// way.
    pub(crate) fn classify_tail_value(v: &Value) -> ExitPath {
        match v {
            Value::EnumVariant { variant, data, .. } if variant == "Err" => {
                let payload = match data {
                    EnumData::Tuple(vs) => vs.first().cloned().unwrap_or(Value::Unit),
                    _ => Value::Unit,
                };
                ExitPath::Err(payload)
            }
            Value::EnumVariant { variant, .. } if variant == "None" => ExitPath::NoneProp,
            _ => ExitPath::Normal,
        }
    }

    pub(crate) fn is_error(&self) -> bool {
        !matches!(self, ExitPath::Normal)
    }
}

impl std::fmt::Debug for ExitPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExitPath::Normal => write!(f, "Normal"),
            ExitPath::Err(_) => write!(f, "Err(_)"),
            ExitPath::NoneProp => write!(f, "NoneProp"),
            ExitPath::Panic => write!(f, "Panic"),
            ExitPath::Cancelled(_) => write!(f, "Cancelled(_)"),
        }
    }
}

/// Convert an integer value sitting at a FLOAT-typed destination into the
/// float it is declared to be (B-2026-08-14-2).
///
/// Kāra's widening coercions are implicit — `check_int_widening_coercion`
/// rejects only narrowing — so an `i32` may legally appear wherever an `f64` is
/// declared, with no `as` in the source. Codegen performs that conversion at
/// every boundary (B-2026-08-13-18); the interpreter performed it NOWHERE, so
/// the int simply stayed an int in a float slot. That is not a cosmetic
/// difference: the operator dispatch has no mixed Int/Float arm, so
/// `let x: f64 = some_u8; x == 200.0` did not answer wrong — it ABORTED, with a
/// message asserting a typecheck error that `karac check` does not report.
///
/// Routed through `cast_value` so the destination's real storage precision
/// applies: an `f32` slot rounds through `f32`, `f16`/`bf16` through their own
/// helpers. Without that, `2147483647i32` into an `f32` would read back exactly
/// on this surface and as `2147483648` on every compiled one.
///
/// Deliberately narrow. Only `Value::Int` at a float-named destination is
/// touched — a value that is already a Float, a non-float destination, and
/// every non-path type expression all pass through unchanged, so nothing that
/// works today can change. Tuples recurse element-wise so an annotated
/// `(f64, f64)` binding converts both halves.
pub(crate) fn coerce_int_value_to_declared_float(
    val: Value,
    te: &TypeExpr,
    src_unsigned_width: Option<u32>,
) -> Value {
    coerce_int_value_to_declared_float_elems(val, te, src_unsigned_width, &[])
}

/// [`coerce_int_value_to_declared_float`] with PER-ELEMENT source signedness
/// for an aggregate literal (B-2026-08-30-48, half (a)).
///
/// The single `src_unsigned_width` is the width of the RHS *expression*, and a
/// tuple / array literal's own type is not an integer — so it was always
/// `None` and every element converted as SIGNED. `let t: (f64, i64) = (u, 1)`
/// with `u: u64 = u64::MAX` therefore read -1 where both compiled backends
/// read 1.8446744073709552e19. Each element has its own source expression and
/// its own signedness, so the caller resolves them and passes them positionally.
///
/// `elem_widths` is consulted only at the TOP level of an aggregate; a nested
/// aggregate's elements fall back to `src_unsigned_width` exactly as before.
/// That is the measured shape (a flat literal) and keeping the recursion
/// width-agnostic avoids inventing an index scheme for a case no measurement
/// reached — see the row's own SCOPE NOTE about this shape space.
pub(crate) fn coerce_int_value_to_declared_float_elems(
    val: Value,
    te: &TypeExpr,
    src_unsigned_width: Option<u32>,
    elem_widths: &[Option<u32>],
) -> Value {
    /// The element type of a fixed-size array annotation, in either spelling
    /// the parser produces: the dedicated `TypeKind::Array` node, and the
    /// `Array[T, N]` PATH form that Kāra's `[]` generic syntax yields.
    /// `Vec[T]` is deliberately NOT accepted — see the call site.
    fn array_elem_te(te: &TypeExpr) -> Option<TypeExpr> {
        match &te.kind {
            TypeKind::Array { element, .. } => Some((**element).clone()),
            TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Array") => {
                match p.generic_args.as_ref()?.first()? {
                    GenericArg::Type(t) => Some(t.clone()),
                    _ => None,
                }
            }
            _ => None,
        }
    }
    fn float_head(te: &TypeExpr) -> Option<&str> {
        match &te.kind {
            TypeKind::Path(p) if p.generic_args.is_none() => {
                match p.segments.last().map(|s| s.as_str()) {
                    Some(h @ ("f16" | "bf16" | "f32" | "f64" | "float")) => Some(h),
                    _ => None,
                }
            }
            _ => None,
        }
    }
    match (&val, &te.kind) {
        (Value::Int(_), _) => match float_head(te) {
            Some(head) => super::eval_expr::cast_value(val, head, src_unsigned_width),
            None => val,
        },
        (Value::Tuple(_), TypeKind::Tuple(elem_tes)) => {
            let Value::Tuple(items) = val else {
                unreachable!()
            };
            if items.len() != elem_tes.len() {
                return Value::Tuple(items);
            }
            Value::Tuple(
                items
                    .into_iter()
                    .zip(elem_tes.iter())
                    .enumerate()
                    .map(|(i, (v, t))| {
                        let w = elem_widths.get(i).copied().flatten().or(src_unsigned_width);
                        coerce_int_value_to_declared_float(v, t, w)
                    })
                    .collect(),
            )
        }
        // `let a: Array[f64, 2] = [v, v]` — a fixed-size array annotation names
        // its element type, so every slot converts. `Vec[f64]` is handled by its
        // own arm below rather than here, because `Array[T, N]` and `Vec[T]`
        // reach their element type through different spellings.
        (Value::Array(_), _) if array_elem_te(te).is_some() => {
            let element = array_elem_te(te).unwrap();
            let Value::Array(rc) = &val else {
                unreachable!()
            };
            let converted: Vec<Value> = rc
                .read()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let w = elem_widths.get(i).copied().flatten().or(src_unsigned_width);
                    coerce_int_value_to_declared_float(v.clone(), &element, w)
                })
                .collect();
            *rc.write().unwrap() = converted;
            val
        }
        // B-2026-08-30-48 — the shapes that converted NOT AT ALL, leaving a
        // `Value::Int` in a slot the program declared `f64`. Each derives its
        // element/payload type from the ANNOTATION's own generic argument, so
        // no program-wide lookup is needed and the conversion is the same one
        // the scalar arm above performs.
        //
        // These arms cover the ANNOTATED-BINDING site only. A later MUTATION
        // (`vp.push(v)`, `mp.insert(k, v)`) has no annotation in reach, and is
        // covered by the separate `float_coerced_arg_sites` channel: the
        // typechecker records the argument span when it checks it against the
        // container's element type, and `coerce_float_slot_arg` converts at the
        // store. `Vec.push` was already wired to it (B-2026-08-14-6); the two
        // `Map`/`SortedMap` insert sites are wired in the same commit as this.
        (Value::Array(_), _) if generic_elem_te(te, &["Vec", "Slice"], 0).is_some() => {
            let element = generic_elem_te(te, &["Vec", "Slice"], 0).unwrap();
            let Value::Array(rc) = &val else {
                unreachable!()
            };
            let converted: Vec<Value> = rc
                .read()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let w = elem_widths.get(i).copied().flatten().or(src_unsigned_width);
                    coerce_int_value_to_declared_float(v.clone(), &element, w)
                })
                .collect();
            *rc.write().unwrap() = converted;
            val
        }
        // `let o: Option[f64] = Some(n)` / `let r: Result[f64, E] = Ok(n)`.
        // Only the SUCCESS payload is the annotation's first generic argument;
        // `Err`'s type is the second, so it is converted against that instead
        // rather than being coerced to the ok type.
        (Value::EnumVariant { .. }, _)
            if generic_elem_te(te, &["Option", "Result"], 0).is_some() =>
        {
            let Value::EnumVariant {
                enum_name,
                variant,
                data,
            } = val
            else {
                unreachable!()
            };
            let idx = if variant == "Err" { 1 } else { 0 };
            let data = match (data, generic_elem_te(te, &["Option", "Result"], idx)) {
                (EnumData::Tuple(vals), Some(t)) => EnumData::Tuple(
                    vals.into_iter()
                        .enumerate()
                        .map(|(i, v)| {
                            let w = elem_widths.get(i).copied().flatten().or(src_unsigned_width);
                            coerce_int_value_to_declared_float(v, &t, w)
                        })
                        .collect(),
                ),
                (d, _) => d,
            };
            Value::EnumVariant {
                enum_name,
                variant,
                data,
            }
        }
        // `let m: Map[i64, f64] = …` — the VALUE half only; a key is the first
        // generic argument and is left alone.
        (Value::Map(_), _) if generic_elem_te(te, &["Map", "SortedMap"], 1).is_some() => {
            let element = generic_elem_te(te, &["Map", "SortedMap"], 1).unwrap();
            let Value::Map(rc) = &val else { unreachable!() };
            let (hasher, converted) = {
                let guard = rc.read().unwrap();
                let entries: Vec<(Value, Value)> = guard
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            coerce_int_value_to_declared_float(
                                v.clone(),
                                &element,
                                src_unsigned_width,
                            ),
                        )
                    })
                    .collect();
                (guard.hasher(), entries)
            };
            // Rebuilt through `from_entries_with_hasher` rather than assigned
            // field-wise so the map keeps the hasher it was BUILT with
            // (B-2026-08-21-6) and its index stays consistent with the entries.
            *rc.write().unwrap() =
                super::value::MapData::from_entries_with_hasher(hasher, converted);
            val
        }
        _ => val,
    }
}

/// The `idx`-th generic argument of `te` when its head is one of `heads` —
/// `Option[f64]` -> `f64`, `Map[i64, f64]` at idx 1 -> `f64`. `None` for any
/// other shape, which is what keeps the arms above off every unrelated value.
fn generic_elem_te(te: &TypeExpr, heads: &[&str], idx: usize) -> Option<TypeExpr> {
    let TypeKind::Path(p) = &te.kind else {
        return None;
    };
    if !heads.contains(&p.segments.last()?.as_str()) {
        return None;
    }
    match p.generic_args.as_ref()?.get(idx)? {
        GenericArg::Type(t) => Some(t.clone()),
        _ => None,
    }
}

/// Deep-clone a `Value`, materializing independent storage for the
/// by-value collection variants (`Array`, `Map`, `Set`, `Tuple`,
/// `Struct`, `EnumVariant`, `Slice`). The derived `Clone` on `Value`
/// shallow-clones `Array` / `SortedSet` etc. (the `Arc<RwLock<...>>`
/// is bumped, sharing storage) — that's the right default for most
/// dispatch paths since slice tracking depends on the shared cell.
/// But operations whose Kāra-spec semantics produce *independent*
/// copies (e.g., `Vec.filled[T: Clone]`) must materialize fresh
/// storage per slot, otherwise nested-collection element types alias
/// across copies.
///
/// Reference-semantics types (`SharedStruct`, `Sender`, `Receiver`,
/// `SharedCell`, `Atomic`) preserve aliasing — those types are
/// shared-by-design per Kāra's `shared struct` and channel rules.
pub(crate) fn deep_clone_value(v: &Value) -> Value {
    match v {
        Value::Array(rc) => {
            let items: Vec<Value> = rc.read().unwrap().iter().map(deep_clone_value).collect();
            Value::array_of(items)
        }
        Value::Slice {
            storage,
            start,
            len,
            ..
        } => {
            // A deep clone of a slice produces an independent owned
            // snapshot — the original window's storage is left alone.
            let snapshot: Vec<Value> = storage.read().unwrap()[*start..*start + *len]
                .iter()
                .map(deep_clone_value)
                .collect();
            Value::array_of(snapshot)
        }
        Value::Set(items) => {
            Value::set_of(items.read().unwrap().iter().map(deep_clone_value).collect())
        }
        Value::Map(entries) => Value::map_of(
            entries
                .read()
                .unwrap()
                .iter()
                .map(|(k, val)| (deep_clone_value(k), deep_clone_value(val)))
                .collect(),
        ),
        Value::SortedMap(entries) => Value::SortedMap(
            entries
                .iter()
                .map(|(k, val)| {
                    (
                        super::value::OrdValue(deep_clone_value(&k.0)),
                        deep_clone_value(val),
                    )
                })
                .collect(),
        ),
        Value::Tuple(items) => Value::Tuple(items.iter().map(deep_clone_value).collect()),
        Value::Struct { name, fields } => Value::Struct {
            name: name.clone(),
            fields: fields
                .iter()
                .map(|(k, val)| (k.clone(), deep_clone_value(val)))
                .collect(),
        },
        Value::EnumVariant {
            enum_name,
            variant,
            data,
        } => Value::EnumVariant {
            enum_name: enum_name.clone(),
            variant: variant.clone(),
            data: match data {
                EnumData::Unit => EnumData::Unit,
                EnumData::Tuple(vals) => {
                    EnumData::Tuple(vals.iter().map(deep_clone_value).collect())
                }
                EnumData::Struct(fields) => EnumData::Struct(
                    fields
                        .iter()
                        .map(|(k, val)| (k.clone(), deep_clone_value(val)))
                        .collect(),
                ),
            },
        },
        // Primitives, String, SortedSet (primitive-keyed), and the
        // reference-semantics types (SharedStruct, Sender, Receiver,
        // SharedCell, Atomic) all clone correctly under the derive.
        // (SortedMap is materialized explicitly above so collection *values*
        // get fresh storage rather than an aliased Arc bump.)
        _ => v.clone(),
    }
}

/// Uniform view of a slice-pattern scrutinee — `(storage, offset, len,
/// source_mutable)`. `Value::Array` exposes its entire backing at offset
/// 0 (immutable for the rest-binding mode flag); `Value::Slice`
/// re-exposes its existing window with the inherited mutability flag.
type SlicePatternView = (Arc<RwLock<Vec<Value>>>, usize, usize, bool);

/// View a slice-pattern scrutinee as a `SlicePatternView`. The
/// rest binding's mutability mirrors the source. Returns `None` for
/// any other Value variant (the typechecker rejects non-sequence
/// scrutinees, so this is a defensive never-match fallback if reached).
pub(crate) fn slice_pattern_view(value: &Value) -> Option<SlicePatternView> {
    match value {
        Value::Array(rc) => {
            let len = rc.read().unwrap().len();
            Some((rc.clone(), 0, len, false))
        }
        Value::Slice {
            storage,
            start,
            len,
            mutable,
        } => Some((storage.clone(), *start, *len, *mutable)),
        _ => None,
    }
}

/// Wrap a `Some(Value)` / `None` Rust option in the corresponding
/// Kāra `Option[T]` enum variant. Used by `pop_back` / `pop_front` —
/// any method whose return type is `Option[T]` and whose Rust impl
/// already produces an `Option<Value>`.
pub(crate) fn option_value_from(v: Option<Value>) -> Value {
    match v {
        Some(inner) => Value::EnumVariant {
            enum_name: "Option".to_string(),
            variant: "Some".to_string(),
            data: EnumData::Tuple(vec![inner]),
        },
        None => Value::EnumVariant {
            enum_name: "Option".to_string(),
            variant: "None".to_string(),
            data: EnumData::Unit,
        },
    }
}

/// Sentinel value bound to `errdefer(e)` in cancelled `par {}` siblings.
/// Per design.md § Drop ordering within a branch, the real value should
/// come from `E::cancelled()` where `E` is the function's `Err` type and
/// `E: Cancellable`; until that trait + factory wiring lands in the
/// typechecker, a placeholder unit-variant carries the right shape.
pub(crate) fn cancelled_sentinel() -> Value {
    Value::EnumVariant {
        enum_name: "Cancelled".to_string(),
        variant: "Cancelled".to_string(),
        data: EnumData::Unit,
    }
}

/// Per-binding last-use index map used by `eval_block_inner` to
/// fire `Drop` slots at the live-range end (NLL placement) instead
/// of waiting for scope exit. Per design.md § Drop ordering within
/// a branch, NLL drops happen at the binding's last-use program
/// point; this map tells the block evaluator which statement to
/// fire each binding's `Drop` after.
///
/// Sentinel: `stmts.len()` means "scope exit" — the binding is
/// referenced in the block's `final_expr`, in any registered
/// defer/errdefer body, or in any nested-block construct that the
/// shallow walker conservatively treats as opaque. Drops with this
/// sentinel stay in `cleanup` and drain via the unified LIFO at
/// scope exit, preserving defer/drop interleave for that case.
///
/// The walker is intentionally conservative — it only fires NLL
/// drops when it can prove the binding is dead. Cross-block
/// liveness (CFG dataflow) is out of scope for this round.
/// NLL last-use indices with the pre-B-2026-08-30-51 never-read fallback: the
/// FIRST `let` of a name wins. CODEGEN reads this, and must keep reading it.
///
/// B-2026-08-31-5. B-2026-08-30-51 moved that fallback to the LAST `let`, which
/// the interpreter needs, and fed it to both backends because they share this
/// function. On codegen it REGRESSED auto-par programs: `emit_par_run` outlines
/// a statement range into a `__par_branch_*` worker, and while that region is
/// emitted the insert block is terminated, so `fire_due_user_drops` early-returns
/// for exactly the statement indices the region spans. Moving a never-read
/// shadowed binding's endpoint from the first `let` (before the region) to the
/// last (inside it) therefore moved its body from the shadowing `let` to
/// FUNCTION EXIT. Measured: `KARAC_AUTO_PAR=0` at COMPILE time makes the same
/// program correct, which is what localizes it to the outlining rather than to
/// the endpoint.
///
/// So the two callers want different answers until codegen's outlining and its
/// NLL placement are reconciled, and they say so here rather than sharing one
/// and hoping. Codegen's own never-read shadowed ORDERING is imperfect under
/// this fallback — it fires the first generation before the shadowing `let` and
/// the second at scope exit — but that is what it did before B-2026-08-30-51 and
/// is tracked as its own defect rather than traded for a worse one.
///
/// `allow(dead_code)` on the DEFAULT leg, not a `cfg`: every caller lives in
/// `src/codegen`, which is behind `--features llvm`, so without this the
/// function is unused there and `cargo clippy --all --all-targets -- -D
/// warnings` — the leg CI runs — fails. Keeping the body compiled on both legs
/// rather than `cfg`-ing it out keeps this doc comment's contrast with the
/// shadow-aware sibling meaningful wherever someone reads it.
#[cfg_attr(not(feature = "llvm"), allow(dead_code))]
/// B-2026-08-30-51 / B-2026-08-31-6 — a SHADOWED name's never-read endpoint is
/// its LAST `let`, not its first, and BOTH backends now ask for that.
///
/// It was two entry points until B-2026-08-31-6. The interpreter needed the
/// last-`let` endpoint because its shadow slots are created AT the shadowing
/// `let`, so an endpoint pinned to the first one precedes the surviving slot's
/// existence and that slot never fires by NLL at all. Codegen was held on the
/// first-`let` endpoint by a hazard that no longer exists: an auto-par group
/// SWALLOWED the firing point of every statement it covered (see
/// `par_group_swallows_nll_drop`), which made the correct endpoint the
/// worst-of-four under outlining and the wrong one merely wrong. With the
/// swallowing closed, the last-`let` endpoint is right in both columns —
/// measured on B-2026-08-31-6's own program, where `KARAC_AUTO_PAR=0` went from
/// `dR3 mid c=0 dR4` to the interpreter's `dR4 dR3 mid c=0`.
///
/// Collapsing the two is the point, not a tidy-up: a per-backend endpoint is a
/// run-vs-build divergence held in place by a `bool`, and every drop-position
/// row in the ledger is some version of the two backends answering one question
/// differently. One function, one answer.
pub(crate) fn compute_block_last_use(block: &Block) -> HashMap<String, usize> {
    // Collect every binding the block introduces.
    let mut owned: HashSet<String> = HashSet::new();
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                for n in pattern.binding_names() {
                    owned.insert(n);
                }
            }
            StmtKind::LetUninit { name, .. } => {
                owned.insert(name.clone());
            }
            _ => {}
        }
    }
    if owned.is_empty() {
        return HashMap::new();
    }
    let scope_exit = block.stmts.len();
    let mut last_use: HashMap<String, usize> = HashMap::new();

    // Per-statement free-idents walk. We only care which `owned`
    // bindings each statement *references* — outer-block bindings
    // shadowed by inner constructs already get filtered by the
    // walker's `bound` tracking when it descends into nested blocks.
    // We pass a fresh empty `bound` set per stmt so the OUTER `owned`
    // names always show up as free idents.
    let record_use = |name: String,
                      idx: usize,
                      owned: &HashSet<String>,
                      last_use: &mut HashMap<String, usize>,
                      scope_exit: usize| {
        if !owned.contains(&name) {
            return;
        }
        // Pinned-to-scope-exit wins; otherwise advance to the latest idx.
        match last_use.get(&name).copied() {
            Some(prev) if prev == scope_exit => {}
            _ => {
                last_use.insert(name, idx);
            }
        }
    };
    for (idx, stmt) in block.stmts.iter().enumerate() {
        let mut idents: Vec<String> = Vec::new();
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            // A defer/errdefer body executes at scope exit. Any
            // binding it references must remain live until then —
            // pin those to `scope_exit`.
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                let mut bound: HashSet<String> = HashSet::new();
                collect_free_idents_block(body, &mut bound, &mut idents);
                for name in idents {
                    if owned.contains(&name) {
                        last_use.insert(name, scope_exit);
                    }
                }
                continue;
            }
            // Let RHS uses outer scope; the new pattern binding takes
            // effect for subsequent statements.
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                let mut bound: HashSet<String> = HashSet::new();
                collect_free_idents_expr(value, &mut bound, &mut idents);
            }
            StmtKind::LetUninit { .. } => {}
            StmtKind::Assign { target, value } => {
                let mut bound: HashSet<String> = HashSet::new();
                collect_free_idents_expr(target, &mut bound, &mut idents);
                collect_free_idents_expr(value, &mut bound, &mut idents);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                let mut bound: HashSet<String> = HashSet::new();
                collect_free_idents_expr(target, &mut bound, &mut idents);
                collect_free_idents_expr(value, &mut bound, &mut idents);
            }
            StmtKind::Expr(expr) => {
                let mut bound: HashSet<String> = HashSet::new();
                collect_free_idents_expr(expr, &mut bound, &mut idents);
            }
        }
        for name in idents {
            record_use(name, idx, &owned, &mut last_use, scope_exit);
        }
    }
    // The block's `final_expr` (if any) runs after the last stmt
    // but before scope-exit cleanup drains. A binding referenced
    // there must stay live until scope exit so the unified LIFO
    // drain interleaves it with any Defers correctly.
    if let Some(final_expr) = &block.final_expr {
        let mut idents: Vec<String> = Vec::new();
        let mut bound: HashSet<String> = HashSet::new();
        collect_free_idents_expr(final_expr, &mut bound, &mut idents);
        for name in idents {
            if owned.contains(&name) {
                last_use.insert(name, scope_exit);
            }
        }
    }
    // Bindings introduced but never read: NLL says they die
    // immediately after the let — last_use = the let's own index.
    //
    // B-2026-08-30-51 — for a SHADOWED name with no read anywhere, the LAST
    // such `let` wins, not the first. This map is name-keyed, so every
    // generation of a shadowed name shares one endpoint; pinning it to the
    // FIRST `let` puts that endpoint before the surviving binding's slot even
    // exists, so the survivor never fires by NLL and drained at scope exit
    // instead -- measured as `let b = E.V(mk(3)); let b = E.V(mk(4));` running
    // the second body at the end of `main` while both compiled backends ran it
    // at the shadowing `let`. Only a name filled by THIS fallback may be
    // overwritten: a real read recorded above still wins, which is what
    // `or_insert` was protecting.
    let mut filled_here: HashSet<String> = HashSet::new();
    let mut note_unread = |n: String, idx: usize, last_use: &mut HashMap<String, usize>| {
        if last_use.contains_key(&n) {
            // A later `let` of a name THIS loop already filled moves the
            // endpoint forward; a real read recorded earlier still wins, which
            // is what the pre-B-2026-08-30-51 `or_insert` was protecting.
            if !filled_here.contains(&n) {
                return;
            }
        }
        filled_here.insert(n.clone());
        last_use.insert(n, idx);
    };
    for stmt_idx in 0..block.stmts.len() {
        let stmt = &block.stmts[stmt_idx];
        match &stmt.kind {
            StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                for n in pattern.binding_names() {
                    note_unread(n, stmt_idx, &mut last_use);
                }
            }
            StmtKind::LetUninit { name, .. } => {
                note_unread(name.clone(), stmt_idx, &mut last_use);
            }
            _ => {}
        }
    }
    last_use
}

/// Push a `Drop` action for each binding the statement introduced.
/// Called after the statement evaluates successfully, so the drop
/// slot lands at the program-order LIFO position the binding
/// claims in the unified stack.
/// One owed field-held `shared struct` release (B-2026-09-03-9), parked by a
/// draining drop slot and performed at the enclosing block's scope exit —
/// which is where the compiled backends put a refcount release, while a
/// holder's PLAIN Drop-bearing fields fire at its NLL endpoint.
///
/// Two shapes because the two holders differ in whether their slot survives
/// long enough to be re-read. A plain struct or tuple holder keeps its binding
/// until scope exit, so its release is addressed BY NAME and the count is read
/// fresh at drain. A `shared` holder does not: its own arm releases the slot as
/// soon as it drains, so the values it held are CAPTURED first and carried
/// here — which is also what makes the drain's `strong_count == 1` test exact.
#[derive(Debug, Clone)]
pub(crate) enum PendingRelease {
    /// Re-read `name`'s slot at scope exit and release what it holds.
    Binding(String),
    /// A `(type name, value)` clone taken out of a dying `shared` holder.
    Captured(String, Value),
}

pub(crate) fn push_drops_for_stmt(stmt: &Stmt, cleanup: &mut Vec<CleanupAction>) {
    match &stmt.kind {
        StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
            for name in pattern.binding_names() {
                cleanup.push(CleanupAction::Drop { name });
            }
        }
        StmtKind::LetUninit { name, .. } => {
            cleanup.push(CleanupAction::Drop { name: name.clone() });
        }
        _ => {}
    }
}

// ── Scoped Environment ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) struct Env {
    pub(crate) scopes: Vec<HashMap<String, Value>>,
    /// Active slot-write watches (B-2026-07-28-7), innermost last. Each entry
    /// is `(binding name, was it written)`; `set` flips the flag of every entry
    /// naming the slot it writes. `eval_match` uses this to learn whether a
    /// match arm body REPLACED its scrutinee's storage, which makes the
    /// payload write-through unsound — see `pattern_match::eval_match`.
    ///
    /// A `Vec` rather than a single slot because matches nest: an inner match
    /// must not hide a write from an enclosing one, so a write marks every
    /// frame that names it, not just the innermost.
    pub(crate) watches: Vec<(String, bool)>,
}

impl Env {
    pub(crate) fn new() -> Self {
        Env {
            scopes: vec![HashMap::new()],
            watches: Vec::new(),
        }
    }

    /// Start watching `name` for slot writes. Pair with `pop_watch`.
    pub(crate) fn push_watch(&mut self, name: &str) {
        self.watches.push((name.to_string(), false));
    }

    /// Stop watching the innermost watched name; `true` if it was written
    /// while the watch was active.
    pub(crate) fn pop_watch(&mut self) -> bool {
        self.watches.pop().is_some_and(|(_, written)| written)
    }

    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    pub(crate) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// B-2026-08-30-51 — the value bound to `name` in the INNERMOST scope only,
    /// ignoring every enclosing one.
    ///
    /// Shadowing within one scope and shadowing an OUTER binding are different
    /// events: the outer binding keeps its own slot in its own block's cleanup
    /// and is still live after this block ends, so it must not be frozen here.
    /// `get` cannot tell them apart -- it searches outward.
    pub(crate) fn get_in_current_scope(&self, name: &str) -> Option<Value> {
        self.scopes.last().and_then(|s| s.get(name)).cloned()
    }

    /// B-2026-08-28-43 — `true` when `name` is bound in the OUTERMOST scope and
    /// nowhere inner, i.e. no local shadows it.
    ///
    /// The outermost scope is the one the item pass seeds: free-fn values,
    /// variant constructors, and — the case this exists for — a bare unit
    /// variant's constant (`B`, alongside the qualified `E.B`). Ownership sites
    /// need to tell that seeded constant apart from a LOCAL of enum type,
    /// because they mean opposite things: the constant is a fresh temp whose
    /// drop nobody else owns, while a local's drop belongs to its binding and a
    /// second caller-side fire would double it. A plain `env.get(name).is_some()`
    /// cannot separate them — both answer `Some` — which is why the bare
    /// spelling of a unit variant ran no body in argument position at all.
    ///
    /// Codegen twin: `fresh_bare_unit_variant_enum`, which mirrors
    /// `compile_expr`'s resolution order for the same purpose.
    pub(crate) fn is_outermost_only(&self, name: &str) -> bool {
        if self.scopes.iter().skip(1).any(|s| s.contains_key(name)) {
            return false;
        }
        self.scopes.first().is_some_and(|s| s.contains_key(name))
    }

    pub(crate) fn define(&mut self, name: String, val: Value) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, val);
        }
    }

    /// Remove a binding from the nearest scope that holds it, releasing
    /// its value (for a shared struct, dropping this holder's `Arc` and
    /// decrementing the strong-count). Used by the shared-struct user-Drop
    /// drain so a later alias's drain observes the decremented count — see
    /// `Interpreter::invoke_user_drop_if_applicable`. Safe at a drain
    /// point because the binding is at its NLL endpoint or scope exit and
    /// is never read again.
    pub(crate) fn remove_local(&mut self, name: &str) {
        for scope in self.scopes.iter_mut().rev() {
            if scope.remove(name).is_some() {
                return;
            }
        }
    }

    pub(crate) fn set(&mut self, name: &str, val: Value) {
        // Update in the nearest scope that has this name. Two slot kinds
        // redirect the write instead of overwriting the binding:
        //   - `SharedCell`: a `mut ref` closure capture aliased back to the
        //     outer binding — write through the cell so the outer binding
        //     observes the mutation.
        //   - `MapSlotRef`: a `mut ref V` into a Map slot, returned by
        //     `Entry.or_insert` — write through to the live map slot so
        //     `*r = v` / `r += 1` land in the map, not the local binding.
        // Record the write against any active watch (B-2026-07-28-7) BEFORE
        // dispatching on the slot kind, so a redirected write (SharedCell /
        // MapSlotRef / VecSlotRef) counts too — each still replaces what the
        // name denotes, which is what a watcher cares about.
        if !self.watches.is_empty() {
            for (watched, written) in self.watches.iter_mut() {
                if watched == name {
                    *written = true;
                }
            }
        }
        let mut redirect: Option<(String, Value)> = None;
        let mut vec_redirect: Option<(Arc<RwLock<Vec<Value>>>, usize)> = None;
        for scope in self.scopes.iter_mut().rev() {
            if let Some(slot) = scope.get_mut(name) {
                match slot {
                    Value::SharedCell(cell) => {
                        *cell.lock().unwrap() = val;
                        return;
                    }
                    Value::MapSlotRef { map_var, key } => {
                        redirect = Some((map_var.clone(), (**key).clone()));
                    }
                    // `iter_mut` element ref (B-2026-07-14-10): `*x = v` writes
                    // through the shared element storage to the live Vec slot.
                    Value::VecSlotRef { storage, index } => {
                        vec_redirect = Some((storage.clone(), *index));
                    }
                    _ => {
                        *slot = val;
                        return;
                    }
                }
                break;
            }
        }
        if let Some((map_var, key)) = redirect {
            self.write_map_slot(&map_var, &key, val);
            return;
        }
        if let Some((storage, index)) = vec_redirect {
            if let Ok(mut g) = storage.write() {
                if index < g.len() {
                    g[index] = val;
                }
            }
            return;
        }
        // If not found, define in current scope
        self.define(name.to_string(), val);
    }

    /// Current scope nesting depth. Used to key a deferred shared-field
    /// release to the block that owes it (B-2026-09-03-9).
    pub(crate) fn scope_depth(&self) -> usize {
        self.scopes.len()
    }

    /// Borrow a binding's slot WITHOUT cloning it. `get` clones, which for
    /// any `SharedStruct` reachable from the slot bumps the `Arc`
    /// strong-count and defeats a last-reference test — the same reason
    /// `drop_target` exists for a BARE shared slot. This is its
    /// arbitrarily-deep sibling, used by the field-held shared-struct drop
    /// hook (B-2026-09-03-9) to read counts through a holder's fields.
    ///
    /// The three aliasing slot kinds `get` auto-derefs (`SharedCell`,
    /// `MapSlotRef`, `VecSlotRef`) answer `None` rather than the aliasing
    /// slot itself: each denotes storage owned elsewhere, so a holder's
    /// death is not the release of what it points at.
    pub(crate) fn slot_ref(&self, name: &str) -> Option<&Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return match v {
                    Value::SharedCell(_) | Value::MapSlotRef { .. } | Value::VecSlotRef { .. } => {
                        None
                    }
                    other => Some(other),
                };
            }
        }
        None
    }

    /// Read a binding by name. Auto-derefs `SharedCell` (a closure mut-ref
    /// alias) and `MapSlotRef` (an `or_insert` mut-ref into a Map slot) so
    /// callers always see the underlying value rather than the aliasing
    /// slot / place-ref.
    pub(crate) fn get(&self, name: &str) -> Option<Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(match v {
                    Value::SharedCell(cell) => cell.lock().unwrap().clone(),
                    Value::MapSlotRef { map_var, key } => self.read_map_slot(map_var, key),
                    Value::VecSlotRef { storage, index } => storage
                        .read()
                        .unwrap()
                        .get(*index)
                        .cloned()
                        .unwrap_or(Value::Unit),
                    other => other.clone(),
                });
            }
        }
        None
    }

    /// Resolve a map PLACE to a borrow: either a plain binding name (`m`) or a
    /// dotted field path rooted at one (`h.buckets`, `self.buckets`).
    ///
    /// B-2026-08-18-34. Every map reference the entry chain builds
    /// (`Value::Entry`, `Value::MapSlotRef`) names its map by BINDING NAME, so a
    /// map living in a struct FIELD had no way to be named and the chain
    /// degraded to `Value::Unit` — surfacing as "method 'push' not found on type
    /// 'unknown'". A dot cannot occur in a Kāra identifier, nor in the `__`-
    /// prefixed synthetic names, so it unambiguously marks a path here.
    ///
    /// Only value structs are walked. A `shared struct` field returns `None`
    /// and so behaves exactly as it did before this existed, rather than being
    /// half-supported: its fields carry interior mutability and would need the
    /// write to go through the cell.
    pub(crate) fn map_place_ref(&self, place: &str) -> Option<&Value> {
        let mut parts = place.split('.');
        let root = parts.next()?;
        let mut cur = self.scopes.iter().rev().find_map(|s| s.get(root))?;
        for field in parts {
            cur = match cur {
                Value::Struct { fields, .. } => fields.get(field)?,
                _ => return None,
            };
        }
        Some(cur)
    }

    /// Mutable peer of [`Self::map_place_ref`], for the write-through half of
    /// an `or_insert` slot reference.
    pub(crate) fn map_place_mut(&mut self, place: &str) -> Option<&mut Value> {
        let mut parts = place.split('.');
        let root = parts.next()?;
        let mut cur = self.scopes.iter_mut().rev().find_map(|s| s.get_mut(root))?;
        for field in parts {
            cur = match cur {
                Value::Struct { fields, .. } => fields.get_mut(field)?,
                _ => return None,
            };
        }
        Some(cur)
    }

    /// Resolve a `MapSlotRef` to the current value in its Map slot. Returns
    /// `Value::Unit` if the binding is gone or the key is absent (a dangling
    /// ref — `or_insert` always inserts, so this is defensive against a
    /// `remove` racing a held ref).
    pub(crate) fn read_map_slot(&self, map_var: &str, key: &Value) -> Value {
        match self.map_place_ref(map_var) {
            Some(Value::Map(pairs)) => pairs
                .read()
                .unwrap()
                .get(key)
                .cloned()
                .unwrap_or(Value::Unit),
            // SortedMap slot (BTreeMap keyed by `OrdValue`) — the entry
            // chain's `MapSlotRef` resolves through here for a SortedMap too.
            Some(Value::SortedMap(m)) => m
                .get(&super::value::OrdValue(key.clone()))
                .cloned()
                .unwrap_or(Value::Unit),
            _ => Value::Unit,
        }
    }

    /// Write `val` through a `MapSlotRef` into its Map slot. Appends a new
    /// `(key, val)` pair if the key is missing (defensive; `or_insert`
    /// normally guarantees presence). No-op if the binding is gone.
    pub(crate) fn write_map_slot(&mut self, map_var: &str, key: &Value, val: Value) {
        match self.map_place_mut(map_var) {
            Some(Value::Map(pairs)) => {
                pairs.write().unwrap().insert(key.clone(), val);
            }
            // SortedMap sibling: insert-or-overwrite by `OrdValue` key.
            Some(Value::SortedMap(m)) => {
                m.insert(super::value::OrdValue(key.clone()), val);
            }
            _ => {}
        }
    }

    /// For the user-`Drop` hook: report a binding's struct type name and,
    /// when the binding is a shared struct, its current `Arc` strong-count
    /// — WITHOUT cloning the slot. (`get` clones, which for a shared struct
    /// would bump the count and defeat the last-reference test.) Returns
    /// `None` when the binding is absent or is neither a value struct nor a
    /// bare `SharedStruct` slot. The `Option<usize>` is `None` for a value
    /// struct (no refcount) and `Some(count)` for a shared struct.
    pub(crate) fn drop_target(&self, name: &str) -> Option<(String, Option<usize>)> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return match v {
                    Value::Struct { name, .. } => Some((name.clone(), None)),
                    Value::SharedStruct(inner) => {
                        Some((inner.name.clone(), Some(Arc::strong_count(inner))))
                    }
                    // Value ENUM binding (B-2026-07-01-8): before this arm the
                    // interpreter never resolved an enum binding's type, so a
                    // user `impl Drop for <Enum>` NEVER fired under `karac run`
                    // (codegen fired it) — a run-vs-build divergence for any
                    // RAII-observing program. Same no-count contract as the
                    // value-struct arm; `invoke_user_drop_if_applicable`'s
                    // `drop_method_keys` gate keeps builtin enums
                    // (Option/Result/Ordering) inert.
                    Value::EnumVariant { enum_name, .. } => Some((enum_name.clone(), None)),
                    _ => None,
                };
            }
        }
        None
    }

    /// Snapshot current env for closure capture. Preserves `SharedCell`
    /// slots verbatim so a captured `mut ref` alias keeps pointing at the
    /// shared cell when the closure dispatches.
    pub(crate) fn snapshot(&self) -> HashMap<String, Value> {
        let mut all = HashMap::new();
        for scope in &self.scopes {
            for (k, v) in scope {
                all.insert(k.clone(), v.clone());
            }
        }
        all
    }

    /// Promote a binding's slot to `SharedCell`, if it isn't one already,
    /// and return a clone of the resulting cell value (also a `SharedCell`)
    /// so callers can install the same alias into a closure's captured-env
    /// map. Used at construction of a `mut ref |...|` closure to convert
    /// each captured outer binding into an aliased cell so mutations made
    /// inside the closure body propagate back.
    pub(crate) fn wrap_capture(&mut self, name: &str) -> Option<Value> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(slot) = scope.get_mut(name) {
                if !matches!(slot, Value::SharedCell(_)) {
                    let inner = std::mem::replace(slot, Value::Unit);
                    *slot = Value::SharedCell(Arc::new(Mutex::new(inner)));
                }
                return Some(slot.clone());
            }
        }
        None
    }
}

// ── Free-variable analysis for `mut ref |...|` closures ────────
//
// Walks a closure body collecting every identifier that resolves outside
// the closure (i.e. is not introduced by a closure param, body-local
// `let`, pattern binding, or nested closure param). The interpreter uses
// this set to decide which outer-scope bindings to promote to
// `Value::SharedCell` so mutations propagate back. Conservative against
// shadowing: a name that appears in the body before a `let` of the same
// name is captured; a name that appears only after the `let` is treated
// as the inner shadow and not captured.
pub(crate) fn add_pattern_bindings(pat: &Pattern, out: &mut HashSet<String>) {
    for n in pat.binding_names() {
        out.insert(n);
    }
}

pub(crate) fn collect_free_idents_block(
    block: &Block,
    bound: &mut HashSet<String>,
    out: &mut Vec<String>,
) {
    let snapshot = bound.clone();
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { pattern, value, .. } => {
                collect_free_idents_expr(value, bound, out);
                add_pattern_bindings(pattern, bound);
            }
            StmtKind::LetUninit { name, .. } => {
                bound.insert(name.clone());
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                collect_free_idents_expr(value, bound, out);
                let snap = bound.clone();
                collect_free_idents_block(else_block, bound, out);
                *bound = snap;
                add_pattern_bindings(pattern, bound);
            }
            StmtKind::Defer { body } => collect_free_idents_block(body, bound, out),
            StmtKind::ErrDefer { body, binding } => {
                let snap = bound.clone();
                if let Some(n) = binding {
                    bound.insert(n.clone());
                }
                collect_free_idents_block(body, bound, out);
                *bound = snap;
            }
            StmtKind::Assign { target, value } => {
                collect_free_idents_expr(target, bound, out);
                collect_free_idents_expr(value, bound, out);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                collect_free_idents_expr(target, bound, out);
                collect_free_idents_expr(value, bound, out);
            }
            StmtKind::Expr(e) => collect_free_idents_expr(e, bound, out),
        }
    }
    if let Some(final_expr) = &block.final_expr {
        collect_free_idents_expr(final_expr, bound, out);
    }
    *bound = snapshot;
}

pub(crate) fn collect_free_idents_expr(
    expr: &Expr,
    bound: &mut HashSet<String>,
    out: &mut Vec<String>,
) {
    match &expr.kind {
        ExprKind::Identifier(name) => {
            if !bound.contains(name) {
                out.push(name.clone());
            }
        }
        ExprKind::Path { .. }
        | ExprKind::Integer(_, _)
        | ExprKind::Float(_, _)
        | ExprKind::Bool(_)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::ByteStringLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::PipePlaceholder
        | ExprKind::Continue { .. }
        | ExprKind::OffsetOf { .. }
        | ExprKind::Error => {}
        ExprKind::InterpolatedStringLit(parts) => {
            for part in parts {
                if let crate::ast::ParsedInterpolationPart::Expr(e, _) = part {
                    collect_free_idents_expr(e, bound, out);
                }
            }
        }
        ExprKind::Binary { left, right, .. } => {
            collect_free_idents_expr(left, bound, out);
            collect_free_idents_expr(right, bound, out);
        }
        ExprKind::Unary { operand, .. } => {
            collect_free_idents_expr(operand, bound, out);
        }
        ExprKind::Call { callee, args } => {
            collect_free_idents_expr(callee, bound, out);
            for arg in args {
                collect_free_idents_expr(&arg.value, bound, out);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            collect_free_idents_expr(object, bound, out);
            for arg in args {
                collect_free_idents_expr(&arg.value, bound, out);
            }
        }
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            collect_free_idents_expr(object, bound, out);
        }
        ExprKind::OptionalChain { object, args, .. } => {
            collect_free_idents_expr(object, bound, out);
            if let Some(args) = args {
                for arg in args {
                    collect_free_idents_expr(&arg.value, bound, out);
                }
            }
        }
        ExprKind::NilCoalesce { left, right } => {
            collect_free_idents_expr(left, bound, out);
            collect_free_idents_expr(right, bound, out);
        }
        ExprKind::Index { object, index } => {
            collect_free_idents_expr(object, bound, out);
            collect_free_idents_expr(index, bound, out);
        }
        ExprKind::Block(b) | ExprKind::Comptime(b) => collect_free_idents_block(b, bound, out),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_free_idents_expr(condition, bound, out);
            collect_free_idents_block(then_block, bound, out);
            if let Some(eb) = else_branch {
                collect_free_idents_expr(eb, bound, out);
            }
        }
        ExprKind::IfLet {
            pattern,
            value,
            then_block,
            else_branch,
        } => {
            collect_free_idents_expr(value, bound, out);
            let snapshot = bound.clone();
            add_pattern_bindings(pattern, bound);
            collect_free_idents_block(then_block, bound, out);
            *bound = snapshot;
            if let Some(eb) = else_branch {
                collect_free_idents_expr(eb, bound, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_free_idents_expr(condition, bound, out);
            collect_free_idents_block(body, bound, out);
        }
        ExprKind::WhileLet {
            pattern,
            value,
            body,
            ..
        } => {
            collect_free_idents_expr(value, bound, out);
            let snapshot = bound.clone();
            add_pattern_bindings(pattern, bound);
            collect_free_idents_block(body, bound, out);
            *bound = snapshot;
        }
        ExprKind::Loop { body, .. } => collect_free_idents_block(body, bound, out),
        ExprKind::LabeledBlock { body, .. } => collect_free_idents_block(body, bound, out),
        ExprKind::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            collect_free_idents_expr(iterable, bound, out);
            let snapshot = bound.clone();
            add_pattern_bindings(pattern, bound);
            collect_free_idents_block(body, bound, out);
            *bound = snapshot;
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_free_idents_expr(scrutinee, bound, out);
            for arm in arms {
                let snapshot = bound.clone();
                add_pattern_bindings(&arm.pattern, bound);
                if let Some(g) = &arm.guard {
                    collect_free_idents_expr(g, bound, out);
                }
                collect_free_idents_expr(&arm.body, bound, out);
                *bound = snapshot;
            }
        }
        ExprKind::Closure { params, body, .. } => {
            let snapshot = bound.clone();
            for p in params {
                add_pattern_bindings(&p.pattern, bound);
            }
            collect_free_idents_expr(body, bound, out);
            *bound = snapshot;
        }
        ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
            for it in items {
                collect_free_idents_expr(it, bound, out);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for it in items {
                collect_free_idents_expr(it, bound, out);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            collect_free_idents_expr(value, bound, out);
            collect_free_idents_expr(count, bound, out);
        }
        ExprKind::MapLiteral(entries) => {
            for (k, v) in entries {
                collect_free_idents_expr(k, bound, out);
                collect_free_idents_expr(v, bound, out);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                collect_free_idents_expr(&f.value, bound, out);
            }
            if let Some(s) = spread {
                collect_free_idents_expr(s, bound, out);
            }
        }
        ExprKind::Return(opt) => {
            if let Some(e) = opt {
                collect_free_idents_expr(e, bound, out);
            }
        }
        ExprKind::Break { value: opt, .. } => {
            if let Some(e) = opt {
                collect_free_idents_expr(e, bound, out);
            }
        }
        ExprKind::Question(inner) | ExprKind::Cast { expr: inner, .. } => {
            collect_free_idents_expr(inner, bound, out);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                collect_free_idents_expr(s, bound, out);
            }
            if let Some(e) = end {
                collect_free_idents_expr(e, bound, out);
            }
        }
        ExprKind::Pipe { left, right } => {
            collect_free_idents_expr(left, bound, out);
            collect_free_idents_expr(right, bound, out);
        }
        ExprKind::Par(b) | ExprKind::Seq(b) | ExprKind::Unsafe(b) | ExprKind::Try(b) => {
            collect_free_idents_block(b, bound, out);
        }
        ExprKind::Lock { mutex, body, alias } => {
            // The place expression (`m`, `self.state`) is evaluated in the outer
            // scope, so its free identifiers are captured.
            collect_free_idents_expr(mutex, bound, out);
            let snap = bound.clone();
            if let Some(a) = alias {
                bound.insert(a.clone());
            }
            collect_free_idents_block(body, bound, out);
            *bound = snap;
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                collect_free_idents_expr(&b.value, bound, out);
            }
            collect_free_idents_block(body, bound, out);
        }
    }
}
