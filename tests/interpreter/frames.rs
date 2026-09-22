//! DataFrame, LazyFrame, Column, stats, CSV/Arrow IPC -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter frames::
//!
//! New fixtures about DataFrame, LazyFrame, Column, stats, CSV/Arrow IPC belong in this file.

use super::*;

#[test]
fn test_narrow_int_overflow_traps() {
    // Narrow integers are real fixed-width types (design.md § Integer
    // overflow): `u8 200 + u8 100 = 300` overflows the width and traps,
    // rather than silently widening to i64. Codegen mirrors this.
    let errors = runtime_errors("fn main() { let a: u8 = 200; let b: u8 = 100; println(a + b); }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("integer overflow")),
        "expected u8-overflow trap, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_narrow_int_in_range_does_not_trap() {
    // A narrow-int sum that fits the width is the value, no trap: `u8 97 + u8
    // 98 = 195` (≤ 255).
    assert_eq!(
        run("fn main() { let a: u8 = 97; let b: u8 = 98; println(a + b); }"),
        "195\n"
    );
}

// ── B-2026-07-01-3 narrow-element container arithmetic widths ──

#[test]
fn test_column_narrow_element_binop_overflow_traps() {
    // Pre-fix the interpreter evaluated `Column[i32] + 1` at i64 width and
    // silently produced 2147483648 where codegen trapped `integer
    // overflow` — `narrow_oob` now peels `Column[T]`/`Tensor[T, S]`
    // recorded types down to the element before the width check.
    let errors = runtime_errors(
        "fn main() {\n\
             let c: Column[i32] = Column.from_vec([2147483647]);\n\
             let d = c + 1;\n\
             match d[0] { Some(x) => { println(x); }, None => { println(0); } }\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("integer overflow")),
        "expected the i32 element-width overflow trap; got {:?}",
        errors
    );
}

// ── Arrow IPC interchange (phase-11 Arrow IPC slice 1, interp MVP) ──
// `Column.to_arrow_ipc()` serializes to the Apache Arrow IPC stream format
// (spec-compliant bytes via arrow-rs); `Column.from_arrow_ipc(bytes)` parses
// them back. Round-trip preserves length, the null pattern, and values.
#[test]
fn test_column_arrow_ipc_roundtrip_i64() {
    let out = run_no_errors(
        "fn main() {\n\
             let mut c: Column[i64] = Column.new();\n\
             c.push(10); c.push(20); c.push_null(); c.push(40);\n\
             let bytes = c.to_arrow_ipc();\n\
             let d: Column[i64] = Column.from_arrow_ipc(bytes);\n\
             println(d.len());\n\
             println(d.null_count());\n\
             println(d.sum());\n\
         }",
    );
    // len 4, one null, sum of valid (10+20+40) = 70.
    assert_eq!(out, "4\n1\n70\n");
}

#[test]
fn test_column_arrow_ipc_roundtrip_f64() {
    let out = run_no_errors(
        "fn main() {\n\
             let mut c: Column[f64] = Column.new();\n\
             c.push(1.5); c.push(2.5); c.push_null();\n\
             let bytes = c.to_arrow_ipc();\n\
             let d: Column[f64] = Column.from_arrow_ipc(bytes);\n\
             println(d.len());\n\
             println(d.null_count());\n\
             println(d.sum());\n\
         }",
    );
    assert_eq!(out, "3\n1\n4\n");
}

#[test]
fn test_column_arrow_ipc_roundtrip_from_vec_nonempty_bytes() {
    // A from_vec column serializes to a non-empty IPC stream and round-trips
    // its values (no nulls). The byte count is stable for a fixed schema/data,
    // but we only assert it's a plausible Arrow stream (> the 8-byte EOS
    // marker) to avoid pinning arrow-rs's exact framing.
    let out = run_no_errors(
        "fn main() {\n\
             let c: Column[i64] = Column.from_vec([1, 2, 3, 4, 5]);\n\
             let bytes = c.to_arrow_ipc();\n\
             let big = bytes.len() > 8;\n\
             println(big);\n\
             let d: Column[i64] = Column.from_arrow_ipc(bytes);\n\
             println(d.len());\n\
             println(d.sum());\n\
         }",
    );
    assert_eq!(out, "true\n5\n15\n");
}

// A `String` column serializes as Arrow `Utf8` and round-trips its values,
// null pattern (null POSITION, not just count), and length.
#[test]
fn test_column_arrow_ipc_roundtrip_string() {
    let out = run_no_errors(
        "fn main() {\n\
             let mut c: Column[String] = Column.new();\n\
             c.push(\"alpha\"); c.push_null(); c.push(\"gamma\");\n\
             let bytes = c.to_arrow_ipc();\n\
             let d: Column[String] = Column.from_arrow_ipc(bytes);\n\
             println(d.len());\n\
             println(d.null_count());\n\
             println(d.is_null(0));\n\
             println(d.is_null(1));\n\
             println(d.is_null(2));\n\
             let vals = d.iter_valid();\n\
             println(vals[0]);\n\
             println(vals[1]);\n\
         }",
    );
    // len 3, one null at index 1; the two valid cells are "alpha", "gamma".
    assert_eq!(out, "3\n1\nfalse\ntrue\nfalse\nalpha\ngamma\n");
}

// A `bool` column serializes as Arrow `Boolean` and round-trips its values,
// null pattern, and length.
#[test]
fn test_column_arrow_ipc_roundtrip_bool() {
    let out = run_no_errors(
        "fn main() {\n\
             let mut c: Column[bool] = Column.new();\n\
             c.push(true); c.push(false); c.push_null(); c.push(true);\n\
             let bytes = c.to_arrow_ipc();\n\
             let d: Column[bool] = Column.from_arrow_ipc(bytes);\n\
             println(d.len());\n\
             println(d.null_count());\n\
             let vals = d.iter_valid();\n\
             println(vals[0]);\n\
             println(vals[1]);\n\
             println(vals[2]);\n\
         }",
    );
    // len 4, one null; the three valid cells are true, false, true.
    assert_eq!(out, "4\n1\ntrue\nfalse\ntrue\n");
}

// A whole `DataFrame` serializes to a multi-field Arrow IPC RecordBatch (one
// field per column, name = column name) and round-trips column names,
// per-column element types, and values — the canonical Arrow tabular mapping.
#[test]
fn test_dataframe_arrow_ipc_roundtrip_types_and_names() {
    let out = run_no_errors(
        "fn main() {\n\
             let mut df = DataFrame.new();\n\
             df.insert(\"id\", Column.from_vec([10, 20, 30]));\n\
             df.insert(\"score\", Column.from_vec([1.5, 2.5, 3.5]));\n\
             let bytes = df.to_arrow_ipc();\n\
             let d = DataFrame.from_arrow_ipc(bytes);\n\
             println(d.width());\n\
             println(d.height());\n\
             let id: Column[i64] = d.column(\"id\");\n\
             println(id.sum());\n\
             let score: Column[f64] = d.column(\"score\");\n\
             println(score.sum());\n\
         }",
    );
    // 2 columns, 3 rows; i64 sum 60, f64 sum 7.5 — types and names preserved.
    assert_eq!(out, "2\n3\n60\n7.5\n");
}

// A DataFrame with a nullable String column round-trips the null pattern and
// string values through the batch.
#[test]
fn test_dataframe_arrow_ipc_roundtrip_string_nulls() {
    let out = run_no_errors(
        "fn main() {\n\
             let mut names: Column[String] = Column.new();\n\
             names.push(\"ann\"); names.push_null(); names.push(\"cat\");\n\
             let mut df = DataFrame.new();\n\
             df.insert(\"name\", names);\n\
             let bytes = df.to_arrow_ipc();\n\
             let d = DataFrame.from_arrow_ipc(bytes);\n\
             let n: Column[String] = d.column(\"name\");\n\
             println(n.len());\n\
             println(n.null_count());\n\
             println(n.is_null(1));\n\
             let vals = n.iter_valid();\n\
             println(vals[0]);\n\
             println(vals[1]);\n\
         }",
    );
    assert_eq!(out, "3\n1\ntrue\nann\ncat\n");
}

#[test]
fn test_column_narrow_element_binop_in_range_ok() {
    // Non-overflowing narrow element ops keep working after the peel.
    let out = run_no_errors(
        "fn main() {\n\
             let c: Column[i32] = Column.from_vec([100, 200]);\n\
             let d = c + 1;\n\
             match d[1] { Some(x) => { println(x); }, None => { println(0); } }\n\
         }",
    );
    assert_eq!(out, "201\n");
}

#[test]
fn test_interp_column_u64_sorted_and_argsort_unsigned() {
    // The receiver's element signedness is recovered from the non-aliased
    // close-paren leaf (`argsort`/`argmin`/`argmax` result-type is `Vec[i64]` /
    // `Option[i64]`, which would otherwise clobber the receiver span).
    let output = run(
        "fn main() { let c: Column[u64] = Column.from_vec([1u64 << 63, 5u64, 1u64 << 62]); \
         let s = c.sorted(); println(f\"{s[0]},{s[1]},{s[2]}\"); \
         let a = c.argsort(); println(f\"{a[0]},{a[1]},{a[2]}\"); \
         println(f\"{c.argmin()}\"); println(f\"{c.argmax()}\"); }",
    );
    // sorted ascending: 5, 2⁶², 2⁶³. argsort: idx 1 (5), 2 (2⁶²), 0 (2⁶³).
    // argmin = first-min index (5 @ 1), argmax = first-max index (2⁶³ @ 0).
    assert_eq!(
        output,
        "5,4611686018427387904,9223372036854775808\n1,2,0\nSome(1)\nSome(0)\n"
    );
}

// ── Stats namespace ───────────────────────────────────────────────

#[test]
fn test_stats_sum() {
    let output = run("fn main() { let xs = [1.0_f64, 2.0_f64, 3.0_f64]; println(Stats.sum(xs)); }");
    assert_eq!(output, "6\n");
}

#[test]
fn test_stats_prod() {
    let output =
        run("fn main() { let xs = [2.0_f64, 3.0_f64, 4.0_f64]; println(Stats.prod(xs)); }");
    assert_eq!(output, "24\n");
}

#[test]
fn test_stats_mean() {
    let output =
        run("fn main() { let xs = [1.0_f64, 2.0_f64, 3.0_f64]; println(Stats.mean(xs)); }");
    assert_eq!(output, "2\n");
}

#[test]
fn test_stats_on_slice() {
    // B-2026-07-18-12: `Stats.*` on a `Slice[f64]` value (`v.as_slice()`, the
    // declared `ref Slice[f64]` param's canonical form) read ZERO elements in
    // the interpreter — the arg extraction only handled `Value::Array`, so a
    // `Value::Slice` fell to the empty case (`sum` → -0, `mean` → panic) while
    // codegen read the slice correctly. Now the interpreter views the slice's
    // `storage[start..start+len]` range and matches `karac build`.
    let output = run("fn main() {\n\
             let v: Vec[f64] = vec![3.0, 1.0, 4.0];\n\
             let sl: Slice[f64] = v.as_slice();\n\
             println(Stats.sum(sl));\n\
             println(Stats.mean(sl));\n\
         }");
    assert_eq!(output, "8\n2.6666666666666665\n");
}

#[test]
fn test_stats_variance() {
    let output = run("fn main() { let xs = [2.0_f64, 4.0_f64, 4.0_f64, 4.0_f64, 5.0_f64, 5.0_f64, 7.0_f64, 9.0_f64]; println(Stats.variance(xs)); }");
    assert_eq!(output, "4\n");
}

#[test]
fn test_stats_stddev() {
    let output = run("fn main() { let xs = [2.0_f64, 4.0_f64, 4.0_f64, 4.0_f64, 5.0_f64, 5.0_f64, 7.0_f64, 9.0_f64]; println(Stats.stddev(xs)); }");
    assert_eq!(output, "2\n");
}

#[test]
fn test_stats_median_odd() {
    let output =
        run("fn main() { let xs = [3.0_f64, 1.0_f64, 2.0_f64]; println(Stats.median(xs)); }");
    assert_eq!(output, "2\n");
}

#[test]
fn test_stats_median_even() {
    let output = run(
        "fn main() { let xs = [1.0_f64, 2.0_f64, 3.0_f64, 4.0_f64]; println(Stats.median(xs)); }",
    );
    assert_eq!(output, "2.5\n");
}

#[test]
fn test_stats_min_nonempty() {
    let output = run("fn main() {\n\
         let xs = [3.0_f64, 1.0_f64, 2.0_f64];\n\
         match Stats.min(xs) {\n\
             Some(v) => println(v),\n\
             None => println(\"none\"),\n\
         }\n\
     }");
    assert_eq!(output, "1\n");
}

#[test]
fn test_stats_max_nonempty() {
    let output = run("fn main() {\n\
         let xs = [3.0_f64, 1.0_f64, 2.0_f64];\n\
         match Stats.max(xs) {\n\
             Some(v) => println(v),\n\
             None => println(\"none\"),\n\
         }\n\
     }");
    assert_eq!(output, "3\n");
}

#[test]
fn test_stats_min_empty() {
    let output = run("fn main() {\n\
         let xs: Vec[f64] = Vec[0.0_f64];\n\
         let ys = xs[1..];\n\
         match Stats.min(ys) {\n\
             Some(v) => println(v),\n\
             None => println(\"none\"),\n\
         }\n\
     }");
    // empty slice → None
    assert_eq!(output, "none\n");
}

#[test]
fn test_stats_percentile() {
    // NumPy convention: p in [0, 100], linear interpolation.
    // sorted [1, 1, 2, 3, 4, 5, 9]: p50 = median = 3; p0 = 1; p100 = 9;
    // p25 -> pos 0.25*6 = 1.5 -> 1 + 0.5*(2-1) = 1.5.
    let output = run("fn main() {\n\
         let xs = [3.0_f64, 1.0_f64, 4.0_f64, 1.0_f64, 5.0_f64, 9.0_f64, 2.0_f64];\n\
         println(Stats.percentile(xs, 50.0_f64));\n\
         println(Stats.percentile(xs, 0.0_f64));\n\
         println(Stats.percentile(xs, 100.0_f64));\n\
         println(Stats.percentile(xs, 25.0_f64));\n\
     }");
    assert_eq!(output, "3\n1\n9\n1.5\n");
}

#[test]
fn test_stats_argmin_argmax() {
    // First-occurrence index of min / max; xs = [3, 1, 4, 1, 5, 9, 2].
    let output = run("fn main() {\n\
         let xs = [3.0_f64, 1.0_f64, 4.0_f64, 1.0_f64, 5.0_f64, 9.0_f64, 2.0_f64];\n\
         match Stats.argmin(xs) { Some(i) => println(i), None => println(-1), }\n\
         match Stats.argmax(xs) { Some(i) => println(i), None => println(-1), }\n\
     }");
    assert_eq!(output, "1\n5\n");
}

#[test]
fn test_stats_argmin_empty_is_none() {
    let output = run("fn main() {\n\
         let xs: Vec[f64] = Vec[0.0_f64];\n\
         let ys = xs[1..];\n\
         match Stats.argmin(ys) { Some(i) => println(i), None => println(-1), }\n\
     }");
    assert_eq!(output, "-1\n");
}

#[test]
fn test_stats_sort() {
    // Ascending copy; the source slice is unchanged.
    let output = run("fn main() {\n\
         let xs = [3.0_f64, 1.0_f64, 2.0_f64];\n\
         let s: Vec[f64] = Stats.sort(xs);\n\
         println(s[0]);\n\
         println(s[2]);\n\
     }");
    assert_eq!(output, "1\n3\n");
}

#[test]
fn test_stats_argsort() {
    // Indices that sort xs ascending; xs = [3, 1, 2] -> [1, 2, 0].
    let output = run("fn main() {\n\
         let xs = [3.0_f64, 1.0_f64, 2.0_f64];\n\
         let a: Vec[i64] = Stats.argsort(xs);\n\
         println(a[0]);\n\
         println(a[1]);\n\
         println(a[2]);\n\
     }");
    assert_eq!(output, "1\n2\n0\n");
}

// ── Stats over i64 elements (S5 — the non-f64 element axis) ──────

#[test]
fn test_stats_i64_element_typed_ops() {
    // sum/prod fold at i64; min/max keep the element type; sort/argsort
    // and argmin/argmax compare at exact i64.
    let output = run("fn main() {\n\
         let xs: Vec[i64] = vec![3, 1, 2];\n\
         println(Stats.sum(xs));\n\
         println(Stats.prod(xs));\n\
         match Stats.min(xs) { Some(v) => println(v), None => println(-1) }\n\
         match Stats.max(xs) { Some(v) => println(v), None => println(-1) }\n\
         let s: Vec[i64] = Stats.sort(xs);\n\
         println(s[0]);\n\
         let a: Vec[i64] = Stats.argsort(xs);\n\
         println(a[0]);\n\
         match Stats.argmin(xs) { Some(i) => println(i), None => println(-1) }\n\
     }");
    assert_eq!(output, "6\n6\n1\n3\n1\n1\n1\n");
}

#[test]
fn test_stats_i64_float_statistics_promote() {
    let output = run("fn main() {\n\
         let xs: Vec[i64] = vec![4, 1, 3, 2];\n\
         println(Stats.mean(xs));\n\
         println(Stats.median(xs));\n\
         println(Stats.percentile(xs, 50));\n\
         let v: Vec[i64] = vec![2, 4, 4, 4, 5, 5, 7, 9];\n\
         println(Stats.variance(v));\n\
         println(Stats.stddev(v));\n\
     }");
    assert_eq!(output, "2.5\n2.5\n2.5\n4\n2\n");
}

#[test]
fn test_stats_i64_exact_above_2_pow_53() {
    // 2^53 and 2^53 + 1 are the same f64; the int paths must stay exact.
    let output = run("fn main() {\n\
         let a = 9007199254740993;\n\
         let b = 9007199254740992;\n\
         let xs: Vec[i64] = vec![a, b];\n\
         match Stats.max(xs) { Some(v) => println(v), None => println(-1) }\n\
         match Stats.argmax(xs) { Some(i) => println(i), None => println(-1) }\n\
         let s: Vec[i64] = Stats.sort(xs);\n\
         println(s[1]);\n\
     }");
    assert_eq!(output, "9007199254740993\n0\n9007199254740993\n");
}

#[test]
fn test_stats_i64_empty_integer_identities() {
    // The STATIC element type drives the empty-input identities: an empty
    // Vec[i64] sums to integer 0 (not the float -0.0) and prods to 1;
    // min/max are None.
    let output = run("fn main() {\n\
         let e: Vec[i64] = vec![];\n\
         println(Stats.sum(e));\n\
         println(Stats.prod(e));\n\
         match Stats.min(e) { Some(v) => println(v), None => println(-1) }\n\
     }");
    assert_eq!(output, "0\n1\n-1\n");
}

#[test]
fn test_stats_i64_sum_overflow_traps() {
    // B-2026-08-19-25 — this test used to assert the trap arrived as a Rust
    // `panic!`, caught with `catch_unwind`. That was the defect, not the
    // contract: `karac run` exited 101 with a backtrace naming
    // `src/interpreter/helpers.rs` where `karac build` printed a clean
    // `integer overflow` at exit 1. The trap still happens — it now travels the
    // runtime-error channel, so `run` returns normally and the error is
    // recorded with a span.
    let errors = runtime_errors(
        "fn main() {\n\
             let big = 9223372036854775807;\n\
             let xs: Vec[i64] = vec![big, 1];\n\
             println(Stats.sum(xs));\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("integer overflow")),
        "expected the checked-sum overflow trap as a runtime error, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_mut_ref_scalar_narrow_int_assign_through() {
    // `mut ref i32` with an unsuffixed literal — the pointee width is preserved.
    let output = run("fn inc(x: mut ref i32) { x = x + 1; }\n\
         fn main() { let mut n: i32 = 41; inc(mut n); println(n); }");
    assert_eq!(output, "42\n");
}

// ── Arrow IPC constructors verify the binding annotation (B-2026-07-28-10) ────
// `Column.from_arrow_ipc` / `Tensor.from_arrow_ipc` take an opaque byte stream,
// so nothing in the argument says what the result's element type or shape is.
// Codegen recovers both from the `let` annotation and has the runtime reject a
// stream that does not convert; the interpreter built straight from whatever the
// stream decoded, so an ill-typed program printed a plausible answer under
// `karac run` while `karac build` trapped. `pending_let_ty` threads the
// annotation into the RHS so both backends now reject the same programs.

#[test]
fn test_column_from_arrow_ipc_rejects_element_type_mismatch() {
    // A Utf8 stream bound at `Column[i64]`. Codegen traps; the interpreter used
    // to hand back a Column of Strings and print 2.
    let errors = runtime_errors(
        "fn main() {\n\
             let s: Column[String] = Column.from_vec([\"a\", \"b\"]);\n\
             let bad: Column[i64] = Column.from_arrow_ipc(s.to_arrow_ipc());\n\
             println(bad.len());\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("do not convert to the declared element type")),
        "expected an element-type rejection, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_column_from_arrow_ipc_accepts_matching_element_types() {
    // The guard must not over-fire: a well-typed round-trip is unchanged on
    // every checkable element class (numeric, String, float).
    assert_eq!(
        run("fn main() {\n\
                 let a: Column[i64] = Column.from_vec([1, 2, 3]);\n\
                 let ra: Column[i64] = Column.from_arrow_ipc(a.to_arrow_ipc());\n\
                 println(ra.len());\n\
                 let b: Column[String] = Column.from_vec([\"a\", \"b\"]);\n\
                 let rb: Column[String] = Column.from_arrow_ipc(b.to_arrow_ipc());\n\
                 println(rb.len());\n\
                 let c: Column[f64] = Column.from_vec([1.5, 2.5]);\n\
                 let rc: Column[f64] = Column.from_arrow_ipc(c.to_arrow_ipc());\n\
                 println(rc.len());\n\
             }"),
        "3\n2\n2\n"
    );
}

#[test]
fn test_interp_int_widening_narrowing_casts() {
    let output = run(r#"
fn main() {
    let a: i32 = 1000i32;
    let b: i64 = a as i64;
    println(b);
    let c: i8 = a as i8;
    let d: i32 = c as i32;
    println(d);
}
"#);
    // 1000 widens to i64 unchanged; truncating to i8 keeps the low 8 bits
    // (1000 & 0xff = 0xe8 = -24 as signed i8), then widens back as -24.
    assert_eq!(output, "1000\n-24\n");
}

// B-2026-08-06-7, the negation leg: `-iN::MIN` traps at the DECLARED width.
//
// The Neg arm used `checked_neg` on the i64 carrier, which only catches
// `-i64::MIN` — so `-(-2147483648i32)` produced 2147483648, a value the
// declared `i32` cannot hold, while the i64 twin trapped correctly. Every
// narrow width, plus the i64 control that was already right, because the check
// keys off the type recorded at the expression's span.
#[test]
fn test_narrow_negation_of_min_is_runtime_error() {
    for (src, what) in [
        ("let a: i8 = -128i8; println(-a);", "i8"),
        ("let a: i16 = -32768i16; println(-a);", "i16"),
        ("let a: i32 = -2147483648i32; println(-a);", "i32"),
        // i64::MIN is built by subtraction rather than written as a literal:
        // a negative literal parses as `Neg(Integer(n))`, so the POSITIVE half
        // must fit i64 — and i64::MIN's does not, making `-9223372036854775808i64`
        // a parse error (`Invalid integer literal`). Filed as B-2026-08-06-13.
        (
            "let a: i64 = -9223372036854775807i64 - 1i64; println(-a);",
            "i64 control",
        ),
    ] {
        let errors = runtime_errors(&format!("fn main() {{\n    {src}\n}}\n"));
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("integer overflow")),
            "{what}: negating the minimum must raise `integer overflow`, got: {errors:?}"
        );
    }
}

// …and the guard must not over-trap: `-(iN::MIN + 1)` is representable at
// every width and must stay legal.
#[test]
fn test_narrow_negation_inside_range_is_legal() {
    let out = run_no_errors(
        "fn main() {\n\
         \x20   let a: i8 = -127i8; println(-a);\n\
         \x20   let b: i16 = -32767i16; println(-b);\n\
         \x20   let c: i32 = -2147483647i32; println(-c);\n\
         \x20   let d: i32 = 5i32; println(-d);\n\
         }\n",
    );
    assert_eq!(out.trim(), "127\n32767\n2147483647\n-5");
}

#[test]
fn test_vector_shuffle_narrowing() {
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 4](1, 2, 3, 4); let r = a.shuffle([3, 0]); \
         println(r[0]); println(r[1]); }",
    );
    assert_eq!(out, "4\n1\n");
}

