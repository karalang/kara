//! Free helper functions used across the interpreter.
//!
//! Houses value-comparison helpers (`value_compare`, `value_discriminant`),
//! the `eval_stats_fn` stdlib stats dispatch, the encoding family
//! (base64/hex/url encode + decode), I/O result wrappers
//! (`io_ok`/`io_err_value`/`io_error_from_std`), the HTTP request
//! emitters (`eval_http_get`/`eval_http_post`, `make_response`,
//! `make_http_error`, `wrap_ok_response`), and JSON ↔ Value
//! conversion (`serde_json_to_kara_json`, `kara_json_to_serde_json`,
//! `make_json_error`).
//!
//! All `pub(super)` so the `Interpreter` impl in `super` can call them.

use std::collections::HashMap;

use crate::token::Span;

use super::{EnumData, Value};

pub(super) fn value_compare(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Char(x), Value::Char(y)) => x.cmp(y),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::Tuple(xs), Value::Tuple(ys)) => xs
            .iter()
            .zip(ys.iter())
            .find_map(|(a, b)| {
                let ord = value_compare(a, b);
                if ord != Ordering::Equal {
                    Some(ord)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| xs.len().cmp(&ys.len())),
        // Two Vecs (B-2026-06-30-15): lexicographic elementwise, then by
        // length — the missing arm that made `Vec[Vec[..]].sort()` a silent
        // NO-OP under the interpreter (both Arrays fell to the discriminant
        // fallback → always Equal → stable sort preserved insertion order;
        // the ledger's "the interpreter handles nested Vecs" premise was
        // itself wrong). Same-Arc operands double-read-lock, which is fine
        // single-threaded (and sorts hold only the OUTER vec's write lock).
        (Value::Array(a), Value::Array(b)) => {
            let av = a.read().unwrap();
            let bv = b.read().unwrap();
            av.iter()
                .zip(bv.iter())
                .find_map(|(x, y)| {
                    let ord = value_compare(x, y);
                    if ord != Ordering::Equal {
                        Some(ord)
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| av.len().cmp(&bv.len()))
        }
        // Two Slices: compare the viewed ranges the same way.
        (
            Value::Slice {
                storage: sa,
                start: a0,
                len: la,
                ..
            },
            Value::Slice {
                storage: sb,
                start: b0,
                len: lb,
                ..
            },
        ) => {
            let av = sa.read().unwrap();
            let bv = sb.read().unwrap();
            let a_view = &av[*a0..*a0 + *la];
            let b_view = &bv[*b0..*b0 + *lb];
            a_view
                .iter()
                .zip(b_view.iter())
                .find_map(|(x, y)| {
                    let ord = value_compare(x, y);
                    if ord != Ordering::Equal {
                        Some(ord)
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| la.cmp(lb))
        }
        // Two Maps: lexicographic over (key, value) pairs in insertion order
        (Value::Map(a), Value::Map(b)) => a
            .iter()
            .zip(b.iter())
            .find_map(|((ak, av), (bk, bv))| {
                let k_ord = value_compare(ak, bk);
                if k_ord != Ordering::Equal {
                    Some(k_ord)
                } else {
                    let v_ord = value_compare(av, bv);
                    if v_ord != Ordering::Equal {
                        Some(v_ord)
                    } else {
                        None
                    }
                }
            })
            .unwrap_or_else(|| a.len().cmp(&b.len())),
        // Two SortedSets: lexicographic over their ascending key sequences
        (Value::SortedSet(a), Value::SortedSet(b)) => {
            let ak: Vec<_> = a.keys().collect();
            let bk: Vec<_> = b.keys().collect();
            ak.iter()
                .zip(bk.iter())
                .find_map(|(x, y)| {
                    let ord = value_compare(&x.0, &y.0);
                    if ord != Ordering::Equal {
                        Some(ord)
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| ak.len().cmp(&bk.len()))
        }
        // Two SortedMaps: lexicographic over their ascending (key, value) pairs
        (Value::SortedMap(a), Value::SortedMap(b)) => a
            .iter()
            .zip(b.iter())
            .find_map(|((ak, av), (bk, bv))| {
                let k_ord = value_compare(&ak.0, &bk.0);
                if k_ord != Ordering::Equal {
                    Some(k_ord)
                } else {
                    let v_ord = value_compare(av, bv);
                    if v_ord != Ordering::Equal {
                        Some(v_ord)
                    } else {
                        None
                    }
                }
            })
            .unwrap_or_else(|| a.len().cmp(&b.len())),
        // Two Structs: order by type name, then by fields in derived-`Ord`
        // DECLARATION order (B-2026-07-03-12), recovered from the per-thread
        // `type_order` registry; when the registry is absent (or the type is
        // unknown) fall back to the alphabetical field-name order introduced in
        // B-2026-07-03-6. Either way the order is consistent with `Value::eq`
        // (equal iff every field is equal, so distinct structs never compare
        // `Equal`) and is a proper total order — sound for the `OrdValue`-keyed
        // BTreeMap backing `SortedSet` / `SortedMap` and for `Vec[Struct].sort()`.
        // Without an arm here both structs fell to the `_ => discriminant`
        // fallback where every struct maps to the same catch-all discriminant →
        // always `Equal` → distinct keys collapsed (silent DATA LOSS) and
        // `sort()` was a NO-OP.
        (
            Value::Struct {
                name: an,
                fields: af,
            },
            Value::Struct {
                name: bn,
                fields: bf,
            },
        ) => an.cmp(bn).then_with(|| compare_struct_fields(an, af, bf)),
        // Two enum variants: order by enum name, then variant DECLARATION index
        // (B-2026-07-03-12 — so `Priority { Low, Med, High }` sorts
        // `Low < Med < High`, not alphabetically `High < Low < Med`), then
        // payload. Falls back to variant-name order when the registry is
        // absent. Same silent-data-loss / no-op class as structs above without
        // the arm.
        (
            Value::EnumVariant {
                enum_name: an,
                variant: av,
                data: ad,
            },
            Value::EnumVariant {
                enum_name: bn,
                variant: bv,
                data: bd,
            },
        ) => an
            .cmp(bn)
            .then_with(|| compare_variant_order(an, av, bv))
            .then_with(|| compare_enum_data(an, av, ad, bd)),
        // Cross-variant ordering by discriminant index
        _ => value_discriminant(a).cmp(&value_discriminant(b)),
    }
}

/// Compare two struct field maps for a struct named `type_name`, in
/// derived-`Ord` DECLARATION order when the per-thread `type_order` registry
/// knows the type, else in the alphabetical fallback order. Both are proper
/// total orders consistent with `Value::eq`.
fn compare_struct_fields(
    type_name: &str,
    a: &HashMap<String, Value>,
    b: &HashMap<String, Value>,
) -> std::cmp::Ordering {
    if let Some(reg) = crate::interpreter::type_order::current() {
        if let Some(order) = reg.struct_field_order(type_name) {
            return compare_field_maps_ordered(a, b, order);
        }
    }
    compare_field_maps(a, b)
}

/// Compare two variant names of enum `enum_name` by DECLARATION index (registry
/// present), else alphabetically by name (fallback).
fn compare_variant_order(enum_name: &str, av: &str, bv: &str) -> std::cmp::Ordering {
    if let Some(reg) = crate::interpreter::type_order::current() {
        if let Some(order) = reg.enum_variant_order(enum_name) {
            if let (Some(x), Some(y)) = (order.get(av), order.get(bv)) {
                return x.cmp(y);
            }
        }
    }
    av.cmp(bv)
}

/// Compare two struct field maps in the declaration order given by `order`
/// (field name → declaration index). Any field absent from `order` (should not
/// happen for a well-formed type) sorts after all known fields, then
/// alphabetically, purely for determinism.
fn compare_field_maps_ordered(
    a: &HashMap<String, Value>,
    b: &HashMap<String, Value>,
    order: &HashMap<String, u32>,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
    keys.sort();
    keys.dedup();
    keys.sort_by(|x, y| {
        let ix = order.get(*x).copied().unwrap_or(u32::MAX);
        let iy = order.get(*y).copied().unwrap_or(u32::MAX);
        ix.cmp(&iy).then_with(|| x.cmp(y))
    });
    for k in keys {
        let ord = match (a.get(k), b.get(k)) {
            (Some(x), Some(y)) => value_compare(x, y),
            (Some(_), None) => Ordering::Greater,
            (None, Some(_)) => Ordering::Less,
            (None, None) => Ordering::Equal,
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

/// Deterministic total order over two struct field maps: compare each field
/// in sorted-field-name order. Consistent with the field-map content equality
/// `Value::eq` uses (returns `Equal` iff every shared field is equal and the
/// key sets match), so it never conflates two distinct structs. A field
/// present in only one side orders that side greater (defensive — same-type
/// structs always share a key set). The alphabetical fallback used when the
/// `type_order` registry can't supply declaration order.
fn compare_field_maps(
    a: &HashMap<String, Value>,
    b: &HashMap<String, Value>,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
    keys.sort();
    keys.dedup();
    for k in keys {
        let ord = match (a.get(k), b.get(k)) {
            (Some(x), Some(y)) => value_compare(x, y),
            (Some(_), None) => Ordering::Greater,
            (None, Some(_)) => Ordering::Less,
            (None, None) => Ordering::Equal,
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

/// Total order over two enum payloads for variant `variant` of enum
/// `enum_name`. Reached only after the enclosing variant names matched, so both
/// payloads share a shape in well-typed code; the cross-shape arm orders by a
/// stable shape rank purely for totality. Tuple payloads compare positionally
/// (inherent order); struct payloads compare in derived-`Ord` DECLARATION order
/// via the `type_order` registry (B-2026-07-03-12), falling back to
/// alphabetical field order when absent.
fn compare_enum_data(
    enum_name: &str,
    variant: &str,
    a: &EnumData,
    b: &EnumData,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (EnumData::Unit, EnumData::Unit) => Ordering::Equal,
        (EnumData::Tuple(x), EnumData::Tuple(y)) => x
            .iter()
            .zip(y.iter())
            .find_map(|(p, q)| {
                let o = value_compare(p, q);
                (o != Ordering::Equal).then_some(o)
            })
            .unwrap_or_else(|| x.len().cmp(&y.len())),
        (EnumData::Struct(x), EnumData::Struct(y)) => {
            if let Some(reg) = crate::interpreter::type_order::current() {
                if let Some(order) = reg.variant_field_order(enum_name, variant) {
                    return compare_field_maps_ordered(x, y, order);
                }
            }
            compare_field_maps(x, y)
        }
        _ => enum_data_rank(a).cmp(&enum_data_rank(b)),
    }
}

fn enum_data_rank(d: &EnumData) -> u8 {
    match d {
        EnumData::Unit => 0,
        EnumData::Tuple(_) => 1,
        EnumData::Struct(_) => 2,
    }
}

pub(super) fn value_discriminant(v: &Value) -> u8 {
    match v {
        Value::Int(_) => 0,
        Value::Float(_) => 1,
        Value::Bool(_) => 2,
        Value::Char(_) => 3,
        Value::String(_) => 4,
        Value::Tuple(_) => 5,
        Value::Array(_) => 6,
        Value::Unit => 7,
        Value::Map(_) => 12,
        Value::SortedSet(_) => 9,
        Value::SortedMap(_) => 14,
        Value::Set(_) => 13,
        Value::Sender(_) => 10,
        Value::Receiver(_) => 11,
        _ => 8,
    }
}

// ── Stats stdlib helpers ─────────────────────────────────────────────────────

pub(super) fn eval_stats_fn(name: &str, xs: &[f64], p: Option<f64>, span: &Span) -> Value {
    use crate::reduce_kernel::{quantile_linear_sorted, reduce_f64, ReduceOp, ReduceOutcome};

    // The `Stats.*` slice is population-form variance/stddev (÷ n), distinct
    // from `Column.var`/`std`'s sample (÷ n−1) form, and its empty-input
    // policy is per-op: `sum`/`prod` return the identity, `min`/`max`/`arg*`
    // return `None`, `sort`/`argsort` return empty, and
    // `mean`/`variance`/`stddev`/`median`/`percentile` trap. The `panic!`
    // trap mechanism (vs `Column`/`Tensor`'s `record_runtime_error`) is
    // preserved here at the call site; the arithmetic funnels through
    // `crate::reduce_kernel`.
    let float = |o: ReduceOutcome| match o {
        ReduceOutcome::Scalar(f) => Value::Float(f),
        _ => unreachable!("stats scalar op returned non-scalar outcome"),
    };
    match name {
        "Stats.sum" => float(reduce_f64(xs, ReduceOp::Sum)),
        "Stats.prod" => float(reduce_f64(xs, ReduceOp::Prod)),
        "Stats.mean" => {
            if xs.is_empty() {
                panic!(
                    "Stats.mean() called on empty slice at {}:{}",
                    span.line, span.column
                );
            }
            float(reduce_f64(xs, ReduceOp::Mean))
        }
        "Stats.variance" => {
            if xs.is_empty() {
                panic!(
                    "Stats.variance() called on empty slice at {}:{}",
                    span.line, span.column
                );
            }
            float(reduce_f64(xs, ReduceOp::Var { bessel: false }))
        }
        "Stats.stddev" => {
            if xs.is_empty() {
                panic!(
                    "Stats.stddev() called on empty slice at {}:{}",
                    span.line, span.column
                );
            }
            float(reduce_f64(xs, ReduceOp::Std { bessel: false }))
        }
        "Stats.median" => {
            if xs.is_empty() {
                panic!(
                    "Stats.median() called on empty slice at {}:{}",
                    span.line, span.column
                );
            }
            float(reduce_f64(xs, ReduceOp::Median))
        }
        "Stats.min" => match reduce_f64(xs, ReduceOp::Min) {
            ReduceOutcome::OptScalar(Some(v)) => stats_option_some(Value::Float(v)),
            _ => stats_option_none(),
        },
        "Stats.max" => match reduce_f64(xs, ReduceOp::Max) {
            ReduceOutcome::OptScalar(Some(v)) => stats_option_some(Value::Float(v)),
            _ => stats_option_none(),
        },
        // `percentile(p)` — NumPy/`np.percentile` convention: `p ∈ [0, 100]`
        // (distinct from `Column.quantile`'s `[0, 1]`), linear interpolation
        // between the two nearest ranks. `median ≡ percentile(50)`. Empty
        // slice or `p` out of range traps, mirroring the other f64 reductions.
        "Stats.percentile" => {
            if xs.is_empty() {
                panic!(
                    "Stats.percentile() called on empty slice at {}:{}",
                    span.line, span.column
                );
            }
            let p = p.unwrap_or(f64::NAN);
            if !(0.0..=100.0).contains(&p) {
                panic!(
                    "Stats.percentile() p must be in [0, 100], got {} at {}:{}",
                    p, span.line, span.column
                );
            }
            let ReduceOutcome::F64Vec(sorted) = reduce_f64(xs, ReduceOp::Sort) else {
                unreachable!("Sort returns F64Vec")
            };
            let pos = (p / 100.0) * (sorted.len() - 1) as f64;
            Value::Float(quantile_linear_sorted(&sorted, pos))
        }
        // `argmin` / `argmax` → `Option[i64]` (the index of the first min/max;
        // `None` on an empty slice, mirroring `min`/`max`'s `Option[f64]`).
        "Stats.argmin" | "Stats.argmax" => {
            let op = if name == "Stats.argmax" {
                ReduceOp::Argmax
            } else {
                ReduceOp::Argmin
            };
            match reduce_f64(xs, op) {
                ReduceOutcome::OptIndex(Some(i)) => stats_option_some(Value::Int(i)),
                _ => stats_option_none(),
            }
        }
        // `sort` → a fresh ascending `Vec[f64]` (the slice is borrowed and
        // unchanged); empty → empty.
        "Stats.sort" => {
            let ReduceOutcome::F64Vec(sorted) = reduce_f64(xs, ReduceOp::Sort) else {
                unreachable!("Sort returns F64Vec")
            };
            let elems: Vec<Value> = sorted.into_iter().map(Value::Float).collect();
            Value::Array(std::sync::Arc::new(std::sync::RwLock::new(elems)))
        }
        // `argsort` → `Vec[i64]` of the indices that sort `xs` ascending
        // (stable: ties keep input order); empty → empty.
        "Stats.argsort" => {
            let ReduceOutcome::I64Vec(idx) = reduce_f64(xs, ReduceOp::Argsort) else {
                unreachable!("Argsort returns I64Vec")
            };
            let elems: Vec<Value> = idx.into_iter().map(Value::Int).collect();
            Value::Array(std::sync::Arc::new(std::sync::RwLock::new(elems)))
        }
        _ => Value::Unit,
    }
}

/// The i64-element twin of [`eval_stats_fn`] (S5 — the non-f64 element axis;
/// `docs/spikes/reduce-elementwise-trait-unification.md`). The genuinely-int
/// ops stay exact at i64 through [`reduce_i64`]: `sum`/`prod` are
/// element-typed **checked** folds (overflow traps like the `+`/`*`
/// operators; empty → the INTEGER identities `0`/`1`, not the float
/// `-0.0`/`1.0`), `min`/`max` → `Option[i64]`, `sort` → `Vec[i64]`, and
/// `argmin`/`argmax`/`argsort` compare at i64 (no lossy float round-trip
/// above 2⁵³). The float statistics (`mean`/`variance`/`stddev`/`median`/
/// `percentile`) promote to `f64` — same trap policy as the f64 surface.
pub(super) fn eval_stats_fn_int(name: &str, xs: &[i64], p: Option<f64>, span: &Span) -> Value {
    use crate::reduce_kernel::{quantile_linear_sorted_i64, reduce_i64, ReduceOp, ReduceOutcome};

    let run = |op: ReduceOp| match reduce_i64(xs, op) {
        Ok(o) => o,
        Err(_) => panic!(
            "integer overflow in {}() at {}:{}",
            name, span.line, span.column
        ),
    };
    match name {
        "Stats.sum" | "Stats.prod" => {
            let op = if name == "Stats.sum" {
                ReduceOp::Sum
            } else {
                ReduceOp::Prod
            };
            match run(op) {
                ReduceOutcome::IntScalar(v) => Value::Int(v),
                _ => unreachable!("int sum/prod returns IntScalar"),
            }
        }
        "Stats.mean" | "Stats.variance" | "Stats.stddev" | "Stats.median" => {
            if xs.is_empty() {
                let m = name.strip_prefix("Stats.").unwrap_or(name);
                panic!(
                    "Stats.{}() called on empty slice at {}:{}",
                    m, span.line, span.column
                );
            }
            let op = match name {
                "Stats.mean" => ReduceOp::Mean,
                "Stats.variance" => ReduceOp::Var { bessel: false },
                "Stats.stddev" => ReduceOp::Std { bessel: false },
                _ => ReduceOp::Median,
            };
            match run(op) {
                ReduceOutcome::Scalar(f) => Value::Float(f),
                _ => unreachable!("int float-statistic returns Scalar"),
            }
        }
        "Stats.min" | "Stats.max" => {
            let op = if name == "Stats.min" {
                ReduceOp::Min
            } else {
                ReduceOp::Max
            };
            match run(op) {
                ReduceOutcome::OptIntScalar(Some(v)) => stats_option_some(Value::Int(v)),
                _ => stats_option_none(),
            }
        }
        "Stats.percentile" => {
            if xs.is_empty() {
                panic!(
                    "Stats.percentile() called on empty slice at {}:{}",
                    span.line, span.column
                );
            }
            let p = p.unwrap_or(f64::NAN);
            if !(0.0..=100.0).contains(&p) {
                panic!(
                    "Stats.percentile() p must be in [0, 100], got {} at {}:{}",
                    p, span.line, span.column
                );
            }
            let ReduceOutcome::I64Vec(sorted) = run(ReduceOp::Sort) else {
                unreachable!("int Sort returns I64Vec")
            };
            let pos = (p / 100.0) * (sorted.len() - 1) as f64;
            Value::Float(quantile_linear_sorted_i64(&sorted, pos))
        }
        "Stats.argmin" | "Stats.argmax" => {
            let op = if name == "Stats.argmax" {
                ReduceOp::Argmax
            } else {
                ReduceOp::Argmin
            };
            match run(op) {
                ReduceOutcome::OptIndex(Some(i)) => stats_option_some(Value::Int(i)),
                _ => stats_option_none(),
            }
        }
        "Stats.sort" | "Stats.argsort" => {
            let op = if name == "Stats.sort" {
                ReduceOp::Sort
            } else {
                ReduceOp::Argsort
            };
            let ReduceOutcome::I64Vec(v) = run(op) else {
                unreachable!("int sort/argsort returns I64Vec")
            };
            let elems: Vec<Value> = v.into_iter().map(Value::Int).collect();
            Value::Array(std::sync::Arc::new(std::sync::RwLock::new(elems)))
        }
        _ => Value::Unit,
    }
}

/// Numeric `Value` (`Int`/`Float`) as `f64`; non-numeric values (never
/// reached for a typechecked numeric container) fall back to `0.0`. Shared by
/// the `Column` and `Tensor` float-result reductions.
pub(super) fn value_as_f64(v: &Value) -> f64 {
    match v {
        Value::Int(i) => *i as f64,
        Value::Float(f) => *f,
        _ => 0.0,
    }
}

/// Min (or max) `Value` of a non-empty list, preserving the element type
/// (bare `T`, not `f64`): strict `<`/`>` keeps the first on a tie, and NaN
/// compares false against everything so it is never selected over a real
/// value (the scalar `<` posture). Shared by `Column.min`/`max` and
/// `Tensor.min`/`max`. Panics on an empty list — every caller guards
/// emptiness first.
pub(super) fn minmax_value_reduce(is_min: bool, vals: Vec<Value>) -> Value {
    let mut it = vals.into_iter();
    let mut acc = it.next().expect("non-empty");
    for x in it {
        let take = match (&acc, &x) {
            (Value::Int(a), Value::Int(b)) => {
                if is_min {
                    b < a
                } else {
                    b > a
                }
            }
            (Value::Float(a), Value::Float(b)) => {
                if is_min {
                    b < a
                } else {
                    b > a
                }
            }
            _ => false,
        };
        if take {
            acc = x;
        }
    }
    acc
}

/// `Some(v)` in the interpreter's `Value::EnumVariant` Option representation.
fn stats_option_some(v: Value) -> Value {
    Value::EnumVariant {
        enum_name: "Option".to_string(),
        variant: "Some".to_string(),
        data: EnumData::Tuple(vec![v]),
    }
}

/// `None` in the interpreter's `Value::EnumVariant` Option representation.
fn stats_option_none() -> Value {
    Value::EnumVariant {
        enum_name: "Option".to_string(),
        variant: "None".to_string(),
        data: EnumData::Unit,
    }
}

// ── Encoding stdlib helpers (Base64 / Hex / Url) ────────────────────────────

const BASE64_STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const BASE64_URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub(super) fn base64_encode(bytes: &[u8], url_safe: bool) -> String {
    let alphabet = if url_safe { BASE64_URL } else { BASE64_STD };
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(alphabet[((n >> 18) & 0x3f) as usize] as char);
        out.push(alphabet[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() >= 2 {
            out.push(alphabet[((n >> 6) & 0x3f) as usize] as char);
        } else if !url_safe {
            out.push('=');
        }
        if chunk.len() == 3 {
            out.push(alphabet[(n & 0x3f) as usize] as char);
        } else if !url_safe {
            out.push('=');
        }
    }
    out
}

pub(super) fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    fn decode_char(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let trimmed = s.trim_end_matches('=');
    let mut bytes = Vec::with_capacity(trimmed.len() * 3 / 4);
    let mut buf = [0u8; 4];
    let mut n = 0;
    for c in trimmed.bytes() {
        if c.is_ascii_whitespace() {
            continue;
        }
        let v =
            decode_char(c).ok_or_else(|| format!("invalid base64 character: {:?}", c as char))?;
        buf[n] = v;
        n += 1;
        if n == 4 {
            bytes.push((buf[0] << 2) | (buf[1] >> 4));
            bytes.push((buf[1] << 4) | (buf[2] >> 2));
            bytes.push((buf[2] << 6) | buf[3]);
            n = 0;
        }
    }
    match n {
        0 => {}
        1 => return Err("invalid base64 length: trailing single character".to_string()),
        2 => bytes.push((buf[0] << 2) | (buf[1] >> 4)),
        3 => {
            bytes.push((buf[0] << 2) | (buf[1] >> 4));
            bytes.push((buf[1] << 4) | (buf[2] >> 2));
        }
        _ => unreachable!(),
    }
    Ok(bytes)
}

pub(super) fn hex_encode(bytes: &[u8], upper: bool) -> String {
    let lut: &[u8; 16] = if upper {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(lut[(b >> 4) as usize] as char);
        out.push(lut[(b & 0xf) as usize] as char);
    }
    out
}

pub(super) fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    fn from_hex(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let bs = s.as_bytes();
    if !bs.len().is_multiple_of(2) {
        return Err(format!("invalid hex length: {} (must be even)", bs.len()));
    }
    let mut out = Vec::with_capacity(bs.len() / 2);
    for chunk in bs.chunks(2) {
        let hi = from_hex(chunk[0])
            .ok_or_else(|| format!("invalid hex character: {:?}", chunk[0] as char))?;
        let lo = from_hex(chunk[1])
            .ok_or_else(|| format!("invalid hex character: {:?}", chunk[1] as char))?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

pub(super) fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xf) as usize] as char);
            }
        }
    }
    out
}

pub(super) fn url_decode(s: &str) -> Result<String, String> {
    fn from_hex(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let bs = s.as_bytes();
    let mut out = Vec::with_capacity(bs.len());
    let mut i = 0;
    while i < bs.len() {
        if bs[i] == b'%' {
            if i + 2 >= bs.len() {
                return Err("incomplete percent-encoded sequence at end of input".to_string());
            }
            let hi = from_hex(bs[i + 1]).ok_or_else(|| {
                format!(
                    "invalid percent-encoded byte: %{}{}",
                    bs[i + 1] as char,
                    bs[i + 2] as char
                )
            })?;
            let lo = from_hex(bs[i + 2]).ok_or_else(|| {
                format!(
                    "invalid percent-encoded byte: %{}{}",
                    bs[i + 1] as char,
                    bs[i + 2] as char
                )
            })?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bs[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|e| format!("invalid UTF-8 in decoded URL: {e}"))
}

pub(super) fn decode_ok_bytes(bytes: Vec<u8>) -> Value {
    let arr: Vec<Value> = bytes.into_iter().map(|b| Value::Int(b as i64)).collect();
    Value::EnumVariant {
        enum_name: "Result".to_string(),
        variant: "Ok".to_string(),
        data: EnumData::Tuple(vec![Value::array_of(arr)]),
    }
}

pub(super) fn decode_ok_string(s: String) -> Value {
    Value::EnumVariant {
        enum_name: "Result".to_string(),
        variant: "Ok".to_string(),
        data: EnumData::Tuple(vec![Value::String(s)]),
    }
}

pub(super) fn decode_err(message: String) -> Value {
    let mut fields = HashMap::new();
    fields.insert("message".to_string(), Value::String(message));
    Value::EnumVariant {
        enum_name: "Result".to_string(),
        variant: "Err".to_string(),
        data: EnumData::Tuple(vec![Value::Struct {
            name: "DecodeError".to_string(),
            fields,
        }]),
    }
}

// ── I/O stdlib helpers ──────────────────────────────────────────────────────

pub(super) fn io_ok(val: Value) -> Value {
    Value::EnumVariant {
        enum_name: "Result".to_string(),
        variant: "Ok".to_string(),
        data: EnumData::Tuple(vec![val]),
    }
}

pub(super) fn io_err_value(io_error: Value) -> Value {
    Value::EnumVariant {
        enum_name: "Result".to_string(),
        variant: "Err".to_string(),
        data: EnumData::Tuple(vec![io_error]),
    }
}

pub(super) fn io_error_from_std(e: &std::io::Error) -> Value {
    let (variant, payload) = match e.kind() {
        std::io::ErrorKind::NotFound => ("NotFound", None),
        std::io::ErrorKind::PermissionDenied => ("PermissionDenied", None),
        std::io::ErrorKind::AlreadyExists => ("AlreadyExists", None),
        std::io::ErrorKind::UnexpectedEof => ("UnexpectedEof", None),
        std::io::ErrorKind::InvalidData => ("InvalidUtf8", None),
        std::io::ErrorKind::Interrupted => ("Interrupted", None),
        _ => ("Other", Some(e.to_string())),
    };
    Value::EnumVariant {
        enum_name: "IoError".to_string(),
        variant: variant.to_string(),
        data: match payload {
            None => EnumData::Unit,
            Some(msg) => EnumData::Tuple(vec![Value::String(msg)]),
        },
    }
}

// ── std.http helpers ──────────────────────────────────────────────────────────

// Native-only: the wasm32 build's http stubs short-circuit to
// `make_http_error` and never build a successful response.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn make_response(status: u16, body: String, headers: Vec<(String, String)>) -> Value {
    let mut fields = HashMap::new();
    fields.insert("status".to_string(), Value::Int(status as i64));
    fields.insert("body".to_string(), Value::String(body));
    let header_pairs: Vec<Value> = headers
        .into_iter()
        .map(|(k, v)| Value::Tuple(vec![Value::String(k), Value::String(v)]))
        .collect();
    // Store headers as a flat Vec<(k,v)> in a Map value for header() lookup.
    let map_pairs: Vec<(Value, Value)> = header_pairs
        .iter()
        .filter_map(|v| {
            if let Value::Tuple(ref kv) = v {
                if kv.len() == 2 {
                    return Some((kv[0].clone(), kv[1].clone()));
                }
            }
            None
        })
        .collect();
    fields.insert("headers".to_string(), Value::Map(map_pairs));
    Value::Struct {
        name: "Response".to_string(),
        fields,
    }
}

pub(super) fn make_http_error(message: String) -> Value {
    let mut fields = HashMap::new();
    fields.insert("message".to_string(), Value::String(message));
    Value::EnumVariant {
        enum_name: "Result".to_string(),
        variant: "Err".to_string(),
        data: EnumData::Tuple(vec![Value::Struct {
            name: "HttpError".to_string(),
            fields,
        }]),
    }
}

// `ureq` is native-only — wasm32 builds (browser playground, tracker
// line 703) replace these arms with stubs that surface a runtime
// `HttpError` so user code calling `Http.get` / `Http.post` fails
// cleanly instead of compile-erroring. The interpreter does not enforce
// effects, so a `reads(Net)` declaration on user code stays untouched.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn wrap_ok_response(resp: ureq::Response) -> Value {
    let status = resp.status();
    // Collect headers before consuming the response.
    let content_type = resp.header("content-type").unwrap_or("").to_string();
    let body = resp.into_string().unwrap_or_default();
    let mut headers = Vec::new();
    if !content_type.is_empty() {
        headers.push(("content-type".to_string(), content_type));
    }
    Value::EnumVariant {
        enum_name: "Result".to_string(),
        variant: "Ok".to_string(),
        data: EnumData::Tuple(vec![make_response(status, body, headers)]),
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn eval_http_get(url: &str) -> Value {
    match ureq::get(url).call() {
        Ok(resp) => wrap_ok_response(resp),
        Err(e) => make_http_error(e.to_string()),
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn eval_http_get(_url: &str) -> Value {
    make_http_error("Http.get is not available in the browser playground".to_string())
}

// ── Slice F (`std.json`) helpers ─────────────────────────────────────────
//
// Translation between `serde_json::Value` and the Kāra `Json` enum
// (modeled as `Value::EnumVariant { enum_name: "Json", ... }`). The
// interpreter dispatches `Json.parse(s)` and `j.stringify()` directly
// against `serde_json` rather than crossing the runtime FFI surface —
// the runtime crate's `karac_runtime_json_*` exports exist for codegen
// builds (Slice B's `Response.json[T: ToJson]` builder, deferred), but
// going through them from the interpreter is pure overhead since both
// sides link the same `serde_json` version.

/// Build a Kāra `Json` enum value from a `serde_json::Value` tree.
pub(super) fn serde_json_to_kara_json(v: &serde_json::Value) -> Value {
    let (variant, data) = match v {
        serde_json::Value::Null => ("Null", EnumData::Unit),
        serde_json::Value::Bool(b) => ("Bool", EnumData::Tuple(vec![Value::Bool(*b)])),
        serde_json::Value::Number(n) => (
            "Number",
            EnumData::Tuple(vec![Value::Float(n.as_f64().unwrap_or(0.0))]),
        ),
        serde_json::Value::String(s) => ("String", EnumData::Tuple(vec![Value::String(s.clone())])),
        serde_json::Value::Array(items) => {
            let xs: Vec<Value> = items.iter().map(serde_json_to_kara_json).collect();
            ("Array", EnumData::Tuple(vec![Value::array_of(xs)]))
        }
        serde_json::Value::Object(map) => {
            // Locked design (ii): Object backs a `Vec[(String, Json)]`.
            // The interpreter shape is `Value::Array` of `Value::Tuple`s.
            let pairs: Vec<Value> = map
                .iter()
                .map(|(k, val)| {
                    Value::Tuple(vec![Value::String(k.clone()), serde_json_to_kara_json(val)])
                })
                .collect();
            ("Object", EnumData::Tuple(vec![Value::array_of(pairs)]))
        }
    };
    Value::EnumVariant {
        enum_name: "Json".to_string(),
        variant: variant.to_string(),
        data,
    }
}

/// Inverse: walk a Kāra `Json` value and produce a `serde_json::Value`
/// for `serde_json::to_string`. Reads the variant tag off the
/// `EnumVariant`'s `variant` string and pulls the payload out of the
/// `EnumData::Tuple` slot. Mismatched shapes degrade to `null` rather
/// than panicking — pre-typecheck guarantees match the legal shape, but
/// defensiveness here keeps stringify side-effect-free under stress.
pub(super) fn kara_json_to_serde_json(v: &Value) -> serde_json::Value {
    let Value::EnumVariant {
        enum_name,
        variant,
        data,
    } = v
    else {
        return serde_json::Value::Null;
    };
    if enum_name != "Json" {
        return serde_json::Value::Null;
    }
    let payload = match data {
        EnumData::Unit => Vec::new(),
        EnumData::Tuple(vals) => vals.clone(),
        EnumData::Struct(_) => Vec::new(),
    };
    match variant.as_str() {
        "Null" => serde_json::Value::Null,
        "Bool" => match payload.first() {
            Some(Value::Bool(b)) => serde_json::Value::Bool(*b),
            _ => serde_json::Value::Null,
        },
        "Number" => match payload.first() {
            Some(Value::Float(f)) => serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            Some(Value::Int(i)) => serde_json::Number::from_f64(*i as f64)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            Some(Value::TotalFloat64(f)) => serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            Some(Value::TotalFloat32(f)) => serde_json::Number::from_f64(*f as f64)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            _ => serde_json::Value::Null,
        },
        "String" => match payload.first() {
            Some(Value::String(s)) => serde_json::Value::String(s.clone()),
            _ => serde_json::Value::Null,
        },
        "Array" => match payload.first() {
            Some(Value::Array(rc)) => {
                let items: Vec<serde_json::Value> = rc
                    .read()
                    .unwrap()
                    .iter()
                    .map(kara_json_to_serde_json)
                    .collect();
                serde_json::Value::Array(items)
            }
            _ => serde_json::Value::Null,
        },
        "Object" => match payload.first() {
            Some(Value::Array(rc)) => {
                let mut map = serde_json::Map::with_capacity(rc.read().unwrap().len());
                for entry in rc.read().unwrap().iter() {
                    if let Value::Tuple(t) = entry {
                        if t.len() == 2 {
                            if let Value::String(k) = &t[0] {
                                map.insert(k.clone(), kara_json_to_serde_json(&t[1]));
                            }
                        }
                    }
                }
                serde_json::Value::Object(map)
            }
            _ => serde_json::Value::Null,
        },
        _ => serde_json::Value::Null,
    }
}

/// Build a `JsonError` struct value from `serde_json::Error`.
pub(super) fn make_json_error(e: &serde_json::Error) -> Value {
    let mut fields = HashMap::new();
    fields.insert("line".to_string(), Value::Int(e.line() as i64));
    fields.insert("column".to_string(), Value::Int(e.column() as i64));
    fields.insert("message".to_string(), Value::String(e.to_string()));
    Value::Struct {
        name: "JsonError".to_string(),
        fields,
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn eval_http_post(url: &str, body: &str) -> Value {
    match ureq::post(url).send_string(body) {
        Ok(resp) => wrap_ok_response(resp),
        Err(e) => make_http_error(e.to_string()),
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn eval_http_post(_url: &str, _body: &str) -> Value {
    make_http_error("Http.post is not available in the browser playground".to_string())
}