// ── Column[T] nullable column — interpreter MVP (phase-11 Arrow Q5) ──

#[test]
fn test_column_new_push_and_null_accessors() {
    // new() + push/push_null, then the validity accessors. push_null keeps
    // len growing (Arrow data/validity stay the same length).
    let out = run_no_errors(
        "fn main() {\n\
             let mut c: Column[i64] = Column.new();\n\
             c.push(10i64);\n\
             c.push_null();\n\
             c.push(30i64);\n\
             println(c.len());\n\
             println(c.null_count());\n\
             println(c.valid_count());\n\
             println(c.is_null(0));\n\
             println(c.is_null(1));\n\
         }",
    );
    assert_eq!(out, "3\n1\n2\nfalse\ntrue\n");
}

#[test]
fn test_column_index_returns_option() {
    // `c[i] -> Option[T]`: Some for a valid slot, None for a null.
    let out = run_no_errors(
        "fn main() {\n\
             let mut c: Column[i64] = Column.new();\n\
             c.push(10i64);\n\
             c.push_null();\n\
             match c[0] { Some(v) => { println(v); } None => { println(-1i64); } }\n\
             match c[1] { Some(v) => { println(v); } None => { println(-1i64); } }\n\
         }",
    );
    assert_eq!(out, "10\n-1\n");
}

#[test]
fn test_column_from_vec_all_valid() {
    // from_vec — every slot valid (no nulls).
    let out = run_no_errors(
        "fn main() {\n\
             let c: Column[i64] = Column.from_vec([1i64, 2i64, 3i64]);\n\
             println(c.len());\n\
             println(c.null_count());\n\
             match c[2] { Some(v) => { println(v); } None => { println(-1i64); } }\n\
         }",
    );
    assert_eq!(out, "3\n0\n3\n");
}

#[test]
fn test_column_string_element_and_null() {
    // Works for a heap element type (String); the null slot reads None.
    let out = run_no_errors(
        "fn main() {\n\
             let mut c: Column[String] = Column.new();\n\
             c.push(\"hi\");\n\
             c.push_null();\n\
             match c[0] { Some(v) => { println(v); } None => { println(\"<none>\"); } }\n\
             match c[1] { Some(v) => { println(v); } None => { println(\"<none>\"); } }\n\
         }",
    );
    assert_eq!(out, "hi\n<none>\n");
}

#[test]
fn test_column_from_vec_temp_string_move_runs() {
    // B-2026-07-06-1 run==build parity: `Column.from_vec(<temporary
    // Vec[String]>)` — an inline array literal and a function-call result — is
    // value-semantics-clean under `karac run`. The codegen fix moves the temp's
    // String heaps into the column (the build surface was the loud-fail); this
    // pins the RUN surface to the same output (build covered by
    // `tests/codegen.rs::test_e2e_column_from_vec_temp_string_move` and the
    // `asan_column_from_vec_temp_string_move_no_leak` memory test).
    let out = run_no_errors(
        "fn mk() -> Vec[String] {\n\
             let mut v: Vec[String] = Vec.new();\n\
             v.push(\"delta\".to_string());\n\
             v.push(\"echo\".to_string());\n\
             v\n\
         }\n\
         fn main() {\n\
             let c: Column[String] = Column.from_vec([\"alpha\".to_string(), \"beta\".to_string(), \"gamma\".to_string()]);\n\
             let d: Column[String] = Column.from_vec(mk());\n\
             println(f\"{c.len()} {d.len()}\");\n\
             println(f\"{c[0].unwrap()} {c[2].unwrap()}\");\n\
             println(f\"{d[1].unwrap()}\");\n\
         }",
    );
    assert_eq!(out, "3 2\nalpha gamma\necho\n");
}

#[test]
fn test_column_index_out_of_bounds_traps() {
    // Out-of-range index is a runtime error, NOT None.
    let errors = runtime_errors(
        "fn main() {\n\
             let c: Column[i64] = Column.from_vec([1i64, 2i64]);\n\
             let _ = c[5];\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("column index 5 out of bounds")),
        "{errors:?}",
    );
}

// ── Column[T] slice 2 — iterators + null-handling transforms ──

#[test]
fn test_column_iter_yields_option_per_slot() {
    // `iter()` -> Vec[Option[T]]: Some for valid, None for null, in order.
    let out = run_no_errors(
        "fn main() {\n\
             let mut c: Column[i64] = Column.new();\n\
             c.push(10i64); c.push_null(); c.push(30i64);\n\
             for x in c.iter() {\n\
                 match x { Some(v) => { println(v); } None => { println(-1i64); } }\n\
             }\n\
         }",
    );
    assert_eq!(out, "10\n-1\n30\n");
}

#[test]
fn test_column_iter_valid_skips_nulls() {
    // `iter_valid()` -> Vec[T]: the valid slots only, unwrapped.
    let out = run_no_errors(
        "fn main() {\n\
             let mut c: Column[i64] = Column.new();\n\
             c.push(10i64); c.push_null(); c.push(30i64);\n\
             for v in c.iter_valid() { println(v); }\n\
         }",
    );
    assert_eq!(out, "10\n30\n");
}

#[test]
fn test_column_fillna_replaces_nulls() {
    // `fillna(v)` -> all-valid Column[T] (receiver unchanged).
    let out = run_no_errors(
        "fn main() {\n\
             let mut c: Column[i64] = Column.new();\n\
             c.push(10i64); c.push_null(); c.push(30i64);\n\
             let f = c.fillna(99i64);\n\
             println(f.null_count());\n\
             for v in f.iter_valid() { println(v); }\n\
             println(c.null_count());\n\
         }",
    );
    // f has no nulls (10,99,30); c is unchanged (still 1 null).
    assert_eq!(out, "0\n10\n99\n30\n1\n");
}

#[test]
fn test_column_fillna_treat_nan_as_null() {
    // `treat_nan_as_null: true` normalizes a float column's bitmap-valid NaN
    // slots into fills (design.md § Data types); bare `fillna` leaves them.
    // The column is [1.5, null, NaN, 4.0]: bare fillna touches only the null
    // slot, the flagged form additionally fills the NaN slot.
    let out = run_no_errors(
        "fn main() {\n\
             let z: f64 = 0.0;\n\
             let nan: f64 = z / z;\n\
             let mut c: Column[f64] = Column.new();\n\
             c.push(1.5); c.push_null(); c.push(nan); c.push(4.0);\n\
             let a = c.fillna(0.0);\n\
             println(a.null_count());\n\
             match a[1] { Some(v) => println(v), None => println(-1.0) }\n\
             match a[2] { Some(v) => println(v), None => println(-1.0) }\n\
             let b = c.fillna(0.0, treat_nan_as_null: true);\n\
             match b[1] { Some(v) => println(v), None => println(-1.0) }\n\
             match b[2] { Some(v) => println(v), None => println(-1.0) }\n\
             let d = c.fillna(7.0, true);\n\
             match d[2] { Some(v) => println(v), None => println(-1.0) }\n\
             println(c.null_count());\n\
         }",
    );
    // a: null→0, NaN kept (still NaN). b: both filled with 0. d: NaN→7 via
    // the positional flag. Receiver c keeps its single bitmap-null.
    assert_eq!(out, "0\n0\nNaN\n0\n0\n7\n1\n");
}

#[test]
fn test_column_dropna_removes_nulls() {
    // `dropna()` -> all-valid Column[T] of the valid values (receiver kept).
    let out = run_no_errors(
        "fn main() {\n\
             let mut c: Column[i64] = Column.new();\n\
             c.push(10i64); c.push_null(); c.push(30i64);\n\
             let d = c.dropna();\n\
             println(d.len());\n\
             println(d.null_count());\n\
             for v in d.iter_valid() { println(v); }\n\
         }",
    );
    assert_eq!(out, "2\n0\n10\n30\n");
}

#[test]
fn test_column_from_iter_nullable() {
    // `from_iter_nullable(Vec[Option[T]])` — Some -> valid, None -> null.
    let out = run_no_errors(
        "fn main() {\n\
             let e: Column[i64] = Column.from_iter_nullable([Some(1i64), None, Some(3i64)]);\n\
             println(e.len());\n\
             println(e.null_count());\n\
             println(e.is_null(1));\n\
             match e[0] { Some(v) => { println(v); } None => { println(-1i64); } }\n\
             match e[1] { Some(v) => { println(v); } None => { println(-1i64); } }\n\
         }",
    );
    assert_eq!(out, "3\n1\ntrue\n1\n-1\n");
}

// ── Column[T] slice 3 — three-valued-logic arithmetic / comparison ──

#[test]
fn test_column_arith_null_propagation() {
    // a + b: a valid+b valid -> sum; either null -> null result slot.
    let out = run_no_errors(
        "fn main() {\n\
             let a: Column[i64] = Column.from_iter_nullable([Some(10i64), None, Some(30i64)]);\n\
             let b: Column[i64] = Column.from_iter_nullable([Some(1i64), Some(2i64), None]);\n\
             let r = a + b;\n\
             for x in r.iter() { match x { Some(v) => { println(v); } None => { println(-1i64); } } }\n\
         }",
    );
    assert_eq!(out, "11\n-1\n-1\n");
}

#[test]
fn test_column_scalar_broadcast_and_neg() {
    // Scalar broadcast keeps nulls null; unary '-' negates valid slots only.
    let out = run_no_errors(
        "fn main() {\n\
             let a: Column[i64] = Column.from_iter_nullable([Some(10i64), None, Some(30i64)]);\n\
             let m = a * 2i64;\n\
             for x in m.iter() { match x { Some(v) => { println(v); } None => { println(-1i64); } } }\n\
             let n = -a;\n\
             for x in n.iter() { match x { Some(v) => { println(v); } None => { println(-1i64); } } }\n\
         }",
    );
    assert_eq!(out, "20\n-1\n60\n-10\n-1\n-30\n");
}

#[test]
fn test_column_comparison_is_three_valued() {
    // a == b yields a Column[bool] with nulls where either side is null —
    // NOT false. The headline 3VL rule: null == null = null.
    let out = run_no_errors(
        "fn main() {\n\
             let a: Column[i64] = Column.from_iter_nullable([Some(10i64), None, Some(30i64)]);\n\
             let b: Column[i64] = Column.from_iter_nullable([Some(1i64), Some(2i64), None]);\n\
             let cmp = a == b;\n\
             for x in cmp.iter() { match x { Some(v) => { println(v); } None => { println(\"null\"); } } }\n\
             let n: Column[i64] = Column.from_iter_nullable([None, Some(5i64)]);\n\
             let eq = n == n;\n\
             for x in eq.iter() { match x { Some(v) => { println(v); } None => { println(\"null\"); } } }\n\
         }",
    );
    // a==b: 10==1 -> false; null; null. n==n: null==null -> null; 5==5 -> true.
    assert_eq!(out, "false\nnull\nnull\nnull\ntrue\n");
}

#[test]
fn test_column_div_by_zero_in_valid_slot_traps() {
    let errors = runtime_errors(
        "fn main() {\n\
             let a: Column[i64] = Column.from_vec([10i64, 20i64]);\n\
             let b: Column[i64] = Column.from_vec([2i64, 0i64]);\n\
             let _ = a / b;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("division by zero")),
        "{errors:?}",
    );
}

#[test]
fn test_column_length_mismatch_traps() {
    let errors = runtime_errors(
        "fn main() {\n\
             let a: Column[i64] = Column.from_vec([1i64, 2i64, 3i64]);\n\
             let b: Column[i64] = Column.from_vec([1i64, 2i64]);\n\
             let _ = a + b;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("column length mismatch")),
        "{errors:?}",
    );
}

// ── Column[T] stats — scalar statistical reductions (phase-11) ───────

#[test]
fn test_column_sum_mean_min_max_skip_nulls() {
    // Reductions operate on the valid slots only (nulls skipped). sum/min/max
    // preserve the element type (i64); mean is always f64.
    let out = run_no_errors(
        "fn main() {\n\
             let c: Column[i64] = Column.from_iter_nullable([Some(2i64), None, Some(4i64), Some(6i64)]);\n\
             println(c.sum());\n\
             println(c.min());\n\
             println(c.max());\n\
             println(c.mean());\n\
         }",
    );
    // valid = [2, 4, 6]: sum 12, min 2, max 6, mean 4.0
    assert_eq!(out, "12\n2\n6\n4\n");
}

#[test]
fn test_column_var_std_sample() {
    // Sample (n-1) variance / std over the valid f64 slots.
    let out = run_no_errors(
        "fn main() {\n\
             let c: Column[f64] = Column.from_vec([2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]);\n\
             println(c.var());\n\
             println(c.std());\n\
         }",
    );
    // mean 5.0, ss = 32, sample var = 32/7 = 4.571428..., std = sqrt
    assert_eq!(out, "4.571428571428571\n2.138089935299395\n");
}

#[test]
fn test_column_median_and_quantile() {
    // median (even count -> mean of two middle) and quantile via linear interp.
    let out = run_no_errors(
        "fn main() {\n\
             let c: Column[f64] = Column.from_vec([1.0, 2.0, 3.0, 4.0]);\n\
             println(c.median());\n\
             println(c.quantile(0.0));\n\
             println(c.quantile(0.5));\n\
             println(c.quantile(1.0));\n\
         }",
    );
    // sorted [1,2,3,4]: median 2.5; q0=1, q0.5=2.5, q1=4
    assert_eq!(out, "2.5\n1\n2.5\n4\n");
}

#[test]
fn test_column_corr_pearson() {
    // Pearson correlation: perfectly correlated -> 1.0.
    let out = run_no_errors(
        "fn main() {\n\
             let a: Column[f64] = Column.from_vec([1.0, 2.0, 3.0, 4.0]);\n\
             let b: Column[f64] = Column.from_vec([2.0, 4.0, 6.0, 8.0]);\n\
             println(a.corr(b));\n\
         }",
    );
    assert_eq!(out, "1\n");
}

#[test]
fn test_column_corr_uses_pairwise_valid() {
    // Only slots where BOTH columns are valid contribute to corr.
    let out = run_no_errors(
        "fn main() {\n\
             let a: Column[f64] = Column.from_iter_nullable([Some(1.0), Some(2.0), None, Some(4.0)]);\n\
             let b: Column[f64] = Column.from_iter_nullable([Some(2.0), Some(4.0), Some(99.0), Some(8.0)]);\n\
             println(a.corr(b));\n\
         }",
    );
    // pairs (1,2)(2,4)(4,8) -> perfectly correlated -> 1.0
    assert_eq!(out, "1\n");
}

#[test]
fn test_column_reduce_empty_traps() {
    // A column with no valid values can't be reduced (no identity).
    let errors = runtime_errors(
        "fn main() {\n\
             let mut c: Column[i64] = Column.new();\n\
             c.push_null();\n\
             println(c.sum());\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e.message.contains("no valid values")),
        "{errors:?}",
    );
}

#[test]
fn test_column_var_requires_two_values_traps() {
    // Sample variance is undefined for fewer than 2 valid values.
    let errors = runtime_errors(
        "fn main() {\n\
             let c: Column[f64] = Column.from_vec([3.0]);\n\
             println(c.var());\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("at least 2 valid values")),
        "{errors:?}",
    );
}

#[test]
fn test_column_quantile_out_of_range_traps() {
    let errors = runtime_errors(
        "fn main() {\n\
             let c: Column[f64] = Column.from_vec([1.0, 2.0, 3.0]);\n\
             println(c.quantile(1.5));\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("must be in [0, 1]")),
        "{errors:?}",
    );
}

// ── DataFrame interpreter MVP (phase-11 Arrow Q6) ────────────────────

#[test]
fn test_dataframe_build_lookup_and_accessors() {
    // Heterogeneous build via new() + insert; lookup round-trips values,
    // width/height/column_names report the schema-lite shape.
    let out = run_no_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"age\", Column.from_vec([30i64, 25i64, 40i64]));\n\
             df.insert(\"name\", Column.from_vec([\"a\", \"b\", \"c\"]));\n\
             println(df.width());\n\
             println(df.height());\n\
             let ages: Column[i64] = df.column(\"age\");\n\
             println(ages.len());\n\
             match ages[0] { Some(v) => { println(v); } None => { println(-1i64); } }\n\
             for n in df.column_names() { println(n); }\n\
         }",
    );
    assert_eq!(out, "2\n3\n3\n30\nage\nname\n");
}

#[test]
fn test_dataframe_has_column() {
    let out = run_no_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"x\", Column.from_vec([1i64]));\n\
             println(df.has_column(\"x\"));\n\
             println(df.has_column(\"y\"));\n\
         }",
    );
    assert_eq!(out, "true\nfalse\n");
}

#[test]
fn test_dataframe_insert_replace_keeps_height() {
    // Re-inserting an existing name replaces the column; height unchanged,
    // width unchanged, the new values win.
    let out = run_no_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64, 2i64]));\n\
             df.insert(\"a\", Column.from_vec([9i64, 8i64]));\n\
             println(df.width());\n\
             println(df.height());\n\
             let a: Column[i64] = df.column(\"a\");\n\
             match a[0] { Some(v) => { println(v); } None => { println(-1i64); } }\n\
         }",
    );
    assert_eq!(out, "1\n2\n9\n");
}

#[test]
fn test_dataframe_insert_length_mismatch_traps() {
    // The Arrow equal-length invariant: a column whose length differs from
    // the table's row count is a runtime error.
    let errors = runtime_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64, 2i64, 3i64]));\n\
             df.insert(\"b\", Column.from_vec([1i64, 2i64]));\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("has length 2 but the table has 3 row(s)")),
        "{errors:?}",
    );
}

#[test]
fn test_dataframe_column_missing_traps() {
    // Looking up an absent column is a runtime error.
    let errors = runtime_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64]));\n\
             let _c: Column[i64] = df.column(\"nope\");\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("no column named 'nope'")),
        "{errors:?}",
    );
}

#[test]
fn test_dataframe_write_csv_serializes_header_rows_quoting_and_nulls() {
    // `df.write_csv(path)` (phase-11 CSV leg, slice 1): header row of names
    // in schema order, one line per row, cells formatted like `println`
    // (i64 exact, f64 shortest-roundtrip — `88.0` renders `88`), a NULL slot
    // as an empty cell, and RFC-4180 quoting only where needed (comma /
    // double-quote / newline; embedded quotes doubled). Read the file back
    // through `fs.read_to_string` and print it so the oracle is the exact
    // byte content.
    let tmp = std::env::temp_dir().join("kara_interp_df_write_csv.csv");
    let _ = std::fs::remove_file(&tmp);
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        "fn main() with writes(FileSystem) reads(FileSystem) {{\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"age\", Column.from_vec([30i64, 25i64, 41i64]));\n\
             df.insert(\"score\", Column.from_vec([91.5, 78.25, 88.0]));\n\
             df.insert(\"name\", Column.from_vec([\"ada\", \"bob, jr.\", \"eve \\\"the\\\" grey\"]));\n\
             let nn: Vec[Option[i64]] = vec![Some(1i64), None, Some(3i64)];\n\
             df.insert(\"nullable\", Column.from_iter_nullable(nn));\n\
             match df.write_csv(\"{path}\") {{\n\
                 Ok(_) => match fs.read_to_string(\"{path}\") {{\n\
                     Ok(s) => print(s),\n\
                     Err(_) => println(\"read-err\"),\n\
                 }},\n\
                 Err(_) => println(\"write-err\"),\n\
             }}\n\
         }}"
    );
    let out = run_no_errors(&src);
    let _ = std::fs::remove_file(&tmp);
    assert_eq!(
        out,
        "age,score,name,nullable\n\
         30,91.5,ada,1\n\
         25,78.25,\"bob, jr.\",\n\
         41,88,\"eve \"\"the\"\" grey\",3\n",
        "write_csv must serialize header + rows with println formatting, \
         RFC-4180 quoting, and empty cells for nulls",
    );
}

#[test]
fn test_dataframe_write_csv_unwritable_path_is_err() {
    // A write to an unwritable path surfaces as `Err(IoError...)`, not a
    // trap — the same Result posture as `fs.write`.
    let out = run_no_errors(
        "fn main() with writes(FileSystem) {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64]));\n\
             match df.write_csv(\"/nonexistent-dir-kara/x.csv\") {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(_) => println(\"io-err\"),\n\
             }\n\
         }",
    );
    assert_eq!(out, "io-err\n");
}

#[test]
fn test_dataframe_column_is_value_copy_not_view() {
    // Value semantics: a looked-up column is independent of the frame.
    // Mutating the copy leaves the frame's column untouched — pins the
    // run/build contract the codegen lowering must match (copy-out).
    let out = run_no_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64, 2i64]));\n\
             let mut c: Column[i64] = df.column(\"a\");\n\
             c.push(3i64);\n\
             println(c.len());\n\
             println(df.height());\n\
             let d: Column[i64] = df.column(\"a\");\n\
             println(d.len());\n\
         }",
    );
    // copy grows to 3; frame still 2 rows; a fresh lookup is still 2.
    assert_eq!(out, "3\n2\n2\n");
}

#[test]
fn test_dataframe_describe_numeric_columns() {
    // describe() — per-numeric-column stats; the String column is skipped;
    // a leading `statistic` label column; always 8 stat rows.
    let out = run_no_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"age\", Column.from_vec([20i64, 30i64, 40i64, 50i64]));\n\
             df.insert(\"name\", Column.from_vec([\"a\", \"b\", \"c\", \"d\"]));\n\
             let d: DataFrame = df.describe();\n\
             println(d.width());\n\
             println(d.height());\n\
             for n in d.column_names() { println(n); }\n\
             let a: Column[f64] = d.column(\"age\");\n\
             for v in a.iter_valid() { println(v); }\n\
         }",
    );
    // width 2 (statistic + age; `name` skipped), height 8; age stats:
    // count 4, mean 35, std (sample) 12.909…, min 20, 27.5/35/42.5, max 50.
    assert_eq!(
        out,
        "2\n8\nstatistic\nage\n4\n35\n12.909944487358056\n20\n27.5\n35\n42.5\n50\n"
    );
}

#[test]
fn test_dataframe_describe_skips_nulls_and_labels() {
    // Stats are over the valid (non-null) slots only; the `statistic` column
    // carries the canonical labels in order.
    let out = run_no_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             let score: Column[f64] = Column.from_iter_nullable([Some(1.0), None, Some(3.0), Some(5.0)]);\n\
             df.insert(\"score\", score);\n\
             let d: DataFrame = df.describe();\n\
             let lab: Column[String] = d.column(\"statistic\");\n\
             for s in lab.iter_valid() { println(s); }\n\
             let c: Column[f64] = d.column(\"score\");\n\
             for v in c.iter_valid() { println(v); }\n\
         }",
    );
    // valid = [1, 3, 5]: count 3, mean 3, std 2, min 1, 2/3/4, max 5.
    assert_eq!(
        out,
        "count\nmean\nstd\nmin\n25%\n50%\n75%\nmax\n3\n3\n2\n1\n2\n3\n4\n5\n"
    );
}

#[test]
fn test_dataframe_describe_single_value_std_is_nan() {
    // Sample std is undefined for a single value -> NaN (describe never traps).
    let out = run_no_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"x\", Column.from_vec([7.0]));\n\
             let d: DataFrame = df.describe();\n\
             let c: Column[f64] = d.column(\"x\");\n\
             match c[2] { Some(x) => println(x), None => println(0.0) }\n\
         }",
    );
    // row 2 is `std`.
    assert_eq!(out, "NaN\n");
}

#[test]
fn test_dataframe_describe_no_numeric_columns() {
    // Only non-numeric columns -> just the `statistic` label column.
    let out = run_no_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"name\", Column.from_vec([\"a\", \"b\"]));\n\
             let d: DataFrame = df.describe();\n\
             println(d.width());\n\
             println(d.height());\n\
         }",
    );
    assert_eq!(out, "1\n8\n");
}

#[test]
fn column_prod_reduction() {
    // `Column.prod()` — product of the valid slots (parity with `Tensor.prod`;
    // Column had `sum` but no `prod`). Folds `*` over the valid slots, i64 + f64.
    let out = run_no_errors(
        r#"
fn main() {
    let ci: Column[i64] = Column.from_vec([2, 3, 4]);
    let cf: Column[f64] = Column.from_vec([1.5, 2.0, 4.0]);
    println(f"{ci.prod()}");
    println(f"{cf.prod()}");
}
"#,
    );
    assert_eq!(out, "24\n12\n");
}

#[test]
fn column_fold_reduction() {
    // `Column.fold[A](init, |acc, x| ...)` — the general left-fold primitive,
    // threading `init` through the closure over the valid slots (nulls skipped,
    // in order). The interpreter handles ANY `A`/`T` (unlike the POD-only
    // native first cut): here a numeric fold, a String accumulator (run-only),
    // a nulls-skipping fold, and an empty column (returns `init`, no trap).
    let out = run_no_errors(
        r#"
fn main() {
    let c: Column[i64] = Column.from_vec([1, 2, 3, 4, 5]);
    println(f"{c.fold(0, |a, x| a + x)}");
    println(f"{c.fold(1, |a, x| a * x)}");
    println(f"{c.fold(0, |a, x| if x > 2 { a + 1 } else { a })}");

    // String accumulator — the interpreter is not restricted to POD `A`.
    let words: Column[i64] = Column.from_vec([1, 2, 3]);
    let joined = words.fold("", |acc, x| acc + "!");
    println(f"{joined}");

    // Nulls are skipped (SQL/pandas posture).
    let mut n: Column[i64] = Column.new();
    n.push(10);
    n.push_null();
    n.push(20);
    println(f"{n.fold(0, |a, x| a + x)}");

    // Empty column returns `init` unchanged (the fold identity — no trap).
    let e: Column[i64] = Column.from_vec([]);
    println(f"{e.fold(99, |a, x| a + x)}");
}
"#,
    );
    assert_eq!(out, "15\n120\n3\n!!!\n30\n99\n");
}

#[test]
fn column_map_reduction() {
    // `Column.map(|x| ...) -> Column[T]` — element-wise map producing a fresh
    // column; null slots pass through (the parallel validity bitmap keeps them
    // null). Covers a plain map, a captured outer variable, and null
    // preservation (valid_count / len unchanged, sum over valid slots).
    let out = run_no_errors(
        r#"
fn main() {
    let c: Column[i64] = Column.from_vec([1, 2, 3, 4]);
    let d = c.map(|x| x * 2);
    println(f"{d.sum()}");

    let k: i64 = 100;
    let e = c.map(|x| x + k);
    println(f"{e.sum()}");

    let mut n: Column[i64] = Column.new();
    n.push(10);
    n.push_null();
    n.push(20);
    let m = n.map(|x| x * 3);
    println(f"{m.sum()} {m.valid_count()} {m.len()}");
}
"#,
    );
    // sum([2,4,6,8])=20; sum([101,102,103,104])=410; nulls: 30+60=90, 2 valid, len 3.
    assert_eq!(out, "20\n410\n90 2 3\n");
}

#[test]
fn column_sorted_argsort_narrow_widths_reduction() {
    // The `run` surface for narrow-width `Column.sorted`/`argsort` (the native
    // backend gained i32/u32/f32 support via a widened scratch sort; the
    // interpreter has always been width-agnostic, so this locks the parity).
    // i32, u32, and f32 columns, each with a null dropped by `sorted`.
    let out = run_no_errors(
        r#"
fn main() {
    let mut ci: Column[i32] = Column.with_capacity(4);
    ci.push(5); ci.push(1); ci.push_null(); ci.push(3);
    let cs = ci.sorted();
    let ca = ci.argsort();
    println(f"{cs[0]} {cs[1]} {cs[2]} | {ca[0]} {ca[1]} {ca[2]}");
    let cu: Column[u32] = Column.from_vec([30, 10, 20]);
    let us = cu.sorted();
    let ua = cu.argsort();
    println(f"{us[0]} {us[1]} {us[2]} | {ua[0]} {ua[1]} {ua[2]}");
    let mut cf: Column[f32] = Column.with_capacity(4);
    cf.push(2.5); cf.push_null(); cf.push(1.5); cf.push(0.5);
    let fs = cf.sorted();
    let fa = cf.argsort();
    println(f"{fs[0]} {fs[1]} {fs[2]} | {fa[0]} {fa[1]} {fa[2]}");
}
"#,
    );
    // i32 valid [5,1,3] sorted [1,3,5]; argsort original slots [1,3,0].
    // u32 [30,10,20] sorted [10,20,30]; argsort [1,2,0].
    // f32 valid [2.5,1.5,0.5] sorted [0.5,1.5,2.5]; argsort [3,2,0].
    assert_eq!(
        out,
        "1 3 5 | 1 3 0\n10 20 30 | 1 2 0\n0.5 1.5 2.5 | 3 2 0\n"
    );
}

/// B-2026-08-14-7 — an `f32` / `f16` / `bf16` operator must produce a value of
/// that width, not the f64 the interpreter computes it in.
///
/// `Value::Float` is an f64 and carries no width tag, so every narrow-float
/// operator computed AND STORED at double precision and kept bits no compiled
/// backend has. The narrowing CASTS were given explicit rounding in
/// B-2026-07-22-4 and the TENSOR element-wise path in B-2026-08-05-31; scalar
/// arithmetic was the third site and never got it. Each line below printed a
/// different number under `--interp` than from the built binary, with `karac
/// check` silent — the expectations here are the compiled values.
///
/// Rounding once at the end is exact, not an approximation: a single IEEE
/// `+`/`-`/`*`/`/` is correctly rounded when the intermediate carries `2p+2`
/// bits, and f64's 53 cover f32 (50), f16 (24) and bf16 (18).
///
/// The `bf16` line is why the row's "only `+` on f32 was measured" note
/// mattered — 8 mantissa bits means `1.0 + 0.01` already diverges, where f32
/// needs a value engineered past its ULP. The `sqrt` line is the second site
/// the fix had to reach: the float-math methods compute at f64 too, and
/// codegen calls `sqrtf`. The f64 lines are controls that must not move, and
/// the `-t` line pins that negation is exact at any width and needs no
/// rounding.
#[test]
fn narrow_float_arithmetic_rounds_to_declared_width() {
    let out = run_no_errors(
        r#"
fn main() {
    let a: f32 = 4000000000u32 as f32;
    let one: f32 = 1 as f32;
    println(f"01 {a + one}");
    println(f"02 {a - one}");
    println(f"03 {a / (3 as f32)}");
    let t: f32 = 0.1 as f32;
    println(f"04 {t * (3 as f32)}");
    println(f"05 {t + t + t}");
    let mut m: f32 = 4000000000 as f32;
    m += one;
    println(f"06 {m}");
    println(f"07 {-t}");
    println(f"08 {t.sqrt()}");
    let bh: bf16 = 1.0bf16;
    println(f"09 {bh + 0.01bf16}");
    let hh: f16 = 1.0f16;
    println(f"10 {hh + 0.0005f16}");
    let d: f64 = 0.1;
    println(f"11 {d * 3.0}");
    println(f"12 {d.sqrt()}");
}
"#,
    );
    assert_eq!(
        out,
        "01 4000000000\n\
         02 4000000000\n\
         03 1333333376\n\
         04 0.30000001192092896\n\
         05 0.30000001192092896\n\
         06 4000000000\n\
         07 -0.10000000149011612\n\
         08 0.3162277638912201\n\
         09 1.0078125\n\
         10 1.0009765625\n\
         11 0.30000000000000004\n\
         12 0.31622776601683794\n"
    );
}

/// B-2026-08-14-7, storage half — a value ENTERING a narrow-float slot must be
/// rounded into it, before any operator runs.
///
/// The row was filed as an arithmetic bug and its scope was corrected once
/// B-2026-08-14-6's sweep showed the identical divergence at `field_assign`,
/// `vec_push` and `vec_idx_assign`, none of which computes anything. 4294967295
/// is not representable in f32 — it rounds to 4294967296, which is what every
/// compiled backend stores — so an interpreter that widened the `u32` to f64
/// and stopped was holding a value the slot's own declared type cannot express.
/// Fixing only the operators would have left all three of these wrong.
///
/// B-2026-08-14-6 built the channel this needs: the typechecker flags every
/// integer expression sitting in a float slot. It recorded presence only; the
/// declared WIDTH now rides along, which is the whole change on this half.
///
/// The `f64` lines are the controls — the same values are exact there, so a
/// rounding applied indiscriminately would show up as a wrong answer on them.
#[test]
fn int_to_narrow_float_store_rounds_into_the_slot() {
    let out = run_no_errors(
        r#"
struct H { mut f: f32 }
struct G { mut f: f64 }

fn main() {
    let big = 4294967295u32;
    let mid = 2147483647i32;
    let mut h = H { f: 0 as f32 };
    h.f = big;
    println(f"01 {h.f}");
    h.f = mid;
    println(f"02 {h.f}");
    let mut vf: Vec[f32] = Vec.new();
    vf.push(big);
    println(f"03 {vf[0i64]}");
    vf[0i64] = mid;
    println(f"04 {vf[0i64]}");
    let mut g = G { f: 0.0 };
    g.f = big;
    println(f"05 {g.f}");
    let mut vd: Vec[f64] = Vec.new();
    vd.push(big);
    println(f"06 {vd[0i64]}");
}
"#,
    );
    assert_eq!(
        out,
        "01 4294967296\n\
         02 2147483648\n\
         03 4294967296\n\
         04 2147483648\n\
         05 4294967295\n\
         06 4294967295\n"
    );
}

#[test]
fn test_dataframe_read_csv_round_trips_types_nulls_and_quoting() {
    // `DataFrame.read_csv` (phase-11 CSV leg, slice 2) inverts `write_csv`:
    // header → names, per-column inference (all-i64 → Column[i64], else
    // all-f64 → Column[f64], else String), unquoted-empty → NULL, quoted
    // cells unescape (comma kept, doubled quotes → one). Round-trip a mixed
    // table and read back values, types, and the null.
    let tmp = std::env::temp_dir().join("kara_interp_df_read_csv_rt.csv");
    let _ = std::fs::remove_file(&tmp);
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        "fn main() with writes(FileSystem) reads(FileSystem) {{\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"age\", Column.from_vec([30i64, 25i64, 41i64]));\n\
             df.insert(\"score\", Column.from_vec([91.5, 78.25, 88.0]));\n\
             df.insert(\"name\", Column.from_vec([\"ada\", \"bob, jr.\", \"eve \\\"the\\\" grey\"]));\n\
             let nn: Vec[Option[i64]] = vec![Some(1i64), None, Some(3i64)];\n\
             df.insert(\"nullable\", Column.from_iter_nullable(nn));\n\
             let _ = df.write_csv(\"{path}\");\n\
             match DataFrame.read_csv(\"{path}\") {{\n\
                 Ok(back) => {{\n\
                     println(back.width()); println(back.height());\n\
                     let ages: Column[i64] = back.column(\"age\");\n\
                     match ages[2] {{ Some(v) => println(v), None => println(-1i64) }}\n\
                     let scores: Column[f64] = back.column(\"score\");\n\
                     match scores[1] {{ Some(v) => println(v), None => println(-1.0) }}\n\
                     let names: Column[String] = back.column(\"name\");\n\
                     match names[1] {{ Some(v) => println(v), None => println(\"null\") }}\n\
                     match names[2] {{ Some(v) => println(v), None => println(\"null\") }}\n\
                     let nulls: Column[i64] = back.column(\"nullable\");\n\
                     println(nulls.null_count());\n\
                     match nulls[1] {{ Some(v) => println(v), None => println(\"null-ok\") }}\n\
                 }}\n\
                 Err(_) => println(\"read failed\"),\n\
             }}\n\
         }}"
    );
    let out = run_no_errors(&src);
    let _ = std::fs::remove_file(&tmp);
    assert_eq!(
        out, "4\n3\n41\n78.25\nbob, jr.\neve \"the\" grey\n1\nnull-ok\n",
        "read_csv must invert write_csv: types inferred, quoting unescaped, nulls kept",
    );
}

#[test]
fn test_dataframe_read_csv_missing_file_and_ragged_rows_err() {
    // A missing file surfaces the mapped IoError; a ragged row (cell count
    // ≠ header count) is IoError.Other with the parser's message.
    let tmp = std::env::temp_dir().join("kara_interp_df_read_csv_ragged.csv");
    std::fs::write(&tmp, "a,b\n1,2\n3\n").unwrap();
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        "fn main() with reads(FileSystem) {{\n\
             match DataFrame.read_csv(\"/definitely-missing-kara.csv\") {{\n\
                 Ok(_) => println(\"bad\"),\n\
                 Err(_) => println(\"missing-err\"),\n\
             }}\n\
             match DataFrame.read_csv(\"{path}\") {{\n\
                 Ok(_) => println(\"bad\"),\n\
                 Err(_) => println(\"ragged-err\"),\n\
             }}\n\
         }}"
    );
    let out = run_no_errors(&src);
    let _ = std::fs::remove_file(&tmp);
    assert_eq!(out, "missing-err\nragged-err\n");
}

#[test]
fn test_lazyframe_explain_and_collect_with_pushdown() {
    // Phase-11 LazyDataFrame slice 1: `df.lazy().select(..).limit(..)`
    // records a plan; `explain()` renders the logical plan (innermost
    // SCAN) plus the optimized single-scan form (consecutive limits fuse
    // to the min, the projection pushes into the scan); `collect()` runs
    // the optimized plan (subset + reorder + truncate, typed reads).
    let out = run_no_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64, 2i64, 3i64, 4i64]));\n\
             df.insert(\"b\", Column.from_vec([10.5, 20.5, 30.5, 40.5]));\n\
             df.insert(\"c\", Column.from_vec([\"x\", \"y\", \"z\", \"w\"]));\n\
             let plan = df.lazy().select(vec![\"b\", \"a\"]).limit(7).limit(2);\n\
             println(plan.explain());\n\
             let out = plan.collect();\n\
             println(out.width()); println(out.height());\n\
             for n in out.column_names() { println(n); }\n\
             let b: Column[f64] = out.column(\"b\");\n\
             match b[1] { Some(v) => println(v), None => println(-1.0) }\n\
         }",
    );
    assert_eq!(
        out,
        "== logical plan ==\n\
         LIMIT 2\n\
         \x20 LIMIT 7\n\
         \x20   SELECT [b, a]\n\
         \x20     SCAN [a, b, c]\n\
         == optimized ==\n\
         SCAN cols=[b, a] limit=2\n\
         2\n2\nb\na\n20.5\n",
        "explain must render logical + fused optimized plan; collect must run it",
    );
}

#[test]
fn test_lazyframe_collect_validates_at_run_not_build() {
    // A select naming a missing column BUILDS fine (plans validate when
    // they run); `explain()` renders INVALID PLAN; `collect()` is the
    // runtime error.
    let out = run_no_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64]));\n\
             let bad = df.lazy().select(vec![\"nope\"]);\n\
             println(\"built-ok\");\n\
             println(bad.explain());\n\
         }",
    );
    assert!(
        out.contains("built-ok")
            && out.contains("INVALID PLAN: LazyFrame.select: no column named 'nope'"),
        "got: {out}",
    );
    let errors = runtime_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64]));\n\
             let _ = df.lazy().select(vec![\"nope\"]).collect();\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("no column named 'nope'")),
        "{errors:?}",
    );
}

#[test]
fn test_lazyframe_filter_expression_pipeline() {
    // Phase-11 LazyDataFrame slice 2: `filter` takes an inspectable
    // `LazyExpr` predicate (built by `col(..)` + comparison/boolean
    // methods). explain renders the FILTER step in both plans — the
    // optimized pipeline keeps filter/limit relative order (they do not
    // commute), lifts the projection to the top, and scan-projects the
    // union of needed columns in source order. collect evaluates per row.
    let out = run_no_errors(
        "import std.lazy.{col};\n\
         fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"age\", Column.from_vec([30i64, 15i64, 41i64, 8i64]));\n\
             df.insert(\"name\", Column.from_vec([\"ada\", \"bob\", \"eve\", \"kid\"]));\n\
             df.insert(\"score\", Column.from_vec([9.5, 3.5, 7.0, 1.0]));\n\
             let plan = df.lazy()\n\
                 .filter(col(\"age\").gt(10).and_(col(\"score\").ge(3.5)))\n\
                 .select(vec![\"name\", \"age\"])\n\
                 .limit(2);\n\
             println(plan.explain());\n\
             let out = plan.collect();\n\
             println(out.height());\n\
             let names: Column[String] = out.column(\"name\");\n\
             match names[0] { Some(v) => println(v), None => println(\"null\") }\n\
             match names[1] { Some(v) => println(v), None => println(\"null\") }\n\
         }",
    );
    assert_eq!(
        out,
        "== logical plan ==\n\
         LIMIT 2\n\
         \x20 SELECT [name, age]\n\
         \x20   FILTER ((age > 10) and (score >= 3.5))\n\
         \x20     SCAN [age, name, score]\n\
         == optimized ==\n\
         SELECT [name, age]\n\
         \x20 LIMIT 2\n\
         \x20   FILTER ((age > 10) and (score >= 3.5))\n\
         \x20     SCAN cols=[age, name, score]\n\
         2\nada\nbob\n",
        "filter must render in both plans and evaluate per row",
    );
}

#[test]
fn test_lazyframe_filter_nulls_col_vs_col_and_combinators() {
    // NULL slots fail comparisons (rows dropped); col-vs-col comparisons;
    // eq/ne + not_ + or_; adjacent filters fuse with `and` in the
    // optimized plan; LazyExpr.col is the canonical (import-free) spelling.
    let out = run_no_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"age\", Column.from_vec([30i64, 15i64, 41i64, 8i64]));\n\
             df.insert(\"name\", Column.from_vec([\"ada\", \"bob\", \"eve\", \"kid\"]));\n\
             let nn: Vec[Option[i64]] = vec![Some(30i64), None, Some(41i64), Some(8i64)];\n\
             df.insert(\"opt\", Column.from_iter_nullable(nn));\n\
             println(df.lazy().filter(LazyExpr.col(\"opt\").eq(LazyExpr.col(\"age\"))).collect().height());\n\
             println(df.lazy().filter(LazyExpr.col(\"name\").ne(\"bob\").not_()).collect().height());\n\
             println(df.lazy().filter(LazyExpr.col(\"age\").lt(10).or_(LazyExpr.col(\"age\").gt(40))).collect().height());\n\
             let fused = df.lazy().filter(LazyExpr.col(\"age\").gt(10)).filter(LazyExpr.col(\"age\").lt(40));\n\
             println(fused.collect().height());\n\
             println(fused.explain());\n\
         }",
    );
    assert_eq!(
        out,
        "3\n1\n2\n2\n\
         == logical plan ==\n\
         FILTER (age < 40)\n\
         \x20 FILTER (age > 10)\n\
         \x20   SCAN [age, name, opt]\n\
         == optimized ==\n\
         FILTER ((age > 10) and (age < 40))\n\
         \x20 SCAN cols=[age]\n",
        "nulls drop, col-vs-col compares, adjacent filters fuse",
    );
}

#[test]
fn test_lazyframe_filter_errors_validate_at_collect() {
    // A predicate naming a missing column builds; collect errors. A
    // String-vs-number comparison is a runtime error (loud, not empty).
    let errors = runtime_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64]));\n\
             let _ = df.lazy().filter(LazyExpr.col(\"nope\").gt(0)).collect();\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("no column named 'nope'")),
        "{errors:?}",
    );
    let errors = runtime_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64]));\n\
             let _ = df.lazy().filter(LazyExpr.col(\"a\").gt(\"x\")).collect();\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("cannot compare values of different types")),
        "{errors:?}",
    );
}

#[test]
fn test_lazyframe_sort_multikey_desc_nulls_last_stable() {
    // Phase-11 LazyDataFrame slice 3: stable multi-key sort with a
    // `.desc()` key marker; NULL keys sort LAST regardless of direction;
    // sort renders in both explain plans and preserves order vs limit.
    let out = run_no_errors(
        "import std.lazy.{col};\n\
         fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"dept\", Column.from_vec([\"b\", \"a\", \"b\", \"a\", \"b\"]));\n\
             df.insert(\"score\", Column.from_vec([3i64, 9i64, 7i64, 9i64, 3i64]));\n\
             df.insert(\"name\", Column.from_vec([\"v\", \"w\", \"x\", \"y\", \"z\"]));\n\
             let plan = df.lazy().sort(vec![col(\"dept\"), col(\"score\").desc()]).select(vec![\"name\"]).limit(3);\n\
             println(plan.explain());\n\
             let out = plan.collect();\n\
             let names: Column[String] = out.column(\"name\");\n\
             match names[0] { Some(v) => println(v), None => println(\"null\") }\n\
             match names[1] { Some(v) => println(v), None => println(\"null\") }\n\
             match names[2] { Some(v) => println(v), None => println(\"null\") }\n\
             let mut d2: DataFrame = DataFrame.new();\n\
             let nn: Vec[Option[i64]] = vec![Some(5i64), None, Some(9i64)];\n\
             d2.insert(\"k\", Column.from_iter_nullable(nn));\n\
             d2.insert(\"tag\", Column.from_vec([\"five\", \"none\", \"nine\"]));\n\
             let s2 = d2.lazy().sort(vec![col(\"k\").desc()]).collect();\n\
             let tags: Column[String] = s2.column(\"tag\");\n\
             match tags[0] { Some(v) => println(v), None => println(\"null\") }\n\
             match tags[2] { Some(v) => println(v), None => println(\"null\") }\n\
         }",
    );
    assert_eq!(
        out,
        "== logical plan ==\n\
         LIMIT 3\n\
         \x20 SELECT [name]\n\
         \x20   SORT [dept, score desc]\n\
         \x20     SCAN [dept, score, name]\n\
         == optimized ==\n\
         SELECT [name]\n\
         \x20 LIMIT 3\n\
         \x20   SORT [dept, score desc]\n\
         \x20     SCAN cols=[dept, score, name]\n\
         w\ny\nx\nnine\nnone\n",
        "multi-key desc sort must be stable with NULLs last under desc",
    );
}

#[test]
fn test_lazyframe_sort_errors() {
    // A missing sort-key column errors at collect; desc() outside a sort
    // key (in a filter) errors loudly.
    let errors = runtime_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64]));\n\
             let _ = df.lazy().sort(vec![LazyExpr.col(\"nope\")]).collect();\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("no column named 'nope'")),
        "{errors:?}",
    );
    let errors = runtime_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64]));\n\
             let _ = df.lazy().filter(LazyExpr.col(\"a\").desc().gt(0)).collect();\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("only meaningful as a LazyFrame.sort key")),
        "{errors:?}",
    );
}

#[test]
fn test_lazyframe_group_by_agg_pipeline() {
    // Phase-11 LazyDataFrame slice 4: the full design-sketch pipeline —
    // filter → group_by(keys) → agg(count/sum/mean with alias_) → sort on
    // a DERIVED column. Groups are first-occurrence-ordered; the output
    // schema is keys then aggregates (alias_ wins, else <col>_<op>);
    // downstream ops validate against the derived schema.
    let out = run_no_errors(
        "import std.lazy.{col};\n\
         fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"city\", Column.from_vec([\"oslo\", \"rome\", \"oslo\", \"rome\", \"oslo\"]));\n\
             df.insert(\"name\", Column.from_vec([\"a\", \"b\", \"c\", \"d\", \"e\"]));\n\
             df.insert(\"pop\", Column.from_vec([10i64, 20i64, 30i64, 40i64, 50i64]));\n\
             let plan = df.lazy()\n\
                 .filter(col(\"pop\").gt(15))\n\
                 .group_by(vec![col(\"city\")])\n\
                 .agg(vec![col(\"name\").count().alias_(\"cnt\"), col(\"pop\").sum(), col(\"pop\").mean()])\n\
                 .sort(vec![col(\"pop_sum\").desc()]);\n\
             println(plan.explain());\n\
             let out = plan.collect();\n\
             println(out.width()); println(out.height());\n\
             for n in out.column_names() { println(n); }\n\
             let cities: Column[String] = out.column(\"city\");\n\
             let sums: Column[i64] = out.column(\"pop_sum\");\n\
             let means: Column[f64] = out.column(\"pop_mean\");\n\
             match cities[0] { Some(v) => println(v), None => println(\"null\") }\n\
             match sums[0] { Some(v) => println(v), None => println(-1i64) }\n\
             match means[0] { Some(v) => println(v), None => println(-1.0) }\n\
             match cities[1] { Some(v) => println(v), None => println(\"null\") }\n\
             match sums[1] { Some(v) => println(v), None => println(-1i64) }\n\
         }",
    );
    assert_eq!(
        out,
        "== logical plan ==\n\
         SORT [pop_sum desc]\n\
         \x20 GROUP BY [city] AGG [count(name) as cnt, sum(pop), mean(pop)]\n\
         \x20   FILTER (pop > 15)\n\
         \x20     SCAN [city, name, pop]\n\
         == optimized ==\n\
         SORT [pop_sum desc]\n\
         \x20 GROUP BY [city] AGG [count(name) as cnt, sum(pop), mean(pop)]\n\
         \x20   FILTER (pop > 15)\n\
         \x20     SCAN cols=[city, name, pop]\n\
         4\n2\ncity\ncnt\npop_sum\npop_mean\noslo\n80\n40\nrome\n60\n",
        "group_by/agg must derive schema and sort on derived columns",
    );
}

#[test]
fn test_lazyframe_group_by_agg_nulls_and_errors() {
    // count() counts NON-NULL only; sum over an all-null group is NULL;
    // a non-aggregate agg entry and an aggregate in filter position are
    // loud errors; a post-groupby ref to a pre-groupby column errors.
    let out = run_no_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"k\", Column.from_vec([\"x\", \"x\", \"y\"]));\n\
             let nn: Vec[Option[i64]] = vec![Some(1i64), None, None];\n\
             df.insert(\"v\", Column.from_iter_nullable(nn));\n\
             let out = df.lazy().group_by(vec![LazyExpr.col(\"k\")]).agg(vec![LazyExpr.col(\"v\").count(), LazyExpr.col(\"v\").sum()]).collect();\n\
             let cnts: Column[i64] = out.column(\"v_count\");\n\
             let sums: Column[i64] = out.column(\"v_sum\");\n\
             match cnts[0] { Some(v) => println(v), None => println(-1i64) }\n\
             match cnts[1] { Some(v) => println(v), None => println(-1i64) }\n\
             match sums[1] { Some(v) => println(v), None => println(\"null-ok\") }\n\
         }",
    );
    assert_eq!(out, "1\n0\nnull-ok\n");
    let errors = runtime_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64]));\n\
             let _ = df.lazy().group_by(vec![LazyExpr.col(\"a\")]).agg(vec![LazyExpr.col(\"a\")]).collect();\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("must be an aggregate")),
        "{errors:?}",
    );
    let errors = runtime_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64]));\n\
             let _ = df.lazy().filter(LazyExpr.col(\"a\").sum().gt(0)).collect();\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("only meaningful inside LazyGroupBy.agg")),
        "{errors:?}",
    );
    let errors = runtime_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"k\", Column.from_vec([\"x\"]));\n\
             df.insert(\"v\", Column.from_vec([1i64]));\n\
             let _ = df.lazy().group_by(vec![LazyExpr.col(\"k\")]).agg(vec![LazyExpr.col(\"v\").sum()]).filter(LazyExpr.col(\"v\").gt(0)).collect();\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("no column named 'v' at this plan step")),
        "{errors:?}",
    );
}

#[test]
fn test_lazyframe_constant_folding_and_cse() {
    // Phase-11 LazyDataFrame slice 6: constant folding + CSE — the last
    // two passes of the pinned Option A optimizer list. `lit(..)` wraps a
    // runtime scalar as an expression; literal arms fold at PLAN time
    // (the logical plan keeps the verbatim tree, the optimized one shows
    // the folded form): `x and true` → `x` (and the scan projection
    // narrows to what the folded predicate reads), a constant-true
    // filter drops out entirely (single-scan rendering returns), a
    // constant-false one stays honest (`FILTER false`, empty collect,
    // schema preserved), `x or true` collapses the filter away, literal
    // comparisons fold through `not_`, and structurally-identical
    // conjuncts dedupe (CSE) — both within one predicate and across
    // adjacent fused filters.
    let out = run_no_errors(
        "import std.lazy.{col, lit};\n\
         fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64, 5i64, 9i64]));\n\
             df.insert(\"b\", Column.from_vec([10i64, 20i64, 30i64]));\n\
             let enabled = true;\n\
             let plan = df.lazy().filter(col(\"a\").gt(2).and_(lit(enabled))).select(vec![\"a\"]);\n\
             println(plan.explain());\n\
             println(plan.collect().height());\n\
             let t = df.lazy().filter(lit(true)).select(vec![\"b\"]);\n\
             println(t.explain());\n\
             println(t.collect().height());\n\
             let f = df.lazy().filter(lit(false));\n\
             println(f.explain());\n\
             println(f.collect().height());\n\
             println(f.collect().width());\n\
             let dup = df.lazy().filter(col(\"a\").gt(2)).filter(col(\"a\").gt(2));\n\
             println(dup.explain());\n\
             println(dup.collect().height());\n\
             let dom = df.lazy().filter(col(\"a\").gt(2).or_(lit(true)));\n\
             println(dom.explain());\n\
             println(dom.collect().height());\n\
             let cmpf = df.lazy().filter(lit(3).gt(4).not_());\n\
             println(cmpf.explain());\n\
             let inner = df.lazy().filter(col(\"a\").gt(2).and_(col(\"a\").gt(2)));\n\
             println(inner.explain());\n\
         }",
    );
    assert_eq!(
        out,
        "== logical plan ==\n\
         SELECT [a]\n\
         \x20 FILTER ((a > 2) and true)\n\
         \x20   SCAN [a, b]\n\
         == optimized ==\n\
         SELECT [a]\n\
         \x20 FILTER (a > 2)\n\
         \x20   SCAN cols=[a]\n\
         2\n\
         == logical plan ==\n\
         SELECT [b]\n\
         \x20 FILTER true\n\
         \x20   SCAN [a, b]\n\
         == optimized ==\n\
         SCAN cols=[b]\n\
         3\n\
         == logical plan ==\n\
         FILTER false\n\
         \x20 SCAN [a, b]\n\
         == optimized ==\n\
         FILTER false\n\
         \x20 SCAN cols=[*]\n\
         0\n2\n\
         == logical plan ==\n\
         FILTER (a > 2)\n\
         \x20 FILTER (a > 2)\n\
         \x20   SCAN [a, b]\n\
         == optimized ==\n\
         FILTER (a > 2)\n\
         \x20 SCAN cols=[a]\n\
         2\n\
         == logical plan ==\n\
         FILTER ((a > 2) or true)\n\
         \x20 SCAN [a, b]\n\
         == optimized ==\n\
         SCAN cols=[*]\n\
         3\n\
         == logical plan ==\n\
         FILTER (not (3 > 4))\n\
         \x20 SCAN [a, b]\n\
         == optimized ==\n\
         SCAN cols=[*]\n\
         == logical plan ==\n\
         FILTER ((a > 2) and (a > 2))\n\
         \x20 SCAN [a, b]\n\
         == optimized ==\n\
         FILTER (a > 2)\n\
         \x20 SCAN cols=[a]\n",
        "literal arms must fold at plan time; duplicate conjuncts must dedupe",
    );
}

#[test]
fn test_lazyframe_constant_folding_preserves_errors() {
    // Folding must never mask an error: a bad column name in an elided
    // branch stays loud (validation runs on the ORIGINAL expression);
    // `lit` rejects non-scalar values; a type-mismatched literal
    // comparison stays UNFOLDED and errors at collect.
    let errors = runtime_errors(
        "import std.lazy.{col, lit};\n\
         fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64]));\n\
             let _ = df.lazy().filter(col(\"nope\").gt(1).and_(lit(false))).collect();\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("no column named 'nope' at this plan step")),
        "{errors:?}",
    );
    let errors = runtime_errors(
        "import std.lazy.{lit};\n\
         fn main() {\n\
             let v: Vec[i64] = vec![1i64];\n\
             let _ = lit(v);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("LazyExpr.lit expects a scalar literal")),
        "{errors:?}",
    );
    let errors = runtime_errors(
        "import std.lazy.{lit};\n\
         fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64]));\n\
             let _ = df.lazy().filter(lit(1).eq(\"x\")).collect();\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("cannot compare values of different types")),
        "{errors:?}",
    );
}

#[test]
fn test_lazyframe_with_columns_arithmetic_pipeline() {
    // Phase-11 LazyDataFrame slice 7: `with_columns` (the expression-
    // projection leg) + arithmetic expression nodes (add/sub/mul/div).
    // Entries need an output name (bare col keeps its own — a rename
    // copy via alias_; computed entries must be aliased); results
    // REPLACE a same-named column in place or APPEND; entries see the
    // step's INPUT frame (Polars parallel semantics). NULL propagates
    // through arithmetic; i64 pairs stay i64, an f64 side widens; a
    // computed bool column comes from a comparison expression; sort
    // works on a computed column. A select BEFORE with_columns flushes
    // as an explicit SELECT step whose columns join the scan set (they
    // flow through to the output); constant folding applies inside
    // entries (`2 + 1` folds to `3`).
    let out = run_no_errors(
        "import std.lazy.{col, lit};\n\
         fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64, 2i64, 3i64]));\n\
             df.insert(\"b\", Column.from_vec([10.5, 20.5, 30.5]));\n\
             df.insert(\"name\", Column.from_vec([\"x\", \"y\", \"z\"]));\n\
             let plan = df.lazy().with_columns(vec![col(\"a\").mul(2).alias_(\"a2\"), col(\"b\").add(col(\"a\")).alias_(\"ab\"), col(\"name\").alias_(\"label\")]);\n\
             println(plan.explain());\n\
             let out = plan.collect();\n\
             println(out.width());\n\
             for n in out.column_names() { println(n); }\n\
             let a2: Column[i64] = out.column(\"a2\");\n\
             let ab: Column[f64] = out.column(\"ab\");\n\
             match a2[2] { Some(v) => println(v), None => println(-1i64) }\n\
             match ab[0] { Some(v) => println(v), None => println(-1.0) }\n\
             let rep = df.lazy().with_columns(vec![col(\"a\").add(100).alias_(\"a\")]).collect();\n\
             for n in rep.column_names() { println(n); }\n\
             let ra: Column[i64] = rep.column(\"a\");\n\
             match ra[0] { Some(v) => println(v), None => println(-1i64) }\n\
             let nn: Vec[Option[i64]] = vec![Some(1i64), None];\n\
             let mut nf: DataFrame = DataFrame.new();\n\
             nf.insert(\"v\", Column.from_iter_nullable(nn));\n\
             let v10: Column[i64] = nf.lazy().with_columns(vec![col(\"v\").mul(10).alias_(\"v10\")]).collect().column(\"v10\");\n\
             match v10[0] { Some(v) => println(v), None => println(\"null\") }\n\
             match v10[1] { Some(v) => println(v), None => println(\"null\") }\n\
             let piped = df.lazy()\n\
                 .select(vec![\"a\", \"b\"])\n\
                 .with_columns(vec![col(\"a\").mul(lit(2).add(1)).alias_(\"a3\")])\n\
                 .filter(col(\"a3\").gt(3));\n\
             println(piped.explain());\n\
             let out4 = piped.collect();\n\
             println(out4.height());\n\
             let a3: Column[i64] = out4.column(\"a3\");\n\
             match a3[0] { Some(v) => println(v), None => println(-1i64) }\n\
             let bg: Column[bool] = df.lazy().with_columns(vec![col(\"a\").ge(2).alias_(\"big\")]).collect().column(\"big\");\n\
             match bg[0] { Some(v) => println(v), None => println(\"null\") }\n\
             match bg[1] { Some(v) => println(v), None => println(\"null\") }\n\
             let s = df.lazy().with_columns(vec![col(\"a\").mul(-1).alias_(\"neg\")]).sort(vec![col(\"neg\")]).collect();\n\
             let sa: Column[i64] = s.column(\"a\");\n\
             match sa[0] { Some(v) => println(v), None => println(-1i64) }\n\
         }",
    );
    assert_eq!(
        out,
        "== logical plan ==\n\
         WITH [(a * 2) as a2, (b + a) as ab, name as label]\n\
         \x20 SCAN [a, b, name]\n\
         == optimized ==\n\
         WITH [(a * 2) as a2, (b + a) as ab, name as label]\n\
         \x20 SCAN cols=[a, b, name]\n\
         6\na\nb\nname\na2\nab\nlabel\n\
         6\n11.5\n\
         a\nb\nname\n101\n\
         10\nnull\n\
         == logical plan ==\n\
         FILTER (a3 > 3)\n\
         \x20 WITH [(a * (2 + 1)) as a3]\n\
         \x20   SELECT [a, b]\n\
         \x20     SCAN [a, b, name]\n\
         == optimized ==\n\
         FILTER (a3 > 3)\n\
         \x20 WITH [(a * 3) as a3]\n\
         \x20   SELECT [a, b]\n\
         \x20     SCAN cols=[a, b]\n\
         2\n6\n\
         false\ntrue\n\
         3\n",
        "with_columns must compute/replace/append, fold inside entries, flush selects honestly",
    );
}

#[test]
fn test_lazyframe_with_columns_errors() {
    // Unnamed computed entries and duplicate output names are loud at
    // fold (explain shows INVALID PLAN, collect errors); i64 division
    // by zero and arithmetic on non-numeric columns are loud at eval.
    let errors = runtime_errors(
        "import std.lazy.{col};\n\
         fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64]));\n\
             let _ = df.lazy().with_columns(vec![col(\"a\").mul(2)]).collect();\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("each entry needs an output name")),
        "{errors:?}",
    );
    let errors = runtime_errors(
        "import std.lazy.{col};\n\
         fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64]));\n\
             let _ = df.lazy().with_columns(vec![col(\"a\").mul(2).alias_(\"z\"), col(\"a\").add(1).alias_(\"z\")]).collect();\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("duplicate output name 'z'")),
        "{errors:?}",
    );
    let errors = runtime_errors(
        "import std.lazy.{col};\n\
         fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64, 0i64]));\n\
             let _ = df.lazy().with_columns(vec![col(\"a\").div(col(\"a\")).alias_(\"q\")]).collect();\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("integer division by zero")),
        "{errors:?}",
    );
    let errors = runtime_errors(
        "import std.lazy.{col};\n\
         fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"s\", Column.from_vec([\"x\"]));\n\
             let _ = df.lazy().with_columns(vec![col(\"s\").add(1).alias_(\"t\")]).collect();\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("arithmetic on non-numeric values")),
        "{errors:?}",
    );
}

// ── Narrow-float precision (B-2026-07-22-4) ────────────────────────────────

#[test]
fn test_narrow_float_literals_and_casts_round_to_storage_precision() {
    // The interpreter models every float as f64, but a value TYPED f16/bf16
    // must still be representable in that format or it diverges from all
    // three LLVM backends (which materialize `2.7bf16` as the bfloat
    // constant 2.703125 and lower `x as bf16` with an RNE truncation).
    // Literals round at creation; casts round at the cast.
    assert_eq!(run("println(2.7bf16);\n"), "2.703125\n");
    assert_eq!(run("println(2.7f16);\n"), "2.69921875\n");
    // Casts through a fn boundary (the B-2026-07-22-4 filing shape).
    assert_eq!(
        run("fn nar(x: f64) -> bf16 { x as bf16 }\nprintln(nar(2.7));\n"),
        "2.703125\n"
    );
    assert_eq!(
        run("fn narf(x: f64) -> f16 { x as f16 }\nprintln(narf(2.7));\n"),
        "2.69921875\n"
    );
    assert_eq!(
        run("fn nar32(x: f32) -> bf16 { x as bf16 }\nprintln(nar32(2.7f32));\n"),
        "2.703125\n"
    );
    // int → narrow float takes the rounding path too (65505 is not
    // f16-representable; nearest even is 65504).
    assert_eq!(run("println(65505 as f16);\n"), "65504\n");
    // Exact values stay exact.
    assert_eq!(run("println(1.25bf16);\nprintln(1.5f16);\n"), "1.25\n1.5\n");
}

/// B-2026-08-30-40 — the `f16.` / `bf16.` associated-call namespace RUNS, and
/// gives the same answers as `f32.` / `f64.` / `i64.`.
///
/// The row reported a typecheck rejection: `f16.add(a, b)` was refused with
/// "'f16' is a type, not a function". Widening the two typechecker gates it
/// named turned that into a GREEN CHECK THAT FAILED IN BOTH BACKENDS, and
/// widening the interpreter's two gates alongside left codegen returning
/// literal ZEROS for every f16 and bf16 operation — a silent wrong answer,
/// strictly worse than the rejection the row started from. Four more copies of
/// the same longhand width list had to move together; this fixture is what
/// proves they did.
///
/// The `f32` / `f64` / `i64` / `String` / `bool` rows are the controls: they
/// worked before and are what the new widths are being held to. A fix that
/// admitted the narrow widths by loosening the gate for everyone would show up
/// here as one of these changing.
///
/// Twin of `tests/codegen.rs`'s `e2e_narrow_float_assoc_call_namespace_runs`,
/// pinned to the same string.
///
/// The seed is the literal 1 rather than `env.args().len()` (B-2026-09-03-1):
/// an IN-PROCESS interpreter test sees the TEST binary's argv, which is 1 only
/// when the suite runs unfiltered and 2+ under `cargo test <filter>`. Written
/// the other way, this fixture PASSED a full run and FAILED the single-test
/// workflow CLAUDE.md documents (`cargo test -- test_name`) -- every numeric
/// slot shifted with the seed, which reads exactly like a `main`-is-red
/// regression and cost a session a bisect before the filter was suspected. The
/// codegen twin keeps `env.args()` because it needs an opaque seed to survive
/// -O2 folding, and 1 is what that yields under its harness.
#[test]
fn test_narrow_float_assoc_call_namespace_runs() {
    assert_eq!(
        run(r#"fn main() {
    let n: i64 = 1;
    let a: f16 = ((n as f32) + 1.5f32) as f16;
    let b: bf16 = ((n as f32) + 2.5f32) as bf16;
    let c: f32 = (n as f32) + 3.5f32;
    let d: f64 = (n as f64) + 4.5;
    println(f"f16  {f16.add(a, a)} {f16.sub(a, a)} {f16.mul(a, a)} {f16.div(a, a)}");
    println(f"bf16 {bf16.add(b, b)} {bf16.sub(b, b)} {bf16.mul(b, b)}");
    println(f"f32  {f32.add(c, c)} {f32.mul(c, c)}");
    println(f"f64  {f64.add(d, d)} {f64.mul(d, d)}");
    println(f"i64  {i64.add(n, n)} {i64.lt(n, n + 1)}");
    println(f"str  {String.add(f"a", f"b")}");
    println(f"bool {bool.eq(true, true)}");
}
"#),
        r#"f16  5 0 6.25 1
bf16 7 0 12.25
f32  9 20.25
f64  11 30.25
i64  2 true
str  ab
bool true
"#
    );
}

/// B-2026-08-31-9 — a `#[derive(Display)]` struct renders its `Vector` and
/// narrow-float FIELDS, and renders them the same way at every depth.
///
/// Two defects with one symptom. `display_field_is_leaf` omitted `f16`/`bf16`,
/// so a narrow-float field refused to compile at all — while the same field
/// read on its own (`f"{h.a}"`) rendered correctly on both backends, which is
/// the refusal inverted with respect to difficulty. And a `Vector` field could
/// not join that list: the parts path synthesizes each field expression with
/// the BASE's span, and a vector interpolation resolves through the span-keyed
/// `vector_typed_exprs` table, so it misses and falls to the SCALAR renderer.
/// Admitting it as a leaf printed `WithVec { v: 1, n: 1 }` — one stray lane,
/// the B-2026-08-29-52 defect — which is why the vector case routes through the
/// by-pointer renderer instead.
///
/// THE `nest` ROW IS THE ONE THAT PROVES THE APPROACH. That spelling already
/// worked before this fix, because a container element goes through
/// `emit_struct_debug_display_fn` — the same by-pointer renderer the top-level
/// case now uses. `top` and `nest` printing the same struct identically is the
/// assertion that the two paths agree rather than merely both succeeding.
///
/// `plain` is the control: a struct whose fields the parts path handles must
/// keep taking it and keep its text, since the fix adds a second path rather
/// than replacing the first. `u128` is in it because that width was this same
/// list's previous omission (B-2026-08-19-23).
///
/// Twin of `tests/codegen.rs`'s `e2e_derived_display_renders_vector_and_narrow_float_fields`,
/// pinned to the same string.
#[test]
fn test_derived_display_renders_vector_and_narrow_float_fields() {
    assert_eq!(
        run(r#"#[derive(Display)]
struct WithVec { v: Vector[i32, 4], n: i64 }
#[derive(Display)]
struct WithNarrow { a: f16, b: bf16, c: f32 }
#[derive(Display)]
struct Plain { n: i64, s: String, w: u128 }

fn main() {
    // The seed is the literal 1 rather than `env.args().len()`: an IN-PROCESS
    // interpreter test sees the TEST binary's argv, which is 1 only when the
    // suite runs unfiltered and 2+ under `cargo test <filter>`. The codegen
    // twin keeps `env.args()` because it needs an opaque seed to survive -O2
    // folding, and 1 is what that yields under its harness.
    let n: i64 = 1;
    let iv: Vector[i32, 4] = Vector[i32, 4](1i32, -2i32, 3i32, (0i32 - 4i32));
    let fv: Vector[f64, 2] = Vector[f64, 2](1.5f64, -2.25f64);

    let h = WithVec { v: iv, n: n };
    println(f"top   {h}");
    let hs: Vec[WithVec] = [h];
    println(f"nest  {hs}");

    let g = WithVec { v: fv2(), n: n };
    println(f"float {g}");

    let m = WithNarrow { a: ((n as f32) + 1.5f32) as f16, b: ((n as f32) + 2.5f32) as bf16, c: (n as f32) + 3.5f32 };
    println(f"narrow {m}");

    let p = Plain { n: n, s: f"s", w: 340282366920938463463374607431768211455u128 };
    println(f"plain {p}");
}

fn fv2() -> Vector[i32, 4] { return Vector[i32, 4](9i32, 8i32, 7i32, 6i32) }
"#),
        r#"top   WithVec { v: Vector(1, -2, 3, -4), n: 1 }
nest  [WithVec { v: Vector(1, -2, 3, -4), n: 1 }]
float WithVec { v: Vector(9, 8, 7, 6), n: 1 }
narrow WithNarrow { a: 2.5, b: 3.5, c: 4.5 }
plain Plain { n: 1, s: s, w: 340282366920938463463374607431768211455 }
"#
    );
}

/// B-2026-08-14-12 — the interpreter twin of
/// `tests/codegen.rs::test_e2e_float_narrowing_as_cast_rounds_to_the_target_width`,
/// same source and same expected string.
///
/// The row's complaint was "no diagnostic AND no rounding": before the gate,
/// `let d: f32 = c` printed `0.1` here. The gate now rejects that spelling, and
/// this pins that the `as` it recommends is not a no-op on the surface where
/// the value is interpreted rather than compiled — the two backends must agree
/// on the rounded value, or the fix-it would trade a silent narrowing for a
/// run-vs-build split.
#[test]
fn test_float_narrowing_as_cast_rounds_to_the_target_width() {
    assert_eq!(
        run("fn takef(x: f32) -> f32 { x }\n\
             fn main() {\n\
                 let c: f64 = 0.1;\n\
                 println(c);\n\
                 let d: f32 = c as f32;\n\
                 println(d);\n\
                 println(takef(c as f32));\n\
                 let mut v: Vec[f32] = Vec.new();\n\
                 v.push(c as f32);\n\
                 println(v[0]);\n\
                 let h: f16 = c as f16;\n\
                 println(h);\n\
             }"),
        "0.1\n\
         0.10000000149011612\n\
         0.10000000149011612\n\
         0.10000000149011612\n\
         0.0999755859375\n"
    );
}

/// The narrow widths keep trapping exactly as before — the guard against a
/// carrier change that fixes the 64-bit case by breaking the case that already
/// worked.
#[test]
fn narrow_widths_still_trap_with_the_i128_carrier() {
    for (ty, expr) in [
        ("i8", "127i8 + 1i8"),
        ("i16", "32767i16 + 1i16"),
        ("i32", "2147483647i32 + 1i32"),
        ("u8", "255u8 + 1u8"),
        ("u16", "65535u16 + 1u16"),
        ("u32", "4294967295u32 + 1u32"),
    ] {
        let errors = runtime_errors(&format!(
            "fn main() {{ let x: {ty} = {expr}; println(x); }}"
        ));
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("integer overflow")),
            "expected an integer-overflow trap for {ty} ({expr}), got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }
}

#[test]
fn gpu_argmin_makes_nan_lose_unlike_stats_argmin() {
    // A DELIBERATE difference from `Stats.argmin`, which is position-dependent
    // on NaN — it seeds its best with element 0 and displaces only on a strict
    // comparison, so `Stats.argmin([NaN, 3.0, 1.0])` is 0 while
    // `[3.0, 1.0, NaN]` is 1. A halving tree cannot reproduce that, because
    // the grouping decides the positions. Making NaN always lose restores
    // associativity; `gpu.argmin` answers 2 for the first buffer.
    for (buf, want) in [("[nan, 3.0, 1.0]", "2"), ("[3.0, 1.0, nan]", "1")] {
        let out = run_no_errors(&format!(
            "fn main() {{\n\
            \x20   let zero: f32 = 0.0;\n\
            \x20   let nan: f32 = zero / zero;\n\
            \x20   let v: Vec[f32] = {buf};\n\
            \x20   let m = gpu.argmin(v);\n\
            \x20   match m {{\n\
            \x20       Some(x) => println(f\"{{x}}\"),\n\
            \x20       None => println(\"empty\"),\n\
            \x20   }}\n\
            }}"
        ));
        assert_eq!(out.trim(), want, "gpu.argmin {buf}");
    }
}

/// The counterweight: SIGNED and narrow-unsigned values must render exactly as
/// they did. The peel changes the reading only for the two widths whose top
/// half does not fit the carrier, so a negative `i64` in an `Option` has to stay
/// negative — the failure mode of over-applying this fix is turning every `-1`
/// into 18446744073709551615.
#[test]
fn the_display_peel_leaves_signed_and_narrow_values_alone() {
    assert_eq!(
        run_no_errors(
            "fn main() {\n\
             let s: Option[i64] = Some(0i64 - 1i64);\n\
             println(s);\n\
             let mut si: Vec[i64] = vec![];\n\
             si.push(0i64 - 7i64);\n\
             println(si);\n\
             let mut n8: Vec[u8] = vec![];\n\
             n8.push(200u8);\n\
             println(n8);\n\
             let tn = (0i64 - 3i64, 4i32);\n\
             println(tn);\n\
             let f: Option[f64] = Some(1.5);\n\
             println(f);\n\
             let st: Option[String] = Some(\"hi\");\n\
             println(st);\n\
             let b: Option[bool] = Some(true);\n\
             println(b);\n\
             }"
        ),
        "Some(-1)\n\
         [-7]\n\
         [200]\n\
         (-3, 4)\n\
         Some(1.5)\n\
         Some(hi)\n\
         Some(true)\n"
    );
}

// ── B-2026-08-19-20: Stats refusals are runtime errors, not raw panics ──

#[test]
fn stats_empty_refusals_are_runtime_errors_not_panics() {
    // B-2026-08-19-20. `eval_stats_fn` used `panic!` for the empty-input
    // refusals, so `karac run` printed a Rust backtrace naming
    // `src/interpreter/helpers.rs` and exited 101, while `karac build` printed
    // a Kara-level panic with a source span and exited 1 — a run-vs-build
    // divergence on the one case a caller is most likely to hit from real data,
    // and a violation of the standing rule that phases emit structured
    // diagnostics rather than panicking.
    //
    // Reaching `runtime_errors` at all is the assertion: a `panic!` would
    // unwind the test process instead of returning here.
    for (call, want) in [
        ("Stats.mean(v)", "Stats.mean() called on empty slice"),
        (
            "Stats.variance(v)",
            "Stats.variance() called on empty slice",
        ),
        ("Stats.stddev(v)", "Stats.stddev() called on empty slice"),
        ("Stats.median(v)", "Stats.median() called on empty slice"),
        (
            "Stats.percentile(v, 50.0)",
            "Stats.percentile() called on empty slice",
        ),
    ] {
        let src = format!("fn main() {{ let v: Vec[f64] = []; println({call}); }}");
        let errors = runtime_errors(&src);
        assert!(
            errors.iter().any(|e| e.message.contains(want)),
            "expected a runtime error containing {want:?} for {call}, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
        );
    }
}

#[test]
fn stats_empty_refusals_cover_the_i64_element_axis_too() {
    // The int dispatcher is a separate function with its own guards, so the
    // policy is consulted once at the shared call site rather than duplicated —
    // otherwise the two element axes can drift apart on the message, which is
    // exactly how the `stddev` half of this bug reached codegen.
    for (call, want) in [
        ("Stats.mean(v)", "Stats.mean() called on empty slice"),
        ("Stats.median(v)", "Stats.median() called on empty slice"),
    ] {
        let src = format!("fn main() {{ let v: Vec[i64] = []; println({call}); }}");
        let errors = runtime_errors(&src);
        assert!(
            errors.iter().any(|e| e.message.contains(want)),
            "expected a runtime error containing {want:?} for {call}, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
        );
    }
}

#[test]
fn stats_percentile_out_of_range_is_a_runtime_error() {
    // The same dispatcher's other refusal, on the same channel.
    let errors = runtime_errors(
        "fn main() { let v: Vec[f64] = [1.0]; println(Stats.percentile(v, 150.0)); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("p must be in [0, 100]")),
        "expected the percentile range refusal, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn stats_int_overflow_is_a_runtime_error_not_a_panic() {
    // B-2026-08-19-25 — the last raw `panic!` in the Stats dispatchers.
    // `sum`/`prod` over i64 are CHECKED folds, and the overflow trap used
    // `panic!`, so `karac run` printed a Rust backtrace naming
    // `src/interpreter/helpers.rs` and exited 101 where `karac build` printed a
    // clean `integer overflow` at exit 1.
    //
    // Unlike the empty-input refusals (B-2026-08-19-20), overflow cannot be
    // pre-checked at the call site — it is only discovered inside the fold — so
    // the int dispatcher returns `Result` and the caller reports the `Err`.
    //
    // Reaching `runtime_errors` at all is the assertion: a `panic!` would
    // unwind the test process instead of returning here.
    for call in ["Stats.sum(v)", "Stats.prod(v)"] {
        let src = format!(
            "fn main() {{ let v: Vec[i64] = [9223372036854775807, 9223372036854775807]; println({call}); }}"
        );
        let errors = runtime_errors(&src);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("integer overflow")),
            "expected an `integer overflow` runtime error for {call}, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
        );
    }
}

#[test]
fn stats_int_overflow_message_matches_plain_arithmetic() {
    // The message is deliberately the BARE `integer overflow` that `+`/`*`
    // already produce on both legs, not a Stats-specific wording. Measured
    // before choosing: a plain `a + 1` past i64::MAX prints `integer overflow`
    // under `--interp` and under `karac build` alike. Naming the function here
    // would have made Stats the one arithmetic trap in the language that words
    // itself differently — and the span already says which call faulted.
    let plain =
        runtime_errors("fn main() { let a = 9223372036854775807; let b = a + 1; println(b); }");
    let stats = runtime_errors(
        "fn main() { let v: Vec[i64] = [9223372036854775807, 9223372036854775807]; println(Stats.sum(v)); }",
    );
    let msg_of = |es: &[karac::interpreter::RuntimeError]| {
        es.iter()
            .find(|e| e.message.contains("integer overflow"))
            .map(|e| e.message.clone())
    };
    assert_eq!(
        msg_of(&plain),
        msg_of(&stats),
        "Stats overflow must word itself exactly like plain arithmetic overflow",
    );
}

#[test]
fn stats_int_reductions_still_work_when_they_do_not_overflow() {
    // The `Result` conversion touched every arm of the int dispatcher; pin that
    // the ordinary paths still return values rather than errors.
    assert_eq!(
        run("fn main() {
                 let v: Vec[i64] = [3, 1, 4, 1, 5];
                 println(Stats.sum(v));
                 println(Stats.prod(v));
                 println(Stats.median(v));
                 println(Stats.sort(v));
             }"),
        "14\n60\n3\n[1, 1, 3, 4, 5]\n"
    );
}
