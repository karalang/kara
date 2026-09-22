//! DataFrame, LazyFrame, Column, stats, CSV/Arrow IPC -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen frames::
//!
//! New fixtures about DataFrame, LazyFrame, Column, stats, CSV/Arrow IPC belong in this file.

use super::*;

#[test]
fn arrow_ipc_builtins_rejected_by_codegen() {
    // Arrow IPC interchange — BOTH directions now have AOT twins for all
    // three receivers, so every one must COMPILE to its runtime call. This
    // test began life asserting the opposite (loud deferral) and has been
    // flipped a leg at a time; what it guards now is that no leg silently
    // regresses to the `const 0` ASSOC-call default, which would return
    // the integer 0 in place of a `Column`/`DataFrame`/`Tensor` (the
    // B-2026-07-18-20 run-vs-build divergence class). Byte-parity of the
    // write direction is asserted by
    // `test_e2e_to_arrow_ipc_matches_interpreter_bytes`, the read
    // direction's round-trip by `test_e2e_from_arrow_ipc_round_trips`.
    for (to_ipc, sym) in [
        (
            "fn main() { let c: Column[i64] = Column.from_vec([1, 2]); \
                 let b = c.to_arrow_ipc(); println(b.len()); }",
            "karac_arrow_column_to_ipc",
        ),
        (
            "fn main() { let mut d = DataFrame.new(); \
                 d.insert(\"x\", Column.from_vec([1, 2])); \
                 let b = d.to_arrow_ipc(); println(b.len()); }",
            "karac_arrow_dataframe_to_ipc",
        ),
        (
            "fn main() { let t = Tensor.from([[1, 2], [3, 4]]); \
                 let b = t.to_arrow_ipc(); println(b.len()); }",
            "karac_arrow_tensor_to_ipc",
        ),
    ] {
        let ir = ir_result(to_ipc)
            .unwrap_or_else(|e| panic!("{sym}: to_arrow_ipc has a codegen twin: {e}"));
        assert!(ir.contains(sym), "to_arrow_ipc must lower to `{sym}`");
    }

    for (from_ipc, sym) in [
        (
            "fn main() { let bytes: Vec[u8] = Vec.new(); \
                 let d: Column[i64] = Column.from_arrow_ipc(bytes); println(d.len()); }",
            "karac_arrow_column_from_ipc",
        ),
        (
            "fn main() { let bytes: Vec[u8] = Vec.new(); \
                 let d = DataFrame.from_arrow_ipc(bytes); println(d.height()); }",
            "karac_arrow_dataframe_from_ipc",
        ),
        (
            "fn main() { let bytes: Vec[u8] = Vec.new(); \
                 let t: Tensor[i64, [2, 2]] = Tensor.from_arrow_ipc(bytes); println(t.rank()); }",
            "karac_arrow_tensor_from_ipc",
        ),
    ] {
        let ir = ir_result(from_ipc)
            .unwrap_or_else(|e| panic!("{sym}: from_arrow_ipc has a codegen twin: {e}"));
        assert!(ir.contains(sym), "from_arrow_ipc must lower to `{sym}`");
    }
}

#[test]
fn test_e2e_to_arrow_ipc_matches_interpreter_bytes() {
    // Phase-11 Arrow IPC codegen twin — the `karac_arrow_*_to_ipc`
    // entrypoints (runtime/src/arrow_ipc.rs) walk codegen's control blocks
    // and serialize with arrow-rs, the same crate + version the
    // interpreter uses, so the two must emit BYTE-IDENTICAL IPC streams
    // for every receiver.
    //
    // The oracle is the interpreter run in-process, not a hard-coded byte
    // string: arrow-rs owns the IPC framing, so pinning exact bytes would
    // make this test a version tripwire rather than a parity check. Length
    // + checksum over the stream catches any divergence in schema, buffer
    // layout, or null encoding.
    //
    // The cases target the places the two backends could plausibly drift,
    // i.e. the parity rules the runtime implements deliberately (see its
    // module header):
    //
    //   * **Width widening** — the interpreter's `Value` erases int/float
    //     width, so a compiled `Column[i32]` / `Tensor[i32, …]` must
    //     serialize as Int64, not the physically narrower Arrow type.
    //   * **All-null fallback** — an EMPTY `Column[String]` must emit
    //     Int64, matching the interpreter's "no valid slot to key on"
    //     default rather than the statically-known Utf8.
    //   * **Explicit row count** — a zero-column DataFrame still has to
    //     produce a valid batch, which only works because both sides pass
    //     the row count explicitly (arrow can't infer it with no arrays).
    //   * **Header-derived shape** — a Tensor's rank/dims come from its
    //     runtime header on the codegen side but from the interpreter's
    //     `dims` on the other; the `arrow.fixed_shape_tensor` extension
    //     metadata carries them into the stream, so any mismatch shows.
    let cases = [
        (
            "column: i64 with a null",
            "let mut c: Column[i64] = Column.new();\n\
                 c.push(10); c.push(20); c.push_null(); c.push(40);\n\
                 let bytes = c.to_arrow_ipc();",
        ),
        (
            "column: f64",
            "let c: Column[f64] = Column.from_vec([1.5, 2.5, 3.5]);\n\
                 let bytes = c.to_arrow_ipc();",
        ),
        (
            "column: String",
            "let c: Column[String] = Column.from_vec([\"alpha\", \"beta\"]);\n\
                 let bytes = c.to_arrow_ipc();",
        ),
        (
            "column: empty (all-null Int64 fallback parity)",
            "let c: Column[String] = Column.new();\n\
                 let bytes = c.to_arrow_ipc();",
        ),
        (
            "dataframe: i64 + f64 + String",
            "let mut df: DataFrame = DataFrame.new();\n\
                 df.insert(\"age\", Column.from_vec([30i64, 25i64, 41i64]));\n\
                 df.insert(\"score\", Column.from_vec([91.5, 78.25, 88.0]));\n\
                 df.insert(\"name\", Column.from_vec([\"ada\", \"bob\", \"eve\"]));\n\
                 let bytes = df.to_arrow_ipc();",
        ),
        (
            "dataframe: nullable + bool + String",
            "let mut df: DataFrame = DataFrame.new();\n\
                 let nn: Vec[Option[i64]] = vec![Some(1i64), None, Some(3i64)];\n\
                 df.insert(\"nullable\", Column.from_iter_nullable(nn));\n\
                 df.insert(\"flag\", Column.from_vec([true, false, true]));\n\
                 df.insert(\"name\", Column.from_vec([\"ada\", \"bob\", \"eve\"]));\n\
                 let bytes = df.to_arrow_ipc();",
        ),
        (
            "dataframe: narrow + unsigned columns (width-widening parity)",
            "let mut df: DataFrame = DataFrame.new();\n\
                 df.insert(\"u\", Column.from_vec([1u64, 2u64, 3u64]));\n\
                 df.insert(\"s\", Column.from_vec([-1i32, 0i32, 7i32]));\n\
                 df.insert(\"f\", Column.from_vec([1.5f32, 2.5f32, 3.5f32]));\n\
                 let bytes = df.to_arrow_ipc();",
        ),
        (
            "dataframe: zero columns (explicit row-count parity)",
            "let df: DataFrame = DataFrame.new();\n\
                 let bytes = df.to_arrow_ipc();",
        ),
        (
            "tensor: 2-D i64",
            "let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 let bytes = t.to_arrow_ipc();",
        ),
        (
            "tensor: 3-D f64",
            "let t: Tensor[f64, [2, 2, 2]] = \
                 Tensor.from([[[1.5, 2.5], [3.5, 4.5]], [[5.5, 6.5], [7.5, 8.5]]]);\n\
                 let bytes = t.to_arrow_ipc();",
        ),
        (
            "tensor: i32 (width-widening parity)",
            "let t: Tensor[i32, [2, 3]] = Tensor.zeros([2, 3]);\n\
                 let bytes = t.to_arrow_ipc();",
        ),
        (
            "tensor: bool",
            "let t: Tensor[bool, [2, 2]] = Tensor.full([2, 2], true);\n\
                 let bytes = t.to_arrow_ipc();",
        ),
    ];
    for (label, body) in cases {
        let src = format!(
            "fn main() {{\n\
                     {body}\n\
                     println(bytes.len());\n\
                     let mut sum: i64 = 0;\n\
                     for b in bytes {{ sum = sum + (b as i64); }}\n\
                     println(sum);\n\
                 }}"
        );
        // Interpreter oracle, in-process.
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errors: {interp_errs:?}"
        );
        let expected = interp_out.join("");
        // A zero-length stream would make the checksum vacuous.
        assert!(
            expected.lines().next().and_then(|l| l.parse::<i64>().ok()) > Some(8),
            "{label}: interpreter produced an implausibly short stream: {expected:?}"
        );
        if let Some(aot) = run_program(&src) {
            assert_eq!(
                aot, expected,
                "{label}: AOT `to_arrow_ipc` must emit byte-identical \
                     Arrow IPC to the interpreter (len + checksum)",
            );
        }
    }
}

#[test]
fn test_e2e_from_arrow_ipc_round_trips() {
    // Phase-11 Arrow IPC read direction — `karac_arrow_column_from_ipc` /
    // `karac_arrow_dataframe_from_ipc` parse a stream and BUILD the
    // control-block graph codegen would have built itself.
    //
    // The oracle is again the in-process interpreter, but the assertion is
    // stronger than the write direction's: each case round-trips through
    // `to_arrow_ipc` and then reads VALUES back out, so a graph that is
    // merely well-formed (right length, freeable) but holds the wrong
    // bytes still fails. Sums and null counts, not just shapes.
    //
    // Cases target where the two directions can disagree:
    //
    //   * **Declared vs. stream type** — the stream carries Int64/Float64
    //     regardless of the column's declared width (the write side's
    //     widening rule), so reading into `Column[i32]` / `Column[f32]`
    //     exercises the narrowing half of the conversion.
    //   * **Nulls** — a null slot must stay null through both directions
    //     and must never attempt a conversion.
    //   * **The all-null Int64 fallback** — an empty `Column[String]`
    //     serializes as Int64; reading it back into `Column[String]` only
    //     works because a null (or absent) slot bypasses the
    //     String-vs-numeric rejection.
    //   * **Frame reconstruction** — names, order, and per-column types
    //     all come from the schema, and a String column additionally
    //     allocates a per-cell heap the frame must own.
    //   * **Shape reconciliation** — a Tensor's dims come from the
    //     stream's `arrow.fixed_shape_tensor` metadata but must satisfy
    //     the receiver's annotation, so both a fully-static shape and one
    //     with a `?` axis are exercised. Rejection of a MISMATCHED shape
    //     is `test_e2e_tensor_from_arrow_ipc_shape_mismatch_traps`.
    //   * **Temporary argument** — `from(to())` in one expression: the
    //     intermediate buffer has no other owner, so the call site frees
    //     it, and only after the runtime has read it.
    let cases = [
        (
            "column i64 with nulls",
            "let mut c: Column[i64] = Column.new();\n\
                 c.push(10); c.push(20); c.push_null(); c.push(40);\n\
                 let b = c.to_arrow_ipc();\n\
                 let r: Column[i64] = Column.from_arrow_ipc(b);\n\
                 println(r.len()); println(r.null_count()); println(r.sum());",
        ),
        (
            "column i32 (narrowing back from the Int64 stream)",
            "let c: Column[i32] = Column.from_vec([1i32, -2i32, 3i32]);\n\
                 let r: Column[i32] = Column.from_arrow_ipc(c.to_arrow_ipc());\n\
                 println(r.len()); println(r.sum());",
        ),
        (
            "column f32 (narrowing back from the Float64 stream)",
            "let c: Column[f32] = Column.from_vec([1.5f32, 2.25f32]);\n\
                 let r: Column[f32] = Column.from_arrow_ipc(c.to_arrow_ipc());\n\
                 println(r.len()); println(r.sum());",
        ),
        (
            "column String",
            "let c: Column[String] = Column.from_vec([\"alpha\", \"beta\"]);\n\
                 let b = c.to_arrow_ipc();\n\
                 let r: Column[String] = Column.from_arrow_ipc(b);\n\
                 println(r.len()); println(r.null_count());",
        ),
        (
            "column bool",
            "let c: Column[bool] = Column.from_vec([true, false, true]);\n\
                 let r: Column[bool] = Column.from_arrow_ipc(c.to_arrow_ipc());\n\
                 println(r.len()); println(r.null_count());",
        ),
        (
            "column empty String (all-null Int64 fallback must be accepted)",
            "let c: Column[String] = Column.new();\n\
                 let r: Column[String] = Column.from_arrow_ipc(c.to_arrow_ipc());\n\
                 println(r.len()); println(r.null_count());",
        ),
        (
            "dataframe i64 + f64 + String",
            "let mut df: DataFrame = DataFrame.new();\n\
                 df.insert(\"age\", Column.from_vec([30i64, 25i64, 41i64]));\n\
                 df.insert(\"score\", Column.from_vec([91.5, 78.25, 88.0]));\n\
                 df.insert(\"name\", Column.from_vec([\"ada\", \"bob\", \"eve\"]));\n\
                 let r: DataFrame = DataFrame.from_arrow_ipc(df.to_arrow_ipc());\n\
                 println(r.width()); println(r.height());",
        ),
        (
            "dataframe nullable + bool",
            "let mut df: DataFrame = DataFrame.new();\n\
                 let nn: Vec[Option[i64]] = vec![Some(1i64), None, Some(3i64)];\n\
                 df.insert(\"nullable\", Column.from_iter_nullable(nn));\n\
                 df.insert(\"flag\", Column.from_vec([true, false, true]));\n\
                 let r: DataFrame = DataFrame.from_arrow_ipc(df.to_arrow_ipc());\n\
                 println(r.width()); println(r.height());",
        ),
        (
            "dataframe zero columns",
            "let df: DataFrame = DataFrame.new();\n\
                 let r: DataFrame = DataFrame.from_arrow_ipc(df.to_arrow_ipc());\n\
                 println(r.width()); println(r.height());",
        ),
        (
            "double round-trip (a parsed graph must re-serialize)",
            "let c: Column[i64] = Column.from_vec([7i64, 8i64, 9i64]);\n\
                 let r1: Column[i64] = Column.from_arrow_ipc(c.to_arrow_ipc());\n\
                 let r2: Column[i64] = Column.from_arrow_ipc(r1.to_arrow_ipc());\n\
                 println(r2.len()); println(r2.sum());",
        ),
        (
            "tensor 2-D i64",
            "let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 let r: Tensor[i64, [2, 3]] = Tensor.from_arrow_ipc(t.to_arrow_ipc());\n\
                 println(r.rank()); println(r.sum());",
        ),
        (
            "tensor 3-D f64",
            "let t: Tensor[f64, [2, 2, 2]] = \
                 Tensor.from([[[1.5, 2.5], [3.5, 4.5]], [[5.5, 6.5], [7.5, 8.5]]]);\n\
                 let r: Tensor[f64, [2, 2, 2]] = Tensor.from_arrow_ipc(t.to_arrow_ipc());\n\
                 println(r.rank()); println(r.sum());",
        ),
        (
            "tensor i32 (narrowing back from the Int64 stream)",
            "let t: Tensor[i32, [2, 2]] = Tensor.full([2, 2], 7i32);\n\
                 let r: Tensor[i32, [2, 2]] = Tensor.from_arrow_ipc(t.to_arrow_ipc());\n\
                 println(r.rank()); println(r.sum());",
        ),
        (
            "tensor with a `?` axis (extent taken from the stream)",
            "let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 let r: Tensor[i64, [?, 3]] = Tensor.from_arrow_ipc(t.to_arrow_ipc());\n\
                 println(r.rank()); println(r.sum());",
        ),
        (
            "tensor double round-trip",
            "let t = Tensor.from([[2, 4], [6, 8]]);\n\
                 let r1: Tensor[i64, [2, 2]] = Tensor.from_arrow_ipc(t.to_arrow_ipc());\n\
                 let r2: Tensor[i64, [2, 2]] = Tensor.from_arrow_ipc(r1.to_arrow_ipc());\n\
                 println(r2.rank()); println(r2.sum());",
        ),
    ];
    for (label, body) in cases {
        let src = format!("fn main() {{\n{body}\n}}");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errors: {interp_errs:?}"
        );
        let expected = interp_out.join("");
        if let Some(aot) = run_program(&src) {
            assert_eq!(
                aot, expected,
                "{label}: AOT `from_arrow_ipc` must reconstruct the same \
                     values the interpreter does",
            );
        }
    }
}

#[test]
fn test_e2e_dataframe_write_csv_serializes_all_backends() {
    // Phase-11 CSV leg — the codegen twin (karac_runtime_df_write_csv
    // walks the DataFrame/Column control blocks; Rust `Display` IS the
    // interpreter's cell formatting). Byte-identical across backends:
    // header in schema order, println formatting, NULL → empty cell,
    // RFC-4180 quoting. This test replaced the slice-1 loud-deferral
    // assert when the twin landed.
    let tmp = std::env::temp_dir().join("kara_e2e_df_write_csv.csv");
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
    let out = run_program(&src);
    let _ = std::fs::remove_file(&tmp);
    if let Some(out) = out {
        assert_eq!(
            out,
            "age,score,name,nullable\n\
                 30,91.5,ada,1\n\
                 25,78.25,\"bob, jr.\",\n\
                 41,88,\"eve \"\"the\"\" grey\",3\n",
            "AOT write_csv must serialize byte-identically to the interpreter",
        );
    }
}

#[test]
fn lazyframe_rejected_by_codegen_with_run_hint() {
    // Phase-11 LazyDataFrame codegen twin: the FULL op surface now
    // LOWERS (select/limit/filter/sort/group_by+agg/join/with_columns +
    // every LazyExpr builder). The remaining loud bail is the user-
    // method leak guard — a user impl method declared to return a Lazy
    // value has no caller-side release registration, so it bails with
    // the `karac run` pointer. That guard is the canary now.
    let err = ir_result(
        "struct W { x: i64 }\n\
             impl W {\n\
                 fn make(ref self) -> LazyExpr { LazyExpr.col(\"a\") }\n\
             }\n\
             fn main() {\n\
                 let w = W { x: 1i64 };\n\
                 let e = w.make();\n\
                 let mut df: DataFrame = DataFrame.new();\n\
                 df.insert(\"a\", Column.from_vec([1i64]));\n\
                 let plan = df.lazy().filter(e.gt(0));\n\
                 let out = plan.collect();\n\
                 println(out.height());\n\
             }",
    )
    .expect_err("a user method returning LazyExpr must be rejected by the v1 codegen twin");
    assert!(
        err.contains(
            "returning LazyExpr/LazyFrame from user methods (`W.make`) is not yet \
                 lowered by the v1 codegen twin — run it with `karac run` \
                 (tracker: phase-11-stdlib-longtail.md § LazyDataFrame)"
        ),
        "got: {err}"
    );
    // And the FULL op surface must NOT bail: sort / group_by+agg (all
    // five aggregates, desc, alias_) / join / with_columns all compile.
    ir_result(
            "fn main() {\n\
                 let mut df: DataFrame = DataFrame.new();\n\
                 df.insert(\"k\", Column.from_vec([\"a\", \"b\"]));\n\
                 df.insert(\"v\", Column.from_vec([1i64, 2i64]));\n\
                 let plan = df.lazy()\n\
                     .with_columns(vec![LazyExpr.col(\"v\").mul(2).alias_(\"v2\")])\n\
                     .sort(vec![LazyExpr.col(\"v2\").desc()])\n\
                     .group_by(vec![LazyExpr.col(\"k\")])\n\
                     .agg(vec![LazyExpr.col(\"v2\").sum(), LazyExpr.col(\"v2\").count(), LazyExpr.col(\"v2\").mean(), LazyExpr.col(\"v2\").min(), LazyExpr.col(\"v2\").max()]);\n\
                 let mut r: DataFrame = DataFrame.new();\n\
                 r.insert(\"k\", Column.from_vec([\"a\"]));\n\
                 let joined = plan.join(r.lazy(), vec![\"k\"]);\n\
                 let out = joined.collect();\n\
                 println(out.height());\n\
             }",
        )
        .expect("the full LazyFrame op surface must compile under the codegen twin");
}

#[test]
fn test_e2e_dataframe_read_csv_round_trips_all_backends() {
    // Phase-11 CSV leg — the read_csv codegen twin
    // (karac_runtime_df_read_csv builds the malloc'd control-block graph
    // runtime-side; the Ok(df) pattern binding restores the pointer
    // shape, registers method dispatch, and owns the frame via
    // FreeDataFrame). Round-trip a mixed table through write_csv →
    // read_csv and read back values, inferred types, quoting, and the
    // null — output must match the interpreter twin. Replaced the
    // slice-2 loud-deferral assert when the twin landed.
    let tmp = std::env::temp_dir().join("kara_e2e_df_read_csv_rt.csv");
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
                 match DataFrame.read_csv(\"/definitely-missing-kara.csv\") {{\n\
                     Ok(_) => println(\"bad\"),\n\
                     Err(_) => println(\"missing-err\"),\n\
                 }}\n\
             }}"
        );
    let out = run_program(&src);
    let _ = std::fs::remove_file(&tmp);
    if let Some(out) = out {
        assert_eq!(
            out, "4\n3\n41\n78.25\nbob, jr.\neve \"the\" grey\n1\nnull-ok\nmissing-err\n",
            "read_csv round-trip must match the interpreter twin",
        );
    }
}

#[test]
fn test_e2e_lazyframe_group_by_agg_pipeline() {
    // Full-surface twin port: filter → group_by(keys) → agg(count/sum/
    // mean with alias_) → sort on a DERIVED column. First-occurrence
    // group order; output schema keys-then-aggregates (alias_ wins,
    // else <col>_<op>). Byte-pinned to the interpreter oracle
    // (tests/interpreter.rs::test_lazyframe_group_by_agg_pipeline).
    let out = run_program(
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
    if let Some(out) = out {
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
            "group_by/agg must derive schema and sort on derived columns \
                 byte-identically to the interpreter",
        );
    }
}

#[test]
fn test_e2e_lazyframe_with_columns_arithmetic_pipeline() {
    // Full-surface twin port: with_columns computes/replaces/appends,
    // folds constants inside entries (`2 + 1` → `3`), flushes a prior
    // select as an honest SELECT step, propagates NULL through
    // arithmetic, derives bool columns from comparisons, and sorts on
    // a computed column. Chain intermediates let-bound (see the join
    // pipeline test note). Mirrors the interpreter oracle
    // (test_lazyframe_with_columns_arithmetic_pipeline).
    let out = run_program(
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
                 let repp = df.lazy().with_columns(vec![col(\"a\").add(100).alias_(\"a\")]);\n\
                 let rep = repp.collect();\n\
                 for n in rep.column_names() { println(n); }\n\
                 let ra: Column[i64] = rep.column(\"a\");\n\
                 match ra[0] { Some(v) => println(v), None => println(-1i64) }\n\
                 let nn: Vec[Option[i64]] = vec![Some(1i64), None];\n\
                 let mut nf: DataFrame = DataFrame.new();\n\
                 nf.insert(\"v\", Column.from_iter_nullable(nn));\n\
                 let nfp = nf.lazy().with_columns(vec![col(\"v\").mul(10).alias_(\"v10\")]);\n\
                 let nfc = nfp.collect();\n\
                 let v10: Column[i64] = nfc.column(\"v10\");\n\
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
                 let bgp = df.lazy().with_columns(vec![col(\"a\").ge(2).alias_(\"big\")]);\n\
                 let bgc = bgp.collect();\n\
                 let bg: Column[bool] = bgc.column(\"big\");\n\
                 match bg[0] { Some(v) => println(v), None => println(\"null\") }\n\
                 match bg[1] { Some(v) => println(v), None => println(\"null\") }\n\
                 let sp = df.lazy().with_columns(vec![col(\"a\").mul(-1).alias_(\"neg\")]).sort(vec![col(\"neg\")]);\n\
                 let s = sp.collect();\n\
                 let sa: Column[i64] = s.column(\"a\");\n\
                 match sa[0] { Some(v) => println(v), None => println(-1i64) }\n\
             }",
        );
    if let Some(out) = out {
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
            "with_columns must compute/replace/append, fold inside entries, \
                 flush selects honestly — byte-identically to the interpreter",
        );
    }
}

#[test]
fn test_e2e_lazyframe_sort_multikey_desc_nulls_last_stable() {
    // Full-surface twin port: stable multi-key sort with a `.desc()`
    // marker; NULL keys sort LAST regardless of direction; sort renders
    // in both explain plans and preserves order vs limit. Byte-pinned
    // to the interpreter oracle
    // (test_lazyframe_sort_multikey_desc_nulls_last_stable).
    let out = run_program(
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
                 let s2p = d2.lazy().sort(vec![col(\"k\").desc()]);\n\
                 let s2 = s2p.collect();\n\
                 let tags: Column[String] = s2.column(\"tag\");\n\
                 match tags[0] { Some(v) => println(v), None => println(\"null\") }\n\
                 match tags[2] { Some(v) => println(v), None => println(\"null\") }\n\
             }",
        );
    if let Some(out) = out {
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
            "multi-key desc sort must be stable with NULLs last under desc, \
                 byte-identically to the interpreter",
        );
    }
}

/// A narrow local's SLOT is sized by its ANNOTATION, not by the width its
/// initializer literal happened to compile at.
///
/// `const_int_for_suffix` reads the SUFFIX alone, so a bare `5` compiled
/// at the default i64 however the binding was annotated, and
/// `bind_pattern` — which allocas at `val.get_type()` — then handed a
/// `u8` binding a 64-bit slot. Every width-sensitive operation on it ran
/// at 64 bits.
///
/// Arm (c) is why this is a soundness bug and not a cosmetic one: a
/// binding declared `i32` compared GREATER than `i32::MAX`. It is also
/// exactly the case `e2e_shift_runs_at_declared_width` above asserts — but
/// that test writes `1i32 << 31i32` WITH suffixes, and the suffixed
/// spelling was always correct, so B-2026-08-06-7's fix was never
/// exercised on the spelling anyone would actually write. Hence the
/// suffixed/unsuffixed PAIRS here: the point is that the two agree.
///
/// Arm (e) pins the negated literal, which reaches codegen as a lowered
/// `i8.neg(100)` CALL rather than a `Unary` — a first pass at the fix
/// matched only the `Unary` spelling and left this arm still wrong.
///
/// Arm (f) is the opposite direction (B-2026-08-13-15): a `u8` value under
/// an `i64` annotation must still WIDEN into a 64-bit slot. The two legs
/// share one helper, so a fix to either can break the other.
#[test]
fn e2e_narrow_let_slot_is_sized_by_its_annotation() {
    let Some(out) = run_program(
        "fn main() {\n\
             \x20   // (a) unsuffixed narrow shift == suffixed narrow shift\n\
             \x20   let a: u8 = 200u8;\n\
             \x20   println(a << 4u8);\n\
             \x20   let b: u8 = 200;\n\
             \x20   println(b << 4);\n\
             \x20   // (b) same for a signed narrow sign-bit flip\n\
             \x20   let c: i32 = 1i32;\n\
             \x20   println(c << 31i32);\n\
             \x20   let d: i32 = 1;\n\
             \x20   println(d << 31);\n\
             \x20   // (c) …so an `i32` binding can no longer exceed i32::MAX\n\
             \x20   println(d << 31 > 2147483647);\n\
             \x20   // (d) the value still round-trips as its declared type\n\
             \x20   let e: u8 = 255;\n\
             \x20   println(e);\n\
             \x20   let f: i8 = -128;\n\
             \x20   println(f);\n\
             \x20   // (e) a NEGATED unsuffixed literal (a lowered `neg` call)\n\
             \x20   let g: i8 = -100;\n\
             \x20   println(g << 1);\n\
             \x20   let h: i8 = -100i8;\n\
             \x20   println(h << 1i8);\n\
             \x20   // (f) the WIDENING direction still widens\n\
             \x20   let i: u8 = 200;\n\
             \x20   let j: i64 = i;\n\
             \x20   println(j);\n\
             \x20   // (g) 64-bit and pointer-width annotations are untouched\n\
             \x20   let k: u64 = 5;\n\
             \x20   println(k);\n\
             \x20   let l: usize = 5;\n\
             \x20   println(l);\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "128\n128\n-2147483648\n-2147483648\nfalse\n\
             255\n-128\n56\n56\n200\n5\n5\n"
    );
}

/// B-2026-08-06-7, the negation leg — `-iN::MIN` traps at the DECLARED
/// width, not just at the i64 carrier's.
///
/// `compile_unaryop` lowers a negate as a checked `0 - v`, but that check
/// runs on the i64 carrier, so every narrow negation was effectively
/// unchecked: `-(-2147483648i32)` yielded 2147483648 — a value the declared
/// `i32` cannot represent — while the i64 twin trapped correctly. That left
/// narrow negation as the last operator violating "a narrow-typed value
/// always fits its declared width", which B-2026-08-05-38's widening half
/// depends on.
///
/// Pinned at the IR level as well as by behaviour, for the same reason the
/// shift guard is: the E2E twin can only prove the trap for the widths it
/// names, and the check has to be there for all of them.
#[test]
fn test_ir_narrow_negation_emits_range_check() {
    for src in [
        "fn f(a: i8) -> i8 { -a }",
        "fn f(a: i16) -> i16 { -a }",
        "fn f(a: i32) -> i32 { -a }",
    ] {
        let ir = ir_for(src);
        assert!(
            ir.contains("ni.oob") || ir.contains("ni.ovf.trap"),
            "`{src}` must range-check the negated value against its \
                 declared width:\n{ir}"
        );
    }
}

/// The behavioural twin of the above, across every narrow width plus the
/// i64 control — and the legal boundary, which must NOT trap.
#[test]
fn e2e_narrow_negation_traps_at_declared_width() {
    // Legal: `-(iN::MIN + 1)` fits at every width, and so does the i64 case.
    let Some(out) = run_program(
        "fn main() {\n\
             \x20   let a: i32 = -2147483647i32;\n\
             \x20   println(-a);\n\
             \x20   let b: i16 = -32767i16;\n\
             \x20   println(-b);\n\
             \x20   let c: i8 = -127i8;\n\
             \x20   println(-c);\n\
             \x20   let d: i64 = -9223372036854775807i64;\n\
             \x20   println(-d);\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "2147483647\n32767\n127\n9223372036854775807\n");
}

/// The trap itself, on a RUNTIME-derived operand — a constant `-i32::MIN`
/// would let the optimizer fold the check away, which would leave the test
/// proving nothing about the guard actually emitted (B-2026-08-04-17's
/// vacuity hazard applied to a trap fixture). The seed is opaque and the
/// value reaches `i32::MIN` only at run time.
#[test]
fn e2e_narrow_negation_of_min_traps() {
    let src = r#"
fn main() {
    let n: i64 = env.args().len() as i64;
    let base: i32 = -2147483648i32;
    let a: i32 = base + (n as i32) - 1i32;
    println(a);
    println(-a);
}
"#;
    if let Some(cap) = run_program_capturing(src) {
        assert_eq!(cap.status.code(), Some(101), "stderr={:?}", cap.stderr);
        assert!(
            cap.stdout.contains("-2147483648"),
            "the operand must reach i32::MIN before the negate, \
                 got stdout={:?}",
            cap.stdout
        );
        assert!(
            cap.stderr.contains("integer overflow"),
            "negating i32::MIN must trap `integer overflow`, \
                 got stdout={:?} stderr={:?}",
            cap.stdout,
            cap.stderr
        );
    }
}

#[test]
fn test_ir_u64_column_sorted_uses_unsigned_compare() {
    // The scratch-sort compare for a u64 column must be `icmp ugt`, not
    // `sgt` (B-2026-07-07-2). Guards against a silent regression to the
    // signed key compare that would misorder values ≥ 2⁶³.
    let ir = ir_for(
        "fn main() {\n\
             \x20   let c: Column[u64] = Column.from_vec([1u64 << 63, 5u64]);\n\
             \x20   let s = c.sorted();\n\
             \x20   println(f\"{s[0]}\");\n\
             }\n",
    );
    assert!(
        ir.contains("icmp ugt"),
        "u64 column sort must key with unsigned compare:\n{ir}"
    );
}

#[test]
fn test_ir_float_narrow_emits_fptrunc() {
    // phase-8 cast slice 7 (float→float verification): narrowing via `as`
    // uses `fptrunc` (round-to-nearest-even). Widening is implicit and not
    // exercised here.
    let ir = ir_for("fn narrow(x: f64) -> f32 { x as f32 }");
    assert!(
        ir.contains("fptrunc"),
        "float narrowing should use fptrunc:\n{ir}"
    );
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
/// Twin of `tests/interpreter.rs`'s
/// `test_narrow_float_assoc_call_namespace_runs`, pinned to the same
/// string. The compiled side is the half that returned zeros, so asserting
/// it against the interpreter is what catches a silent regression here.
#[test]
fn e2e_narrow_float_assoc_call_namespace_runs() {
    let Some(out) = run_program(
        r#"fn main() {
    let n = env.args().len() as i64;
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
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
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
/// Twin of `tests/interpreter.rs`'s
/// `test_derived_display_renders_vector_and_narrow_float_fields`, pinned to
/// the same string. The compiled side is the half that refused to build and
/// then, under the obvious fix, printed a stray lane — so it is asserted
/// against the interpreter rather than only for compilation success.
#[test]
fn e2e_derived_display_renders_vector_and_narrow_float_fields() {
    let Some(out) = run_program(
        r#"#[derive(Display)]
struct WithVec { v: Vector[i32, 4], n: i64 }
#[derive(Display)]
struct WithNarrow { a: f16, b: bf16, c: f32 }
#[derive(Display)]
struct Plain { n: i64, s: String, w: u128 }

fn main() {
    let n = env.args().len() as i64;
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
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"top   WithVec { v: Vector(1, -2, 3, -4), n: 1 }
nest  [WithVec { v: Vector(1, -2, 3, -4), n: 1 }]
float WithVec { v: Vector(9, 8, 7, 6), n: 1 }
narrow WithNarrow { a: 2.5, b: 3.5, c: 4.5 }
plain Plain { n: 1, s: s, w: 340282366920938463463374607431768211455 }
"#
    );
}

#[test]
fn test_e2e_volatile_write_narrow_field_ref_param_roundtrip() {
    // B-2026-07-12-7 — `volatile_write(pw, 777)` through a `*mut i32` used
    // to store an `i64` (an integer literal defaults to i64 in codegen),
    // mismatching the `i32` pointee. Under `-O` (AOT) the 8-byte volatile
    // store and the paired 4-byte volatile load didn't forward, so a
    // same-function read-back value-numbered to the pre-write value: `karac
    // build` printed the stale `20` while `karac run`/JIT (at -O0) printed
    // the correct `777`. `run_program` builds a REAL optimized binary, so it
    // exercises the AOT path where this reproduced. Both the in-function
    // read-back (bump's return) AND the cross-function read-back (main) must
    // now be 777. The fix coerces the written value to the pointee width.
    let out = run_program(
        r#"
struct Reg { status: i32, control: i32 }
fn bump(r: mut ref Reg) -> i32 {
    // Safety: r is a live borrow of the caller's Reg.
    unsafe {
        let pw: *mut i32 = ptr.mut(r.control);
        volatile_write(pw, 777);
        let pr: *const i32 = ptr.const(r.control);
        volatile_read(pr)
    }
}
fn main() {
    let mut r: Reg = Reg { status: 10, control: 20 };
    let inside = bump(mut r);
    println(inside);
    // Safety: r is still live here.
    unsafe {
        let pr: *const i32 = ptr.const(r.control);
        println(volatile_read(pr));
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "777\n777");
    }
}

/// B-2026-07-08-10: `Vec.filled` / `Vec[v; n]` must size the buffer and stride
/// the fill by the ELEMENT type, not the compiled value width. A bare integer
/// literal compiles to i64; for a narrow element (`Vec[i32]`/`Vec[u8]`/…) the
/// old code filled at 8-byte stride while every read GEPed at the element
/// stride, mis-aligning every slot (interp `777` vs compiled `070` for
/// `Vec[i32].filled(5,7)`, silent exit-0 wrong output). Reads the
/// stride-sensitive MIDDLE slots so a stride mismatch is observable.
#[test]
fn vec_filled_narrow_element_stride() {
    let src = "fn main() {\n\
                   \x20   let a: Vec[i32] = Vec.filled(5, 7);\n\
                   \x20   println(a[1] as i64); println(a[2] as i64); println(a[3] as i64);\n\
                   \x20   let b: Vec[u8] = Vec.filled(4, 200);\n\
                   \x20   println(b[1] as i64); println(b[3] as i64);\n\
                   \x20   let c: Vec[i32] = [9; 4];\n\
                   \x20   println(c[1] as i64); println(c[3] as i64);\n\
                   }\n";
    if let Some(out) = run_program(src) {
        assert_eq!(
            out.split_whitespace().collect::<Vec<_>>(),
            ["7", "7", "7", "200", "200", "9", "9"],
            "narrow-element fill must stride by the element width; stdout:\n{out}"
        );
    }
}

/// `mut ref i32` with an UNSUFFIXED literal (`x + 1`): the borrow strip runs
/// before Q4 literal promotion, so the literal is recorded as `i32` and the
/// store-through coerces to the narrow slot width — prints 42 on both
/// surfaces (a width mismatch here would corrupt the value or fail to link).
#[test]
fn mut_ref_scalar_param_narrow_int_assign_through_e2e() {
    let src = "fn inc(x: mut ref i32) { x = x + 1; }\n\
                   fn main() {\n\
                   \x20   let mut n: i32 = 41; inc(mut n); println(n);\n\
                   }\n";
    assert_eq!(run_program(src).as_deref(), Some("42\n"));
}

/// B-2026-07-03-11 (narrow-width facet): a generic function with a
/// non-generic NARROW return type. `fn narrow[T](x: T) -> u8 { 255 }`
/// used to (a) fail module verification — the mono tail-return emitted
/// `ret i64 255` into an `i8`-returning fn (no `coerce_to_current_ret_type`
/// on the mono path) — and (b) once that's fixed, print `255u8` as `-1`,
/// because the print signedness check found no `fn_return_type_names` entry
/// for the un-declared generic fn and defaulted to signed. Both are closed:
/// the mono return coerces to the declared width, and generic fns now
/// register their concrete return-type name.
#[test]
fn e2e_generic_fn_narrow_width_return() {
    if let Some(out) = run_program(
        "fn narrow[T](x: T) -> u8 { 255 }\n\
             fn passthru[T](x: T) -> u8 { narrow(x) }\n\
             fn main() {\n\
             \x20   println(f\"{narrow(7)}\");\n\
             \x20   println(f\"{passthru(9)}\");\n\
             }",
    ) {
        assert_eq!(out, "255\n255\n");
    }
}

/// B-2026-08-14-12 — the `as` the float-narrowing diagnostic recommends
/// really does round, on this surface and under `--interp` alike.
///
/// An OVER-REACH GUARD, not a regression witness: this source compiled and
/// printed these values before the gate landed too. It is here because the
/// gate's whole value rests on the fix-it being real — a diagnostic that
/// sends the author to a spelling which changes nothing would be worse than
/// silence, since it would launder the same unrounded value behind an
/// explicit cast. Each line is the f32 (or f16) neighbour of the f64 value
/// on its left, so a cast that was a no-op would print the f64 digits.
#[test]
fn test_e2e_float_narrowing_as_cast_rounds_to_the_target_width() {
    assert_eq!(
        run_program(
            "fn takef(x: f32) -> f32 { x }\n\
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
                 }"
        )
        .as_deref(),
        Some(
            "0.1\n\
                 0.10000000149011612\n\
                 0.10000000149011612\n\
                 0.10000000149011612\n\
                 0.0999755859375\n"
        ),
    );
}

/// B-2026-08-14-4, and B-2026-08-13-22's remainder — the two shapes of the
/// same signedness question that the sibling test above does not reach.
///
/// That test's receivers are all GENERIC, so the syntactic walk resolves a
/// receiver name and then fails on the declared type (`v: T` is not a uint
/// name). These fail one step earlier: there is no name to resolve at all.
/// `v[0i64].a` is a field read whose receiver is an EXPRESSION, and the
/// walk resolves a field only through `var_type_names[receiver]`; `arr[0]`
/// on an `Array[T, N]` binding looks in `var_elem_type_exprs`, which only a
/// `Vec` binding populates. Both answered "signed" with confidence.
///
/// The direct read is the diagnostic and is why line 01 has to be here:
/// `m.a` and `v[0i64].a` are the same field of the same struct in the same
/// program, and only the second was wrong. Nothing about the Vec round-trip
/// was involved — `let e = v[0i64]; e.a` was always correct, because that
/// receiver has a name again.
///
/// Line 03's wide struct pins the boundary between the two answers: every
/// unsigned narrow field corrupts, every signed field is untouched, and the
/// `i64` field cannot be affected either way. Line 05 is the cast leg with
/// an INDEXED operand rather than the sibling test's generic-call operand.
/// Line 06's `Array[i8, 2]` and every signed value elsewhere are controls
/// that were already correct: an unconditional sign-extend is what a signed
/// source wants, which is exactly why this family reads as working until an
/// unsigned value with the high bit set goes through it.
#[test]
fn test_e2e_narrow_unsigned_survives_indexed_and_array_reads() {
    let src = r#"
struct Plain { a: u8, c: u16, e: u32 }
struct Wide { a: u8, b: i64, c: u16, d: i8, e: u32, f: i32 }
struct Boxg[T] { v: T }

fn main() {
    let m = Plain { a: 200u8, c: 60000u16, e: 4000000000u32 };
    println(f"01 {m.a} {m.c} {m.e}");
    let mut v: Vec[Plain] = Vec.new();
    v.push(m);
    println(f"02 {v[0i64].a} {v[0i64].c} {v[0i64].e}");
    let w = Wide { a: 200u8, b: -7i64, c: 60000u16, d: -8i8, e: 4000000000u32, f: -9i32 };
    let mut vw: Vec[Wide] = Vec.new();
    vw.push(w);
    println(f"03 {vw[0i64].a} {vw[0i64].b} {vw[0i64].c} {vw[0i64].d} {vw[0i64].e} {vw[0i64].f}");
    let bp: Boxg[Plain] = Boxg { v: Plain { a: 200u8, c: 60000u16, e: 4000000000u32 } };
    println(f"04 {bp.v.a}");
    println(f"05 {v[0i64].a as i64}");
    let arr: Array[u8, 2] = [200u8, 201u8];
    let arrs: Array[i8, 2] = [-56i8, -57i8];
    println(f"06 {arr[0i64]} {arr[1i64]} {arrs[0i64]}");
    let e = ref v[0i64];
    println(f"07 {e.a}");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            "01 200 60000 4000000000\n\
                 02 200 60000 4000000000\n\
                 03 200 -7 60000 -8 4000000000 -9\n\
                 04 200\n\
                 05 200\n\
                 06 200 201 -56\n\
                 07 200\n"
        ),
    );
}

/// B-2026-08-14-7, compiled twin of
/// `narrow_float_arithmetic_rounds_to_declared_width` in
/// `tests/interpreter.rs`.
///
/// That fix is interpreter-side, so nothing here changed — which is the
/// point. These are the values the compiled backends have always produced
/// and that the interpreter now agrees with; pinning them on both surfaces
/// is what makes the pair a parity guard rather than two tests that could
/// drift apart. Every line's f32/f16/bf16 source is written with an
/// explicit `as` or a suffix, so the binding really is narrow on both
/// sides and the comparison is about the OPERATORS.
#[test]
fn test_e2e_narrow_float_arithmetic_matches_interpreter() {
    let src = r#"
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
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
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
        ),
    );
}

#[test]
fn test_e2e_generic_narrow_int_method_multi_instantiation() {
    // B-2026-07-12-16 — two coupled generic-monomorphization codegen bugs.
    // GAP 1: a mono'd generic method with a by-value narrow-int type param
    // (`fn set[T](v: T)` at `T=i32`) declared an `i32` param but the call
    // site passed the arg as i64, hard-failing LLVM verification. GAP 2: the
    // generic field-return deep-clone helper was named + cached by the bare
    // param (`karac_clone_T`, last-writer-wins), so a later `Box[i32].get`
    // reused an earlier `Box[i16].get`'s i16-width clone body and truncated
    // the returned i32 to 16 bits (2000000000 -> 37888). The truncation only
    // bites when MULTIPLE instantiations coexist. Fix: coerce mono args to
    // the param widths (GAP 1); resolve the field's generic param through
    // the active monomorph substitution and gate on the concrete type so a
    // scalar field skips the clone entirely (GAP 2). Covers narrow+narrow
    // (i16/i32/i64), a bool, and a heap field (String) that must still
    // round-trip and stay leak-clean (verified separately under valgrind).
    if let Some(out) = run_program(
        "struct Box[T] { v: T }\n\
             impl[T] Box[T] {\n\
                 fn new(v: T) -> Box[T] { Box { v: v } }\n\
                 fn set(mut ref self, v: T) { self.v = v; }\n\
                 fn get(ref self) -> T { self.v }\n\
             }\n\
             fn main() {\n\
                 let mut a: Box[i16] = Box.new(1);\n\
                 let mut b: Box[i32] = Box.new(1);\n\
                 let mut c: Box[i64] = Box.new(1);\n\
                 a.set(30000); b.set(2000000000); c.set(9000000000);\n\
                 println(a.get()); println(b.get()); println(c.get());\n\
                 let mut d: Box[bool] = Box.new(false);\n\
                 d.set(true);\n\
                 println(d.get());\n\
                 let mut s: Box[String] = Box.new(f\"hello\");\n\
                 s.set(f\"world\");\n\
                 println(s.get());\n\
             }",
    ) {
        assert_eq!(out, "30000\n2000000000\n9000000000\ntrue\nworld\n");
    }
}

#[test]
fn test_e2e_iter_chain_reduce_float_and_narrow_int_payload() {
    // B-2026-07-17-11 — the synthesized `Some(<acc>)` match binding is
    // compiled without a typecheck pass, so its payload had no
    // `pattern_binding_types` entry and reconstructed via the raw-i64
    // default: a FLOAT accumulator read the payload word via `sitofp`
    // (garbage → the reduce returned the None arm, printing the
    // `unwrap_or` default), and a NARROW-INT accumulator never truncated.
    // The synthesis now registers the acc binding's surface type against a
    // unique span so the float-bitcast / int-truncate arms fire. Float max
    // reduce → 2.5; f64 sum-reduce → 4.5; direct `.max()`/`.min()` desugar
    // (which reuses this lowering) → 2.5 / 0.5.
    //
    // The `.max()` / `.min()` legs run over the total-order wrapper `F64`
    // rather than bare `f64`: B-2026-08-11-15 restored the `Ord`-element
    // gate on `Iterator.max`/`min`, so the bare-`f64` spelling this test
    // used no longer typechecks and `karac build` refuses it. The reduce
    // legs above — the row's actual subject — are untouched, and the
    // wrapper legs still reach the same synthesized-`Some(<acc>)`
    // lowering, so the coverage is unchanged. Caught by the check gate
    // (`assert_check_clean`).
    if let Some(out) = run_program(
            "fn main() {\n\
                 let f: Vec[f64] = [1.5, 2.5, 0.5];\n\
                 match f.iter().reduce(|a, x| if x > a { x } else { a }) { Some(s) => println(s), None => println(-1.0) }\n\
                 match f.iter().reduce(|a, x| a + x) { Some(s) => println(s), None => println(-1.0) }\n\
                 let w: Vec[F64] = f.iter().map(F64.from).collect();\n\
                 let hi: F64 = w.max().unwrap_or(F64.from(0.0));\n\
                 let lo: F64 = w.min().unwrap_or(F64.from(0.0));\n\
                 println(hi.value)\n\
                 println(lo.value)\n\
             }",
        ) {
            assert_eq!(out, "2.5\n4.5\n2.5\n0.5\n");
        }
}

#[test]
fn test_e2e_if_branch_untyped_int_literal_narrow_width() {
    // Regression for self-hosting blocker #7: an untyped integer literal
    // in one branch of an `if` whose result type is narrower than i64
    // (`u8`) was lowered at the default i64 width (`const_int_for_suffix`
    // keys off the suffix only), so the phi-merge saw mismatched branch
    // types (i64 vs i8) and silently fell through to a const-0 placeholder
    // — the WHOLE `if` evaluated to 0. This made the self-hosted lexer's
    // `fn peek(ref self) -> u8 { if … { 0 } else { self.bytes[i] } }`
    // always return 0, so every scan loop exited immediately. Fix:
    // `compile_if` truncates the wider (checker-verified-fitting constant)
    // branch to the narrower width before the phi.
    //
    // Covers the literal in the else branch, in the (taken) then branch,
    // and through a Vec[u8] index (the lexer's exact shape).
    if let Some(out) = run_program(
        "fn pick(c: bool, x: u8) -> u8 { if c { 0 } else { x } }\n\
             fn peek(b: ref Vec[u8], i: i64) -> u8 {\n\
                 if i >= b.len() { 0 } else { b[i] }\n\
             }\n\
             fn main() {\n\
                 let a: u8 = if false { 0 } else { 97u8 };\n\
                 println(a.to_string());            // 97\n\
                 let t: u8 = if true { 97u8 } else { 0 };\n\
                 println(t.to_string());            // 97 (taken branch typed)\n\
                 println(pick(false, 5u8).to_string()); // 5\n\
                 let mut v: Vec[u8] = Vec.new();\n\
                 v.push(65u8); v.push(66u8);\n\
                 println(peek(v, 0).to_string());   // 65 (was 0)\n\
                 println(peek(v, 9).to_string());   // 0  (oob -> the literal arm)\n\
             }",
    ) {
        assert_eq!(out, "97\n97\n5\n65\n0\n");
    }
}

#[test]
fn test_e2e_narrow_int_arith_branch_phi_width() {
    // Regression for the narrow-int-arithmetic sibling of #7's literal case
    // (kata #125 valid-palindrome's `to_lower` / ASCII case-fold surface).
    // `compile_narrow_int_binop` range-checks `b + 32u8` to the declared u8
    // width but leaves the VALUE at the i64 it computes at (boundary
    // coercion narrows it later). When that wide branch sits beside a bare
    // narrow branch in a value `if`/`match`/`if let`, the phi-merge saw
    // mismatched widths (i64 vs i8) and fell through to the const-0
    // placeholder — the WHOLE construct returned 0. The earlier
    // `is_const()`-gated width fix only handled the literal case; this asserts
    // the runtime-wide branch is truncated in its predecessor too, across all
    // three merge sites.
    if let Some(out) = run_program(
        // if/else: arith then-branch, plain else-branch (and the mirror)
        "fn to_lower(b: u8) -> u8 {\n\
                 if b >= b'A' and b <= b'Z' { b + (b'a' - b'A') } else { b }\n\
             }\n\
             fn to_upper(b: u8) -> u8 {\n\
                 if b >= b'a' and b <= b'z' { b } else { b - 0u8 }\n\
             }\n\
             // match: mixed arith / plain / arith arms\n\
             fn fold_match(b: u8) -> u8 {\n\
                 match b {\n\
                     b'A' => b + 32u8,\n\
                     b'B' => b,\n\
                     _ => b + 1u8,\n\
                 }\n\
             }\n\
             // if let: arith Some-arm, literal None-arm\n\
             fn opt_fold(o: Option[u8]) -> u8 {\n\
                 if let Some(x) = o { x + 10u8 } else { 0u8 }\n\
             }\n\
             fn main() {\n\
                 println(to_lower(b'A').to_string()); // 97  (was 0)\n\
                 println(to_lower(b'e').to_string()); // 101 (was 0, else branch)\n\
                 println(to_upper(b'b').to_string()); // 98\n\
                 println(fold_match(b'A').to_string()); // 97 (was 0)\n\
                 println(fold_match(b'B').to_string()); // 66 (plain arm)\n\
                 println(fold_match(b'z').to_string()); // 123 (wildcard arm)\n\
                 println(opt_fold(Some(5u8)).to_string()); // 15 (was 0)\n\
                 println(opt_fold(None).to_string());      // 0\n\
             }",
    ) {
        assert_eq!(out, "97\n101\n98\n97\n66\n123\n15\n0\n");
    }
}

#[test]
fn test_e2e_option_narrow_int_payload_narrows_to_surface_width() {
    // Regression: `Vec[u8].pop()` returns `Option[u8]`, whose payload
    // word is i64 in the variant word stream. The `Some(top)` binding
    // must narrow that word back to i8 (u8's LLVM width) — otherwise
    // `top == b` (b: u8 → i8) emits `icmp i64, i8` and module
    // verification fails with "Both operands to ICmp instruction are
    // not of the same type!". Exercises the valid-parentheses stack
    // shape (LeetCode #20) that surfaced the bug: push the matching
    // closer, pop + compare on a closer. Also covers `char` (i32) and
    // `u32` to confirm the narrowing keys off the recorded surface
    // width, not just u8.
    let out = run_program(
        r#"
fn is_valid(s: ref String) -> bool {
    let bytes = s.bytes();
    let n = bytes.len();
    let mut stack: Vec[u8] = Vec.new();
    let mut i = 0i64;
    while i < n {
        let b = bytes[i];
        if b == b'(' or b == b'[' or b == b'{' {
            if b == b'(' { stack.push(b')'); }
            else if b == b'[' { stack.push(b']'); }
            else { stack.push(b'}'); }
        } else {
            match stack.pop() {
                Some(top) => { if top != b { return false; } }
                None => { return false; }
            }
        }
        i = i + 1i64;
    }
    stack.is_empty()
}

fn main() {
    if is_valid("([{}])") { println(1); } else { println(0); }
    if is_valid("(]") { println(1); } else { println(0); }

    // char payload (i32) narrowed from the i64 word.
    let mut cs: Vec[char] = Vec.new();
    cs.push('x');
    match cs.pop() {
        Some(c) => { if c == 'x' { println(2); } else { println(0); } }
        None => { println(0); }
    }

    // u32 payload narrowed from the i64 word.
    let mut ns: Vec[u32] = Vec.new();
    ns.push(7u32);
    if let Some(v) = ns.pop() {
        let w: u32 = 7u32;
        if v == w { println(3); }
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "0", "2", "3"]);
    }
}

#[test]
fn test_e2e_narrow_int_width_semantics() {
    // Narrow ints (u8/i8/u16/i16/u32/i32) are real fixed-width types:
    // arithmetic computes at i64 (so a u8 buffer element and a u8 local
    // agree with the interpreter) but the result is bound to the declared
    // width. B-2026-06-08-1 slice 2.
    // (a) A u8-element sum that FITS the width prints the true value —
    //     pre-fix this wrapped at i8 and printed -61.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let s = \"ab\";\n\
                 let b = s.bytes();\n\
                 println(b[0] + b[1]);\n\
             }",
    ) {
        assert_eq!(out, "195\n"); // 97 + 98, fits u8
    }
    // (b) A `mut i32` accumulated across an assignment inside a function
    //     returns correctly — pre-fix the i64 result stored into the i32
    //     slot read back as 0.
    if let Some(out) = run_program(
        "fn build() -> i32 { let mut r: i32 = 0i32; r = r + 21i32; r }\n\
             fn main() { println(build()); }",
    ) {
        assert_eq!(out, "21\n");
    }
}

#[test]
fn test_e2e_narrow_int_overflow_traps() {
    // `u8 200 + u8 100 = 300` overflows the width and traps `integer
    // overflow` (design.md § Integer overflow), matching the interpreter.
    // Vars (not literals) so it isn't const-folded.
    let captured =
        run_program_capturing("fn main() { let a: u8 = 200; let b: u8 = 100; println(a + b); }");
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("integer overflow"),
            "expected u8-overflow trap, got stdout={:?}",
            c.stdout
        );
    }
}

/// B-2026-08-05-38 — the narrow-width trap is now ELIDED for `/` and `%`,
/// whose results provably fit. These are the two boundary cases the
/// elision must NOT swallow, plus the value parity it must not disturb.
///
/// `%` is elided for every divisor (a remainder's magnitude never exceeds
/// its dividend, which already fits the width) and `/` only for a CONSTANT
/// divisor that is not `-1`. `INT_MIN / -1` is the sole division that
/// overflows in two's complement, so both spellings of it must still trap:
/// a runtime `-1` (which the rule cannot prove anything about) and a
/// constant `-1` (which the rule explicitly excludes).
///
/// A regression that dropped either exclusion would be SILENT — the
/// program would print `-2147483648` instead of faulting — which is why
/// this asserts the trap fires rather than just that the elision happened.
#[test]
fn test_e2e_narrow_div_int_min_by_neg_one_still_traps() {
    // (a) RUNTIME -1 divisor: not a constant, so the check is kept.
    let captured = run_program_capturing(
        "fn main() {\n\
             \x20   let n: i64 = env.args().len();\n\
             \x20   let lo: i32 = -2147483648i32;\n\
             \x20   let d: i32 = 0i32 - (n as i32);\n\
             \x20   println(lo / d);\n\
             }",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("integer overflow"),
            "runtime -1 divisor must still trap, got stdout={:?}",
            c.stdout
        );
        assert!(
            !c.stdout.contains("-2147483648"),
            "the overflowing quotient must not print, got stdout={:?}",
            c.stdout
        );
    }
    // (b) CONSTANT -1 divisor: the rule excludes exactly this value.
    let captured = run_program_capturing(
        "fn main() {\n\
             \x20   let n: i64 = env.args().len();\n\
             \x20   let lo: i32 = -2147483648i32 + ((n as i32) - (n as i32));\n\
             \x20   println(lo / (0i32 - 1i32));\n\
             }",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("integer overflow"),
            "constant -1 divisor must still trap, got stdout={:?}",
            c.stdout
        );
    }
}

#[test]
fn test_e2e_narrow_div_mod_values_unchanged_by_trap_elision() {
    // The value half: `/` and `%` must keep computing exactly what they
    // did with the check in place, including at `INT_MIN` and across the
    // sign boundary (a remainder carries the DIVIDEND's sign, so the
    // negative rows are the ones a mis-elision would disturb).
    if let Some(out) = run_program(
        "fn main() {\n\
             \x20   let n: i64 = env.args().len();\n\
             \x20   let x: i32 = -2147483648i32 + ((n as i32) - (n as i32));\n\
             \x20   println(x % 10i32);\n\
             \x20   println(x / 10i32);\n\
             \x20   println((0i32 - 37i32) % 10i32);\n\
             \x20   println((0i32 - 37i32) / 10i32);\n\
             \x20   println(37i32 % 10i32);\n\
             \x20   println(37i32 / 10i32);\n\
             }",
    ) {
        assert_eq!(out, "-8\n-214748364\n-7\n-3\n7\n3\n");
    }
}

#[test]
fn test_e2e_fstring_narrow_signed_int_negative() {
    // Pre-fix the f-string integer arm passed raw i32 to snprintf("%lld"),
    // which printf read as 64 bits — LLVM zero-padded the upper 32 bits
    // before the call, so a negative i32 like -123 printed as the
    // unsigned reinterpretation 4294967173. Regression for the
    // sign-extend fix in `compile_fstr_part_to_cstr`.
    let out = run_program(
        r#"
fn main() {
    let x: i32 = -123i32;
    println(f"x={x}");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "x=-123");
    }
}

#[test]
fn test_e2e_fstring_narrow_unsigned_int_large() {
    // Companion to the signed-negative regression: a u32 with the high
    // bit set must print as the unsigned value (zext + %llu), not as a
    // signed reinterpretation.
    let out = run_program(
        r#"
fn main() {
    let x: u32 = 4000000000u32;
    println(f"x={x}");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "x=4000000000");
    }
}

#[test]
fn test_e2e_enum_payload_bool_narrowing() {
    // `match Json.Bool(b) => b` from a function returning `bool` —
    // the variant-payload word is i64 in the word stream but the
    // binding's surface type is bool, so codegen needs to insert a
    // `trunc i64 → i1` before binding. Without this, the LLVM
    // verifier rejects `ret i64 0` from a `fn -> bool` arm body.
    // Surfaced by the backend TODO API kata's `extract_completed`
    // helper at `kara-katas/backend/todo-api/main.kara`.
    let output = run_program(
        "fn extract(j: Json) -> bool {\n\
                 match j {\n\
                     Json.Bool(b) => b,\n\
                     _ => false,\n\
                 }\n\
             }\n\
             fn main() {\n\
                 println(extract(Json.Bool(true)));\n\
                 println(extract(Json.Bool(false)));\n\
                 println(extract(Json.Number(42.0)));\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "true\nfalse\nfalse\n");
}

#[test]
fn test_e2e_narrow_literal_vec_packs_at_annotated_width() {
    // B-2026-07-02-6: `let mut v: Vec[i32] = [10, 20, 30]` stored
    // i64-PACKED data behind an i32-typed binding (the literal compiler
    // derived the element type from the first item — int literals are
    // i64) — every read reinterpreted bytes at the i32 stride: `v[2]`
    // returned element 1's low half, iteration summed garbage, and
    // `Column.from_vec`'s memcpy propagated the mispacking
    // (`Column[i32].sum()` summed ceil(bytes/8) elements — 30 instead
    // of 60). The literal compilers now coerce items to the annotated
    // narrow width via the pending-let element hint.
    let src = r#"
fn main() {
    let mut v: Vec[i32] = [10, 20, 30];
    println(v[0]);
    println(v[2]);
    v.push(40);
    println(v[3]);
    let mut total = 0;
    for x in v {
        total = total + (x as i64);
    }
    println(total);
    let c: Column[i32] = Column.from_vec([10, 20, 30]);
    println(c.sum());
    let e: Column[i8] = Column.from_vec([1, 2, 3]);
    println(e.sum());
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "10\n30\n40\n100\n60\n6\n");
}

#[test]
fn test_e2e_column_string_index_and_user_impl() {
    // S6c-12 Slice 5: heap/String element read-back. `Column[String]`
    // indexing `c[i] -> Option[String]` under `karac build` (formerly a
    // loud "not yet supported" error) deep-clones the element so the
    // returned Option owns an independent heap. Covers the direct `c[i]`
    // form AND `self[i]` inside a user `impl … for Column[String]` (the
    // Slice 5 headline — the SelfValue index receiver now routes to the
    // column path). A null slot yields `None`.
    let src = r#"
trait Pick { fn at(ref self, i: i64) -> String; }
impl Pick for Column[String] {
    fn at(ref self, i: i64) -> String { self[i].unwrap() }
}
fn main() {
    let v: Vec[String] = ["alpha", "beta", "gamma"];
    let c: Column[String] = Column.from_vec(v);
    println(f"{c[1].unwrap()}");
    println(f"{c.at(0)} {c.at(2)}");
    match c[2] {
        Some(s) => { println(f"some {s}"); }
        None => { println("none"); }
    }
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "beta\nalpha gamma\nsome gamma\n");
}

#[test]
fn test_e2e_column_from_vec_temp_string_move() {
    // B-2026-07-06-1: `Column.from_vec(<temporary Vec[String]>)` under
    // `karac build` (formerly a loud "not yet supported" error) MOVES the
    // source's String structs into the column and frees only the source's
    // outer buffer — POD/String parity with the already-working i64 temp.
    // Covers BOTH temp shapes — an inline array literal and a function-call
    // result — with element read-back to confirm the moved heaps are intact.
    // Matches `karac run`; memory-safety is covered by
    // `asan_column_from_vec_temp_string_move_no_leak`.
    let src = r#"
fn mk() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("delta".to_string());
    v.push("echo".to_string());
    v
}
fn main() {
    let c: Column[String] = Column.from_vec(["alpha".to_string(), "beta".to_string(), "gamma".to_string()]);
    let d: Column[String] = Column.from_vec(mk());
    println(f"{c.len()} {d.len()}");
    println(f"{c[0].unwrap()} {c[2].unwrap()}");
    println(f"{d[1].unwrap()}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "3 2\nalpha gamma\necho\n");
}

#[test]
fn test_e2e_column_prod() {
    // `Column.prod()` — product of the valid slots, closing the reduction
    // parity gap with `Tensor.prod` (Column had `sum` but no `prod`). A new
    // `#[compiler_builtin]` inherent method folding `*` over the valid slots
    // via the shared `emit_reduce_fold` kernel (`ReduceOp::Prod`, seeded with
    // the multiplicative identity `1`), mirroring `sum`. Traps on an empty /
    // all-null column exactly as `sum`/`min`/`max` do. i64 (checked mul) and
    // f64 (fmul); `run` == `build`.
    let src = r#"
fn main() {
    let ci: Column[i64] = Column.from_vec([2, 3, 4]);
    let cf: Column[f64] = Column.from_vec([1.5, 2.0, 4.0]);
    println(f"{ci.prod()}");
    println(f"{cf.prod()}");
}
"#;
    // 2*3*4 = 24; 1.5*2*4 = 12.
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "24\n12\n");
}

#[test]
fn test_e2e_column_fold() {
    // `Column.fold[A](init, |acc, x| ...)` — the general left-fold
    // primitive the fixed reductions specialize. The closure body is
    // INLINED into an in-place reduction loop over the valid slots (nulls
    // skipped, in order); `A` is the accumulator's type. Covers: a sum
    // (`+`), a product (`*`, seed 1), a predicate count, a sum-of-squares,
    // an f64 accumulator, an outer-variable capture, an empty column
    // (returns `init` — the fold identity, NO trap), and param shadowing
    // (an outer `a` is restored after the fold). `run` == `build` ==
    // default auto-par.
    let src = r#"
fn main() {
    let c: Column[i64] = Column.from_vec([1, 2, 3, 4, 5]);
    println(f"{c.fold(0, |a, x| a + x)}");
    println(f"{c.fold(1, |a, x| a * x)}");
    println(f"{c.fold(0, |a, x| if x > 2 { a + 1 } else { a })}");
    println(f"{c.fold(0, |a, x| a + x * x)}");
    let cf: Column[f64] = Column.from_vec([1.5, 2.5, 4.0]);
    println(f"{cf.fold(0.0, |a, x| a + x)}");
    let k: i64 = 10;
    println(f"{c.fold(0, |a, x| a + x + k)}");
    let e: Column[i64] = Column.from_vec([]);
    println(f"{e.fold(99, |a, x| a + x)}");
    let a: i64 = 7;
    let s = c.fold(0, |a, x| a + x);
    println(f"{s} {a}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "15\n120\n3\n55\n8\n65\n99\n15 7\n");
}

#[test]
fn test_e2e_column_fold_with_nulls_skips_them() {
    // `fold` skips null slots (the SQL/pandas aggregate posture shared with
    // `sum`/`min`/`max`) — the validity bitmap gates the per-slot apply.
    let src = r#"
fn main() {
    let mut c: Column[i64] = Column.new();
    c.push(10);
    c.push_null();
    c.push(20);
    c.push_null();
    c.push(30);
    println(f"{c.fold(0, |a, x| a + x)}");
    println(f"{c.fold(0, |a, x| a + 1)}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // 10+20+30 = 60; 3 valid slots.
    assert_eq!(out, "60\n3\n");
}

#[test]
fn test_e2e_column_map() {
    // `Column.map(|x| ...) -> Column[T]` — element-wise map producing a
    // fresh column. Covers i64 doubling, an f64 body, a captured outer
    // variable, and param shadowing. `run` == `build` == default auto-par.
    let src = r#"
fn main() {
    let c: Column[i64] = Column.from_vec([1, 2, 3, 4]);
    let d = c.map(|x| x * 2);
    println(f"{d.sum()}");
    let cf: Column[f64] = Column.from_vec([1.5, 2.5, 4.0]);
    let df = cf.map(|x| x + 0.5);
    println(f"{df.sum()}");
    let k: i64 = 100;
    let e = c.map(|x| x + k);
    println(f"{e.sum()}");
    let x: i64 = 7;
    let g = c.map(|x| x * x);
    println(f"{g.sum()} {x}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // sum([2,4,6,8])=20; sum([2.0,3.0,4.5])=9.5; sum([101,102,103,104])=410;
    // sum([1,4,9,16])=30, outer x untouched = 7.
    assert_eq!(out, "20\n9.5\n410\n30 7\n");
}

#[test]
fn test_e2e_column_map_preserves_nulls() {
    // The result column carries the source validity bitmap: null slots pass
    // through (not computed), the mapped values fill the valid slots.
    let src = r#"
fn main() {
    let mut c: Column[i64] = Column.new();
    c.push(10);
    c.push_null();
    c.push(20);
    let d = c.map(|x| x * 2);
    println(f"{d.sum()}");
    println(f"{d.valid_count()}");
    println(f"{d.len()}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // 20+40 = 60 over the two valid slots; 2 valid; length 3 preserved.
    assert_eq!(out, "60\n2\n3\n");
}

#[test]
fn test_e2e_column_zip_with() {
    // `Column.zip_with(other, |a, b| ...)` — element-wise combine of two
    // same-length columns through the inline closure, yielding a fresh
    // Column. Result validity = AND of the two bitmaps (null propagation);
    // the closure runs only where both are valid. The binary form of
    // `MapKernelOp::Closure` (`MapOther::Access`). `run` == `build` ==
    // default auto-par.
    let src = r#"
fn main() {
    let a: Column[i64] = Column.from_vec([1, 2, 3, 4]);
    let b: Column[i64] = Column.from_vec([10, 20, 30, 40]);
    let s = a.zip_with(b, |x, y| x + y);
    println(f"{s.sum()}");
    let c: Column[i64] = Column.from_vec([5, 1, 8]);
    let d: Column[i64] = Column.from_vec([2, 9, 3]);
    let m = c.zip_with(d, |x, y| if x > y { x } else { y });
    println(f"{m.sum()}");
    let f1: Column[f64] = Column.from_vec([1.5, 2.5]);
    let f2: Column[f64] = Column.from_vec([0.5, 0.5]);
    let fz = f1.zip_with(f2, |x, y| x + y);
    println(f"{fz.sum()}");
    let k: i64 = 100;
    let cz = a.zip_with(b, |x, y| x + y + k);
    println(f"{cz.sum()}");
}
"#;
    // 110; max=5+9+8=22; 2.0+3.0=5; 110+400=510.
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "110\n22\n5\n510\n");
}

#[test]
fn test_e2e_column_zip_with_propagates_nulls() {
    // A null on EITHER side yields a null result (bitmap AND); the closure
    // is not called there.
    let src = r#"
fn main() {
    let mut a: Column[i64] = Column.new();
    a.push(1);
    a.push_null();
    a.push(3);
    let mut b: Column[i64] = Column.new();
    b.push(10);
    b.push(20);
    b.push_null();
    let z = a.zip_with(b, |x, y| x + y);
    println(f"{z.sum()}");
    println(f"{z.valid_count()}");
}
"#;
    // Only slot 0 valid: 1+10 = 11; one valid slot.
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "11\n1\n");
}

#[test]
fn test_e2e_column_argmin_argmax() {
    // `Column.argmin()`/`argmax() -> Option[i64]` (ElementwiseOrd, S6c): the
    // ORIGINAL slot index of the first min/max over the valid slots; null
    // slots skipped in the compare but the reported index is the original
    // position (`Series.idxmin`). Covers ties (first occurrence wins), an
    // f64 column, null-skipping, and the all-null/empty `None`. `run` ==
    // `build` == default auto-par.
    let src = r#"
fn show(o: Option[i64]) {
    match o {
        Some(i) => println(f"{i}"),
        None => println("none"),
    }
}
fn main() {
    let c: Column[i64] = Column.from_vec([5, 9, 3, 3, 8, 1]);
    show(c.argmin());
    show(c.argmax());
    let fc: Column[f64] = Column.from_vec([2.5, 1.5, 3.5, 1.5]);
    show(fc.argmin());
    show(fc.argmax());
    let mut n: Column[i64] = Column.new();
    n.push(10);
    n.push_null();
    n.push(5);
    n.push_null();
    n.push(20);
    show(n.argmin());
    show(n.argmax());
    let mut allnull: Column[i64] = Column.with_capacity(2);
    allnull.push_null();
    allnull.push_null();
    show(allnull.argmin());
    let empty: Column[i64] = Column.with_capacity(0);
    show(empty.argmax());
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // c: min value 1 at idx 5, max value 9 at idx 1.
    // fc: min 1.5 first at idx 1, max 3.5 at idx 2.
    // n [10,null,5,null,20]: min 5 at idx 2, max 20 at idx 4 (original slots).
    // all-null and empty -> none.
    assert_eq!(out, "5\n1\n1\n2\n2\n4\nnone\nnone\n");
}

#[test]
fn test_e2e_column_sorted_argsort() {
    // `Column.sorted() -> Vec[T]` / `argsort() -> Vec[i64]` (ElementwiseOrd,
    // S6c) over the VALID slots. `sorted` drops nulls (result length is the
    // valid count); `argsort` reports the ORIGINAL slot indices ordered so
    // the values ascend (stable — ties keep ascending index order). i64 and
    // f64 elements. Results are `let`-bound then indexed (the established
    // `Stats.sort` idiom). `run` == `build` == default auto-par.
    let src = r#"
fn main() {
    let c: Column[i64] = Column.from_vec([5, 9, 3, 3, 8, 1]);
    let cs: Vec[i64] = c.sorted();
    println(f"{cs[0]} {cs[1]} {cs[2]} {cs[3]} {cs[4]} {cs[5]}");
    let ca: Vec[i64] = c.argsort();
    println(f"{ca[0]} {ca[1]} {ca[2]} {ca[3]} {ca[4]} {ca[5]}");
    let mut n: Column[i64] = Column.with_capacity(5);
    n.push(10); n.push_null(); n.push(5); n.push_null(); n.push(20);
    let ns: Vec[i64] = n.sorted();
    println(f"{ns.len()} {ns[0]} {ns[1]} {ns[2]}");
    let na: Vec[i64] = n.argsort();
    println(f"{na.len()} {na[0]} {na[1]} {na[2]}");
    let fc: Column[f64] = Column.from_vec([2.5, 1.5, 3.5, 1.5]);
    let fs: Vec[f64] = fc.sorted();
    println(f"{fs[0]} {fs[1]} {fs[2]} {fs[3]}");
    let fa: Vec[i64] = fc.argsort();
    println(f"{fa[0]} {fa[1]} {fa[2]} {fa[3]}");
    let ec: Column[i64] = Column.with_capacity(0);
    let es: Vec[i64] = ec.sorted();
    println(f"{es.len()}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // c sorted [1,3,3,5,8,9]; argsort [5,2,3,0,4,1].
    // n [10,null,5,null,20] valid-only sorted [5,10,20]; argsort original
    //   slots [2,0,4]. fc sorted [1.5,1.5,2.5,3.5]; argsort stable [1,3,0,2].
    // empty column sorted -> length 0.
    assert_eq!(
        out,
        "1 3 3 5 8 9\n5 2 3 0 4 1\n3 5 10 20\n3 2 0 4\n1.5 1.5 2.5 3.5\n1 3 0 2\n0\n"
    );
}

#[test]
fn test_e2e_column_sorted_argsort_narrow_widths() {
    // S6c follow-on: `Column.sorted`/`argsort` beyond i64/f64. Columns store
    // elements at their native width, so the widened 8-byte scratch sort
    // lifts every numeric type — `i8`/`i16`/`i32` sext, `u8`/`u16`/`u32`
    // zext, `f32` fpext into the key, then `sorted` narrows the key back to
    // `Vec[T]`. Covers i32 (with a null), u32, and f32 (with a null);
    // `run` == `build` == default auto-par. (u64 also sorts now, via the
    // unsigned scratch compare — B-2026-07-07-2.)
    let src = r#"
fn main() {
    let mut ci: Column[i32] = Column.with_capacity(4);
    ci.push(5); ci.push(1); ci.push_null(); ci.push(3);
    let cs: Vec[i32] = ci.sorted();
    println(f"{cs.len()} {cs[0]} {cs[1]} {cs[2]}");
    let ca: Vec[i64] = ci.argsort();
    println(f"{ca[0]} {ca[1]} {ca[2]}");
    let cu: Column[u32] = Column.from_vec([30, 10, 20]);
    let us: Vec[u32] = cu.sorted();
    println(f"{us[0]} {us[1]} {us[2]}");
    let ua: Vec[i64] = cu.argsort();
    println(f"{ua[0]} {ua[1]} {ua[2]}");
    let mut cf: Column[f32] = Column.with_capacity(4);
    cf.push(2.5); cf.push_null(); cf.push(1.5); cf.push(0.5);
    let fs: Vec[f32] = cf.sorted();
    println(f"{fs[0]} {fs[1]} {fs[2]}");
    let fa: Vec[i64] = cf.argsort();
    println(f"{fa[0]} {fa[1]} {fa[2]}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // i32 [5,1,null,3] valid sorted [1,3,5]; argsort original slots [1,3,0].
    // u32 [30,10,20] sorted [10,20,30]; argsort [1,2,0].
    // f32 [2.5,null,1.5,0.5] valid sorted [0.5,1.5,2.5]; argsort [3,2,0].
    assert_eq!(out, "3 1 3 5\n1 3 0\n10 20 30\n1 2 0\n0.5 1.5 2.5\n3 2 0\n");
}

#[test]
fn test_e2e_narrow_literal_all_sinks_pack_contextual_width() {
    // B-2026-07-02-6 general fix: the typechecker re-records a collection
    // literal admitted against a scalar-element Vec/Slice/ref-Vec context
    // at its CONTEXTUAL type, and codegen's literal compilers read that
    // span record — so narrow packing holds at EVERY sink, not just
    // annotated lets: by-value fn args, `ref` args, `Slice[T]` params,
    // return position, struct fields, method args, int→float element
    // coercion (sitofp, not bit-landing), and bare `[v; n]` repeat
    // literals in arg position (pre-fix those failed module verification
    // against the Vec ABI).
    let src = r#"
fn total(v: Vec[i32]) -> i64 {
    let mut t = 0;
    for x in v {
        t = t + (x as i64);
    }
    return t;
}

fn total_ref(v: ref Vec[i32]) -> i64 {
    let mut t = 0;
    for x in v {
        t = t + (x as i64);
    }
    return t;
}

fn total_slice(v: Slice[i32]) -> i64 {
    let mut t = 0;
    for x in v {
        t = t + (x as i64);
    }
    return t;
}

fn make() -> Vec[i32] {
    return [10, 20, 30];
}

struct Holder {
    v: Vec[i32],
}

impl Holder {
    fn tally(self, extra: Vec[i32]) -> i64 {
        let mut t = 0;
        for x in self.v {
            t = t + (x as i64);
        }
        for x in extra {
            t = t + (x as i64);
        }
        return t;
    }
}

fn main() {
    println(total([10, 20, 30]));
    println(total_ref([10, 20, 30]));
    println(total_slice([10, 20, 30]));
    println(total([7; 3]));
    let m = make();
    println(m[2]);
    let h = Holder { v: [1, 2, 3] };
    println(h.tally([4, 5, 6]));
    let f: Vec[f64] = [1, 2, 3];
    println(f[1]);
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "60\n60\n60\n21\n30\n21\n2\n");
}

#[test]
fn test_e2e_narrow_column_elemwise_overflow_traps() {
    // B-2026-07-01-3 (codegen half, already correct — pinned):
    // `Column[i32]` element-wise `+ 1` on INT_MAX traps at the element
    // width.
    let src = r#"
fn main() {
    let c: Column[i32] = Column.from_vec([2147483647]);
    let d = c + 1;
    match d[0] {
        Some(x) => { println(x); },
        None => { println("null"); },
    }
}
"#;
    let captured = run_program_capturing(src).expect("program should compile");
    let combined = format!("{}{}", captured.stdout, captured.stderr);
    assert!(
        combined.contains("integer overflow"),
        "expected the i32 element-wise overflow trap; stdout={:?} stderr={:?}",
        captured.stdout,
        captured.stderr
    );
}

#[test]
fn test_e2e_main_exitcode_direct_constructor_narrows_to_i32() {
    // `ExitCode(7)` — the bare distinct constructor over an `i32` base.
    // The default-`i64` literal must narrow to the `i32` base width so
    // it round-trips through the C-entry `i32 main()` signature.
    let cap = run_program_capturing(
        r#"
fn main() -> ExitCode {
    ExitCode(7)
}
"#,
    );
    if let Some(cap) = cap {
        assert_eq!(cap.status.code(), Some(7), "stderr={:?}", cap.stderr);
    }
}

#[test]
fn test_vector_shuffle_narrowing_float() {
    // Narrow a 4-lane f64 source to a 2-lane result (float lane type).
    let out = run_program(
        r#"
fn main() {
    let a = Vector[f64, 4](1.5, 2.5, 3.5, 4.5);
    let r = a.shuffle([3, 0]);
    println(r[0]); println(r[1]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "4.5\n1.5\n");
    }
}

// ── Column[T] codegen — Arrow-buffer layout (phase-11 Q5) ────────
// `Column[T]` lowers to a single pointer to a malloc'd control block
// `{ data, null_bitmap, len, capacity }` (design.md § Column): a
// contiguous values buffer + a bit-packed validity bitmap (1 bit/elem,
// 1 = valid, 0 = SQL null). Core slice: constructors (new /
// with_capacity / from_vec), push / push_null, the null accessors
// (len / null_count / valid_count / is_null), and `c[i] -> Option[T]`
// indexing. AOT output is verified byte-identical to `karac run`.

#[test]
fn test_e2e_column_constructors_push_accessors() {
    // new() + push/push_null, then every null accessor and Option
    // indexing (Some for a valid slot, None for a null).
    let out = run_program(
        "fn main() {\n\
                 let mut c: Column[i64] = Column.new();\n\
                 c.push(10);\n\
                 c.push(20);\n\
                 c.push_null();\n\
                 c.push(40);\n\
                 println(c.len());\n\
                 println(c.null_count());\n\
                 println(c.valid_count());\n\
                 println(c.is_null(2));\n\
                 println(c.is_null(0));\n\
                 match c[1] { Some(v) => println(v), None => println(-1) }\n\
                 match c[2] { Some(v) => println(v), None => println(-1) }\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "4\n1\n3\ntrue\nfalse\n20\n-1\n",
            "Column accessors + Option indexing must match the interpreter"
        );
    }
}

#[test]
fn test_e2e_column_from_vec_and_with_capacity() {
    // from_vec (all-valid, deep-copies the buffer) + with_capacity
    // (capacity hint honored; len starts 0). f64 + bool element types.
    let out = run_program(
        "fn main() {\n\
                 let d: Column[i64] = Column.from_vec([1, 2, 3]);\n\
                 println(d.len());\n\
                 println(d.null_count());\n\
                 match d[0] { Some(x) => println(x), None => println(-9) }\n\
                 let mut f: Column[f64] = Column.with_capacity(2);\n\
                 f.push(1.5);\n\
                 f.push_null();\n\
                 f.push(2.5);\n\
                 println(f.null_count());\n\
                 match f[2] { Some(x) => println(x), None => println(0.0) }\n\
                 let mut b: Column[bool] = Column.new();\n\
                 b.push(true);\n\
                 b.push(false);\n\
                 match b[1] { Some(x) => println(x), None => println(true) }\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "3\n0\n1\n1\n2.5\nfalse\n",
            "Column.from_vec / with_capacity across i64/f64/bool must match the interpreter"
        );
    }
}

#[test]
fn test_e2e_column_fn_boundary_and_move() {
    // By-value fn-boundary move (owned arg) + `let k = h;` move
    // (source slot nulled). Both consumers own the control block; the
    // ASAN lifecycle test pins no double-free.
    let out = run_program(
        "fn take(c: Column[f64]) -> i64 { c.len() }\n\
             fn main() {\n\
                 let g: Column[f64] = Column.from_vec([3.0, 4.0, 5.0]);\n\
                 println(take(g));\n\
                 let h: Column[i64] = Column.from_vec([7, 8]);\n\
                 let k = h;\n\
                 println(k.len());\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "3\n2\n",
            "Column fn-boundary + let-rebind moves must match"
        );
    }
}

#[test]
fn test_e2e_column_index_oob_panics() {
    // Out-of-range index traps (matches the interpreter's runtime OOB
    // trap); an in-bounds-but-null slot is `None`, not a trap.
    let captured = run_program_capturing(
        "fn main() {\n\
                 let c: Column[i64] = Column.from_vec([1, 2]);\n\
                 let i = 9;\n\
                 match c[i] { Some(v) => println(v), None => println(-1) }\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("Column index out of bounds"),
            "expected the runtime index bounds trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_e2e_column_fillna_dropna() {
    // fillna replaces null slots with the value (fresh all-valid
    // column, receiver unchanged); dropna drops them. Both return a
    // fresh owned Column tracked by `FreeColumn`.
    let out = run_program(
        "fn main() {\n\
                 let mut c: Column[i64] = Column.new();\n\
                 c.push(10);\n\
                 c.push_null();\n\
                 c.push(30);\n\
                 c.push_null();\n\
                 let f = c.fillna(99);\n\
                 println(f.len());\n\
                 println(f.null_count());\n\
                 match f[1] { Some(v) => println(v), None => println(-1) }\n\
                 match f[2] { Some(v) => println(v), None => println(-1) }\n\
                 let d = c.dropna();\n\
                 println(d.len());\n\
                 println(d.null_count());\n\
                 match d[0] { Some(v) => println(v), None => println(-1) }\n\
                 match d[1] { Some(v) => println(v), None => println(-1) }\n\
                 println(c.null_count());\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "4\n0\n99\n30\n2\n0\n10\n30\n2\n",
            "Column.fillna / dropna must match the interpreter (receiver unchanged)"
        );
    }
}

#[test]
fn test_e2e_column_fillna_treat_nan_as_null() {
    // `treat_nan_as_null` normalizes a float column's bitmap-valid NaN
    // slots into fills (design.md § Data types). Column [1.5, null, NaN,
    // 4.0]: bare fillna fills only the null slot (NaN kept); the labeled
    // and positional flag forms additionally fill the NaN slot. Receiver
    // unchanged. Byte-identical to `karac run`.
    let out = run_program(
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
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "0\n0\nNaN\n0\n0\n7\n1\n",
            "Column.fillna treat_nan_as_null must normalize NaN only when set"
        );
    }
}

#[test]
fn test_e2e_column_from_iter_nullable() {
    // Vec[Option[T]] -> Column[T]: Some -> valid slot, None -> SQL null.
    let out = run_program(
        "fn main() {\n\
                 let opts: Vec[Option[i64]] = [Some(1), None, Some(3)];\n\
                 let e: Column[i64] = Column.from_iter_nullable(opts);\n\
                 println(e.len());\n\
                 println(e.null_count());\n\
                 println(e.is_null(1));\n\
                 match e[0] { Some(v) => println(v), None => println(-1) }\n\
                 match e[1] { Some(v) => println(v), None => println(-1) }\n\
                 match e[2] { Some(v) => println(v), None => println(-1) }\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "3\n1\ntrue\n1\n-1\n3\n",
            "Column.from_iter_nullable must scatter Some/None into values + bitmap"
        );
    }
}

#[test]
fn test_e2e_column_iter_and_iter_valid() {
    // iter() -> Vec[Option[T]] (every slot as an Option); iter_valid()
    // -> Vec[T] (valid slots only). Exercised via for-loop iteration
    // over both the let-bound result and a direct for-source.
    let out = run_program(
            "fn main() {\n\
                 let mut c: Column[i64] = Column.new();\n\
                 c.push(10);\n\
                 c.push_null();\n\
                 c.push(30);\n\
                 let all: Vec[Option[i64]] = c.iter();\n\
                 println(all.len());\n\
                 let mut sum = 0;\n\
                 for o in all { match o { Some(v) => { sum = sum + v; }, None => { sum = sum - 1; } } }\n\
                 println(sum);\n\
                 let valid: Vec[i64] = c.iter_valid();\n\
                 println(valid.len());\n\
                 let mut vs = 0;\n\
                 for x in c.iter_valid() { vs = vs + x; }\n\
                 println(vs);\n\
             }\n",
        );
    if let Some(out) = out {
        assert_eq!(
            out, "3\n39\n2\n40\n",
            "Column.iter / iter_valid must match the interpreter"
        );
    }
}

#[test]
fn test_e2e_column_3vl_arithmetic() {
    // SQL three-valued-logic element-wise ops: a result slot is valid
    // iff BOTH inputs are valid (null on either side -> null). col-col
    // arithmetic + comparison (-> Column[bool]) + col-scalar broadcast
    // + unary neg, all with null propagation. a=[10,null,30],
    // b=[1,2,null].
    // Helper params are `ref Column[..]` (borrowed), so the same column
    // can be read across multiple calls without an ownership-move error —
    // the check-clean shape enabled by the B-2026-07-02-27 ref-Column
    // codegen deref fix.
    let out = run_program(
            "fn fst(c: ref Column[i64], i: i64) -> i64 { match c[i] { Some(v) => v, None => -999 } }\n\
             fn fstb(c: ref Column[bool], i: i64) -> i64 { match c[i] { Some(v) => { if v { 1 } else { 0 } }, None => -9 } }\n\
             fn main() {\n\
                 let mut a: Column[i64] = Column.new();\n\
                 a.push(10); a.push_null(); a.push(30);\n\
                 let mut b: Column[i64] = Column.new();\n\
                 b.push(1); b.push(2); b.push_null();\n\
                 let s = a + b;\n\
                 println(fst(s, 0)); println(fst(s, 1)); println(s.null_count());\n\
                 let m = a * 10;\n\
                 println(fst(m, 0)); println(fst(m, 2));\n\
                 let eq = a == b;\n\
                 println(fstb(eq, 0)); println(eq.null_count());\n\
                 let ng = -a;\n\
                 println(fst(ng, 0)); println(fst(ng, 2));\n\
             }\n",
        );
    if let Some(out) = out {
        assert_eq!(
            out, "11\n-999\n2\n100\n300\n0\n2\n-10\n-30\n",
            "Column 3VL arithmetic/comparison/neg must match the interpreter"
        );
    }
}

#[test]
fn test_e2e_column_3vl_div_null_skips_trap() {
    // A null result slot's per-element op is NEVER evaluated, so a
    // null paired with a zero divisor must not trap (matches the
    // interpreter, which evals valid slots only).
    let out = run_program(
        "fn main() {\n\
                 let mut a: Column[i64] = Column.new();\n\
                 a.push_null(); a.push(8);\n\
                 let mut b: Column[i64] = Column.new();\n\
                 b.push(0); b.push(2);\n\
                 let q = a / b;\n\
                 match q[0] { Some(v) => { println(v); }, None => { println(-1); } }\n\
                 match q[1] { Some(v) => { println(v); }, None => { println(-1); } }\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "-1\n4\n",
            "null slot must skip the div-by-zero; valid slot computes"
        );
    }
}

// ── Column[T] codegen — statistical reductions (phase-11 stats) ──

#[test]
fn test_e2e_column_stats_int_reductions() {
    // sum/min/max preserve the integer element type; mean is f64. All
    // skip null slots (valid = [2, 4, 6]).
    let out = run_program(
            "fn main() {\n\
                 let c: Column[i64] = Column.from_iter_nullable([Some(2), None, Some(4), Some(6)]);\n\
                 println(c.sum());\n\
                 println(c.min());\n\
                 println(c.max());\n\
                 println(c.mean());\n\
             }\n",
        );
    if let Some(out) = out {
        assert_eq!(
            out, "12\n2\n6\n4\n",
            "int reductions skip nulls; sum/min/max -> T, mean -> f64"
        );
    }
}

#[test]
fn test_e2e_column_stats_var_std_sample() {
    // f64 column [2, 4, 6]: mean 4, sample var = 8/2 = 4, std = 2.
    let out = run_program(
        "fn main() {\n\
                 let c: Column[f64] = Column.from_vec([2.0, 4.0, 6.0]);\n\
                 println(c.mean());\n\
                 println(c.var());\n\
                 println(c.std());\n\
                 println(c.min());\n\
                 println(c.max());\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "4\n4\n2\n2\n6\n",
            "sample var/std over valid f64 slots"
        );
    }
}

#[test]
fn test_e2e_column_corr_pairwise_valid() {
    // Perfectly correlated -> 1; only both-valid slots contribute.
    let out = run_program(
            "fn main() {\n\
                 let a: Column[f64] = Column.from_vec([1.0, 2.0, 3.0, 4.0]);\n\
                 let b: Column[f64] = Column.from_vec([2.0, 4.0, 6.0, 8.0]);\n\
                 println(a.corr(b));\n\
                 let p: Column[f64] = Column.from_iter_nullable([Some(1.0), Some(2.0), None, Some(4.0)]);\n\
                 let q: Column[f64] = Column.from_iter_nullable([Some(2.0), Some(4.0), Some(99.0), Some(8.0)]);\n\
                 println(p.corr(q));\n\
             }\n",
        );
    if let Some(out) = out {
        assert_eq!(out, "1\n1\n", "Pearson corr over pairwise-valid slots");
    }
}

#[test]
fn test_e2e_column_stats_empty_reduce_traps() {
    // An all-null column has no valid values -> sum traps.
    let captured = run_program_capturing(
        "fn main() {\n\
                 let mut c: Column[i64] = Column.new();\n\
                 c.push_null();\n\
                 println(c.sum());\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("no valid values"),
            "expected empty-reduce trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_e2e_column_var_requires_two_values_traps() {
    // Sample variance is undefined for fewer than 2 valid values.
    let captured = run_program_capturing(
        "fn main() {\n\
                 let c: Column[f64] = Column.from_vec([3.0]);\n\
                 println(c.var());\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("at least 2 valid values"),
            "expected var<2 trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_e2e_column_median_and_quantile() {
    // In-IR sort then linear-interpolation quantile; median == quantile(0.5).
    // Unsorted input + a null-skipping integer median pin the sort + skip.
    let out = run_program(
            "fn main() {\n\
                 let c: Column[f64] = Column.from_vec([4.0, 1.0, 3.0, 2.0]);\n\
                 println(c.median());\n\
                 println(c.quantile(0.0));\n\
                 println(c.quantile(0.25));\n\
                 println(c.quantile(0.75));\n\
                 println(c.quantile(1.0));\n\
                 let o: Column[i64] = Column.from_iter_nullable([Some(5), None, Some(1), Some(3)]);\n\
                 println(o.median());\n\
             }\n",
        );
    if let Some(out) = out {
        assert_eq!(
            out, "2.5\n1\n1.75\n3.25\n4\n3\n",
            "median/quantile must match the interpreter (sorted, null-skipping)"
        );
    }
}

#[test]
fn test_e2e_column_quantile_out_of_range_traps() {
    // q outside [0, 1] traps (matches the interpreter).
    let captured = run_program_capturing(
        "fn main() {\n\
                 let c: Column[f64] = Column.from_vec([1.0, 2.0, 3.0]);\n\
                 println(c.quantile(1.5));\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("must be in [0, 1]"),
            "expected quantile-range trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

// ── Stats.* free-function codegen (phase-11, run/build parity) ──

#[test]
fn test_e2e_stats_free_functions() {
    // All eight `Stats` free functions over a `Vec[f64]`, byte-identical
    // to the interpreter (`eval_stats_fn`). variance/stddev are the
    // POPULATION forms (÷n) — distinct from the Column sample (÷n-1) stats.
    let out = run_program(
        "fn main() {\n\
                 let v: Vec[f64] = vec![3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0];\n\
                 println(Stats.sum(v));\n\
                 println(Stats.prod(v));\n\
                 println(Stats.mean(v));\n\
                 println(Stats.variance(v));\n\
                 println(Stats.stddev(v));\n\
                 println(Stats.median(v));\n\
                 match Stats.min(v) { Some(m) => println(m), None => println(-1.0), };\n\
                 match Stats.max(v) { Some(m) => println(m), None => println(-1.0), };\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "25\n1080\n3.5714285714285716\n6.816326530612246\n2.610809554642438\n3\n1\n9\n",
            "Stats.* must match the interpreter (population var/std, sorted median)"
        );
    }
}

#[test]
fn test_e2e_stats_on_slice() {
    // B-2026-07-18-12: `Stats.*` on a `Slice[f64]` (`v.as_slice()`, the
    // declared `ref Slice[f64]` param's canonical form). Codegen already read
    // the slice correctly; this pins run==build parity now that the
    // interpreter's arg extraction handles `Value::Slice` too (it previously
    // read a slice as empty → sum -0 / mean panic — a run-vs-build divergence).
    let out = run_program(
        "fn main() {\n\
                 let v: Vec[f64] = vec![3.0, 1.0, 4.0];\n\
                 let sl: Slice[f64] = v.as_slice();\n\
                 println(Stats.sum(sl));\n\
                 println(Stats.mean(sl));\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out, "8\n2.6666666666666665\n");
    }
}

#[test]
fn test_e2e_stats_even_median_and_fresh_temp_arg() {
    // Even-count median averages the two middles; a fresh `vec![…]` temp
    // argument is read then freed (the owned-temp leak guard — see the
    // ASAN test below).
    let out = run_program(
        "fn main() {\n\
                 let e: Vec[f64] = vec![10.0, 20.0, 30.0, 40.0];\n\
                 println(Stats.median(e));\n\
                 println(Stats.sum(vec![100.0, 200.0]));\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out, "25\n300\n", "even-count median + fresh-temp arg");
    }
}

#[test]
fn test_e2e_stats_empty_slice() {
    // Empty input: sum -> -0 (Rust's float `Sum` identity is -0.0),
    // prod -> 1, min/max -> None. (mean/variance/stddev/median trap — below.)
    let out = run_program(
        "fn main() {\n\
                 let v: Vec[f64] = vec![];\n\
                 println(Stats.sum(v));\n\
                 println(Stats.prod(v));\n\
                 match Stats.min(v) { Some(m) => println(m), None => println(-1.0), };\n\
                 match Stats.max(v) { Some(m) => println(m), None => println(-1.0), };\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "-0\n1\n-1\n-1\n",
            "empty sum -> -0, prod -> 1, min/max -> None (interpreter parity)"
        );
    }
}

#[test]
fn test_e2e_stats_mean_empty_traps() {
    // `Stats.mean` on an empty slice traps (parity with the interpreter's
    // empty-slice panic).
    let captured = run_program_capturing(
        "fn main() {\n\
                 let v: Vec[f64] = vec![];\n\
                 println(Stats.mean(v));\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("empty slice"),
            "expected empty-slice trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_e2e_stats_stddev_empty_names_stddev_not_variance() {
    // B-2026-08-19-20. `stddev` lowers as `sqrt(variance)`, so it went
    // through `stats_variance`'s guard and reported `Stats.variance()
    // called on empty slice` — while the interpreter, which dispatches
    // `stddev` on its own, said `Stats.stddev()`. One program, two legs,
    // two different function names in the refusal.
    let captured = run_program_capturing(
        "fn main() {\n\
                 let v: Vec[f64] = vec![];\n\
                 println(Stats.stddev(v));\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("Stats.stddev() called on empty slice"),
            "stddev must name itself, not variance; got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

// ── Stats over i64 elements (S5 — the non-f64 element axis) ──────

#[test]
fn test_e2e_stats_i64_full_surface() {
    // B-2026-07-01-9: integer slices previously bit-reinterpreted as
    // doubles under `karac build` (denormal garbage) while `karac run`
    // computed real values. Now the element kind threads from the
    // typechecker: sum/prod fold at i64 (element-typed results),
    // min/max/argmin/argmax/sort/argsort compare at exact i64, and the
    // float statistics promote — all byte-identical to the interpreter.
    let out = run_program(
        "fn main() {\n\
                 let xs: Vec[i64] = vec![4, 1, 3, 2];\n\
                 println(Stats.sum(xs));\n\
                 println(Stats.prod(xs));\n\
                 println(Stats.mean(xs));\n\
                 println(Stats.variance(xs));\n\
                 println(Stats.median(xs));\n\
                 println(Stats.percentile(xs, 25));\n\
                 match Stats.min(xs) { Some(v) => println(v), None => println(-1) }\n\
                 match Stats.max(xs) { Some(v) => println(v), None => println(-1) }\n\
                 match Stats.argmin(xs) { Some(i) => println(i), None => println(-1) }\n\
                 match Stats.argmax(xs) { Some(i) => println(i), None => println(-1) }\n\
                 let s: Vec[i64] = Stats.sort(xs);\n\
                 println(s[0]); println(s[3]);\n\
                 let a: Vec[i64] = Stats.argsort(xs);\n\
                 println(a[0]); println(a[3]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "10\n24\n2.5\n1.25\n2.5\n1.75\n1\n4\n1\n0\n1\n4\n1\n0\n",
            "the i64 Stats surface must match the interpreter"
        );
    }
}

#[test]
fn test_e2e_stats_i64_exact_above_2_pow_53_and_empty_identities() {
    // 2^53 and 2^53 + 1 are the same f64 — the i64 ordering ops must
    // stay exact (no float round-trip). An empty Vec[i64] gets the
    // INTEGER identities (sum 0, prod 1 — not the float -0.0).
    let out = run_program(
        "fn main() {\n\
                 let a = 9007199254740993;\n\
                 let b = 9007199254740992;\n\
                 let xs: Vec[i64] = vec![a, b];\n\
                 match Stats.max(xs) { Some(v) => println(v), None => println(-1) }\n\
                 let s: Vec[i64] = Stats.sort(xs);\n\
                 println(s[1]);\n\
                 let e: Vec[i64] = vec![];\n\
                 println(Stats.sum(e));\n\
                 println(Stats.prod(e));\n\
                 match Stats.min(e) { Some(v) => println(v), None => println(-1) }\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "9007199254740993\n9007199254740993\n0\n1\n-1\n",
            "i64 ordering must be exact above 2^53; empty identities are integer"
        );
    }
}

#[test]
fn test_e2e_stats_i64_sum_overflow_traps() {
    // The i64 fold is CHECKED — overflow traps like the scalar `+`
    // (matching the interpreter's reduce_i64), never wraps silently.
    let captured = run_program_capturing(
        "fn main() {\n\
                 let big = 9223372036854775807;\n\
                 let xs: Vec[i64] = vec![big, 1];\n\
                 println(Stats.sum(xs));\n\
             }\n",
    );
    if let Some(c) = captured {
        let all = format!("{}{}", c.stdout, c.stderr);
        assert!(
            all.contains("integer overflow"),
            "expected the checked-fold overflow trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_e2e_stats_percentile() {
    // NumPy convention p in [0, 100], linear interpolation; byte-identical
    // to the interpreter. sorted [1, 1, 2, 3, 4, 5, 9].
    let out = run_program(
        "fn main() {\n\
                 let v: Vec[f64] = vec![3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0];\n\
                 println(Stats.percentile(v, 50.0));\n\
                 println(Stats.percentile(v, 0.0));\n\
                 println(Stats.percentile(v, 100.0));\n\
                 println(Stats.percentile(v, 25.0));\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out, "3\n1\n9\n1.5\n", "percentile p0/p25/p50/p100");
    }
}

#[test]
fn test_e2e_stats_argmin_argmax_sort_argsort() {
    // argmin/argmax -> Option[i64] (first-index); sort -> Vec[f64];
    // argsort -> Vec[i64]. xs = [3, 1, 4, 1, 5, 9, 2].
    let out = run_program(
        "fn main() {\n\
                 let v: Vec[f64] = vec![3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0];\n\
                 match Stats.argmin(v) { Some(i) => println(i), None => println(-1), }\n\
                 match Stats.argmax(v) { Some(i) => println(i), None => println(-1), }\n\
                 let s: Vec[f64] = Stats.sort(v);\n\
                 println(s[0]);\n\
                 println(s[6]);\n\
                 let a: Vec[i64] = Stats.argsort(v);\n\
                 println(a[0]);\n\
                 println(a[6]);\n\
             }\n",
    );
    if let Some(out) = out {
        // argmin idx 1 (first 1.0), argmax idx 5 (9.0); sort [1..9];
        // argsort first index of smallest = 1, last = index of 9 = 5.
        assert_eq!(out, "1\n5\n1\n9\n1\n5\n", "argmin/argmax/sort/argsort");
    }
}

#[test]
fn test_e2e_stats_argmin_empty_is_none_and_percentile_traps() {
    // argmin on empty -> None.
    let out = run_program(
        "fn main() {\n\
                 let v: Vec[f64] = vec![];\n\
                 match Stats.argmin(v) { Some(i) => println(i), None => println(-1), }\n\
                 let a: Vec[i64] = Stats.argsort(v);\n\
                 println(a.len());\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "-1\n0\n",
            "empty argmin -> None, empty argsort -> len 0"
        );
    }
    // percentile out of range traps.
    let captured = run_program_capturing(
        "fn main() {\n\
                 let v: Vec[f64] = vec![1.0, 2.0, 3.0];\n\
                 println(Stats.percentile(v, 150.0));\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("[0, 100]"),
            "expected percentile-range trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

// ── Column[String] codegen — heap-element lifecycle (phase-11) ──

#[test]
fn test_e2e_column_string_from_vec_iter_valid() {
    // Build a Column[String] from a Vec[String] binding (deep-clones each
    // string in), read it back via iter_valid (deep-clones each out) —
    // byte-identical to the interpreter. A subsequent move (`let d = c`)
    // and scope drop must free every string exactly once (ASAN test below).
    let out = run_program(
        "fn main() {\n\
                 let v: Vec[String] = [\"alpha\", \"beta\", \"gamma\"];\n\
                 let c: Column[String] = Column.from_vec(v);\n\
                 println(c.len());\n\
                 println(c.null_count());\n\
                 println(c.valid_count());\n\
                 for s in c.iter_valid() { println(s); }\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "3\n0\n3\nalpha\nbeta\ngamma\n",
            "Column[String] from_vec + iter_valid must match the interpreter"
        );
    }
}

// ── DataFrame codegen (phase-11 Arrow Q6) ───────────────────────

#[test]
fn test_e2e_dataframe_build_lookup_accessors() {
    // Heterogeneous build (i64 + f64 columns), width/height/has_column,
    // and column(name) copy-out round-trips values. AOT output must be
    // byte-identical to the interpreter twin.
    let out = run_program(
        "fn main() {\n\
                 let mut df: DataFrame = DataFrame.new();\n\
                 df.insert(\"age\", Column.from_vec([30, 25, 40]));\n\
                 df.insert(\"score\", Column.from_vec([1.5, 2.5, 3.5]));\n\
                 println(df.width());\n\
                 println(df.height());\n\
                 println(df.has_column(\"age\"));\n\
                 println(df.has_column(\"nope\"));\n\
                 let ages: Column[i64] = df.column(\"age\");\n\
                 match ages[2] { Some(v) => println(v), None => println(-1) }\n\
                 let scores: Column[f64] = df.column(\"score\");\n\
                 match scores[0] { Some(v) => println(v), None => println(0.0) }\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "2\n3\ntrue\nfalse\n40\n1.5\n",
            "DataFrame build / lookup / accessors must match the interpreter"
        );
    }
}

#[test]
fn test_e2e_dataframe_column_is_value_copy() {
    // Value semantics: a looked-up column is independent of the frame
    // (copy-out). Mutating the copy leaves the frame untouched — the
    // run/build parity contract pinned by the interpreter twin.
    let out = run_program(
        "fn main() {\n\
                 let mut df: DataFrame = DataFrame.new();\n\
                 df.insert(\"a\", Column.from_vec([1, 2]));\n\
                 let mut c: Column[i64] = df.column(\"a\");\n\
                 c.push(3);\n\
                 println(c.len());\n\
                 println(df.height());\n\
                 let d: Column[i64] = df.column(\"a\");\n\
                 println(d.len());\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "3\n2\n2\n",
            "column() must be a value copy, not a view"
        );
    }
}

#[test]
fn test_e2e_dataframe_insert_replace() {
    // Re-inserting an existing name replaces the column (frees the old);
    // width unchanged, the new values win.
    let out = run_program(
        "fn main() {\n\
                 let mut df: DataFrame = DataFrame.new();\n\
                 df.insert(\"a\", Column.from_vec([1, 2]));\n\
                 df.insert(\"a\", Column.from_vec([9, 8]));\n\
                 println(df.width());\n\
                 println(df.height());\n\
                 let a: Column[i64] = df.column(\"a\");\n\
                 match a[0] { Some(v) => println(v), None => println(-1) }\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "1\n2\n9\n",
            "insert replace must overwrite + free the old column"
        );
    }
}

#[test]
fn test_e2e_dataframe_insert_length_mismatch_traps() {
    // The Arrow equal-length invariant: inserting a column whose length
    // differs from the table's row count traps (matches the interpreter).
    let captured = run_program_capturing(
        "fn main() {\n\
                 let mut df: DataFrame = DataFrame.new();\n\
                 df.insert(\"a\", Column.from_vec([1, 2, 3]));\n\
                 df.insert(\"b\", Column.from_vec([1, 2]));\n\
                 println(df.width());\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("column length does not match"),
            "expected the equal-length trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_e2e_dataframe_column_names() {
    // column_names() -> Vec[String] in schema order, for-iterable.
    let out = run_program(
        "fn main() {\n\
                 let mut df: DataFrame = DataFrame.new();\n\
                 df.insert(\"age\", Column.from_vec([1, 2]));\n\
                 df.insert(\"name\", Column.from_vec([3, 4]));\n\
                 for n in df.column_names() { println(n); }\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "age\nname\n",
            "column_names must yield schema-order names"
        );
    }
}

#[test]
fn test_e2e_dataframe_string_column() {
    // A heterogeneous frame with a String column: insert + copy-out +
    // select reorder + replace, all value-semantics (independent String
    // heaps). Byte-identical to the interpreter twin.
    let out = run_program(
        "fn main() {\n\
                 let mut df: DataFrame = DataFrame.new();\n\
                 let names: Vec[String] = [\"ann\", \"bob\"];\n\
                 df.insert(\"name\", Column.from_vec(names));\n\
                 df.insert(\"age\", Column.from_vec([20, 30]));\n\
                 println(df.width());\n\
                 println(df.height());\n\
                 let back: Column[String] = df.column(\"name\");\n\
                 for s in back.iter_valid() { println(s); }\n\
                 let sub: DataFrame = df.select([\"age\", \"name\"]);\n\
                 let sn: Column[String] = sub.column(\"name\");\n\
                 for s in sn.iter_valid() { println(s); }\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "2\n2\nann\nbob\nann\nbob\n",
            "DataFrame with a String column must match the interpreter"
        );
    }
}

#[test]
fn test_e2e_dataframe_describe() {
    // describe() — per-numeric-column stats (count/mean/std/min/quartiles/
    // max), a String column skipped, a leading statistic label column,
    // null-skipping. Byte-identical to the interpreter twin.
    let out = run_program(
            "fn main() {\n\
                 let mut df: DataFrame = DataFrame.new();\n\
                 df.insert(\"age\", Column.from_vec([20, 30, 40, 50]));\n\
                 let names: Vec[String] = [\"a\", \"b\", \"c\", \"d\"];\n\
                 df.insert(\"name\", Column.from_vec(names));\n\
                 let score: Column[f64] = Column.from_iter_nullable([Some(1.0), None, Some(3.0), Some(5.0)]);\n\
                 df.insert(\"score\", score);\n\
                 let d: DataFrame = df.describe();\n\
                 println(d.width());\n\
                 println(d.height());\n\
                 for n in d.column_names() { println(n); }\n\
                 let a: Column[f64] = d.column(\"age\");\n\
                 for v in a.iter_valid() { println(v); }\n\
                 let sc: Column[f64] = d.column(\"score\");\n\
                 for v in sc.iter_valid() { println(v); }\n\
             }\n",
        );
    if let Some(out) = out {
        // width 3 (statistic + age + score; name skipped), height 8.
        // age [20,30,40,50]: 4/35/std/20/27.5/35/42.5/50.
        // score valid [1,3,5]: 3/3/2/1/2/3/4/5.
        assert_eq!(
                out,
                "3\n8\nstatistic\nage\nscore\n4\n35\n12.909944487358056\n20\n27.5\n35\n42.5\n50\n3\n3\n2\n1\n2\n3\n4\n5\n",
                "describe() must match the interpreter (numeric-only, null-skipping, labels)"
            );
    }
}

#[test]
fn test_e2e_column_neg_i64_min_traps() {
    // B-2026-07-01-2: `-c` on a Column[i64] slot holding i64::MIN must
    // trap with the scalar checked-neg overflow (like the interpreter),
    // not silently wrap the way the old bare `ineg` lowering did.
    let captured = run_program_capturing(
        "fn main() {\n\
                 let big = -9223372036854775807;\n\
                 let mut c: Column[i64] = Column.new();\n\
                 c.push(big - 1);\n\
                 let m = -c;\n\
                 match m[0] { Some(v) => { println(v); }, None => { println(-1); } }\n\
             }\n",
    );
    if let Some(c) = captured {
        let all = format!("{}{}", c.stdout, c.stderr);
        assert!(
            all.contains("integer overflow"),
            "expected the checked-neg overflow trap on i64::MIN, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

// ── Sub-word Vec element store must narrow the value to the element width ──
//
// Regression for the `Vec[u8]`/`Vec[bool]` push (and index-store) heap
// overflow: a computed scalar for a sub-word element (`v.push(b'a' + (i as
// u8))`) compiles to the default i64, and the element store wrote 8 bytes
// over the 1-byte slot. Harmless inside allocation slack, but the push that
// fills an exact-size-class buffer (cap 64/128/256…) smeared 7 bytes past
// the end, corrupting the adjacent heap — an ASLR-intermittent SIGSEGV on
// later realloc/free. The values read back correct (each slot's low byte
// survives), so this is a build+run test: pre-fix it reliably crashes once
// the buffer grows past ~64 elements; the fix narrows the value to the
// element type before the store. ASAN missed it (the spill lands in
// realloc-rounding slack, not a poisoned redzone), so the guard is E2E
// execution, not the sanitizer suite.
#[test]
fn e2e_subword_vec_push_narrows_to_elem_width_no_heap_overflow() {
    // 240 computed-u8 pushes cross the 64/128 power-of-two cap boundaries
    // where the pre-fix 8-byte store overflowed an exact-size allocation.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut v: Vec[u8] = Vec.new();\n\
                 let mut a = 0i64;\n\
                 while a < 240i64 { v.push(b'a' + ((a % 3i64) as u8)); a = a + 1i64; }\n\
                 let mut sum = 0i64;\n\
                 let mut k = 0i64;\n\
                 while k < v.len() { sum = sum + (v[k] as i64); k = k + 1i64; }\n\
                 println(f\"{v.len()} {sum}\");\n\
                 // index-store path: overwrite a computed u8 then re-sum.\n\
                 v[100] = 200u8 - ((0i64 % 3i64) as u8);\n\
                 println(f\"{v[100] as i64}\");\n\
             }",
    ) {
        // 80×(97+98+99) = 23520; v[100] = 200.
        assert_eq!(out, "240 23520\n200\n");
    }
}

// The same narrowing rule at the AUTO-PAR TABULATE REWRITE, which is a
// SECOND element-store site. When the analyzer recognizes a
// `while … { v.push(e) }` loop as a tabulate, it replaces the per-push
// grow/store with one hoisted realloc plus a raw `base[idx] = v` store
// (`emit_tabulate_store`, codegen/reduce.rs) — and that rewrite did not
// carry the `coerce_scalar_to_type_from` the push arm above performs, so a
// computed sub-word element went back to an 8-byte store over a 1-byte
// slot. Verified at the machine level, not just by crash: pre-fix the
// `Vec[u16]` form emitted `mov %rdx,(%rax,%rcx,2)` — a 64-bit store at a
// 2-byte stride — while the `Vec[u8]` form aborted in glibc
// (`realloc(): invalid next size`) at as few as 20 elements. The wider
// sub-word types corrupt identically but land in allocator slack, so `u8`
// is the shape that reports it.
//
// Distinct from `e2e_subword_vec_push_narrows_to_elem_width_no_heap_
// overflow` above: that one guards the ordinary push arm and does NOT
// trigger the tabulate rewrite, which is why it stayed green throughout.
// The nested-loop body here is what the rewrite recognizes.
#[test]
fn e2e_subword_vec_tabulate_store_narrows_to_elem_width() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut src: Vec[u8] = Vec.new();\n\
                 let mut i = 0i64;\n\
                 while i < 40i64 {\n\
                     let mut p = 0i64;\n\
                     while p < 12i64 {\n\
                         src.push(97u8 + ((i + p) % 26i64) as u8);\n\
                         p = p + 1i64;\n\
                     }\n\
                     i = i + 1i64;\n\
                 }\n\
                 let mut k = 0i64;\n\
                 let mut s = 0i64;\n\
                 while k < src.len() { s = s + (src[k] as i64); k = k + 1i64; }\n\
                 println(f\"{src.len()} {s}\");\n\
             }",
    ) {
        assert_eq!(out, "480 52476\n");
    }
}

#[test]
fn e2e_subword_vec_tabulate_store_narrows_u16_and_u32() {
    // The width-family peers of the case above. These never crashed — the
    // 6-/4-byte spill lands inside realloc rounding — so they are pinned on
    // VALUES, which the fix must keep exact while the store narrows.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let mut a: Vec[u16] = Vec.new();\n\
                 let mut b: Vec[u32] = Vec.new();\n\
                 let mut i = 0i64;\n\
                 while i < 40i64 {\n\
                     let mut p = 0i64;\n\
                     while p < 4i64 {\n\
                         a.push(1000u16 + ((i + p) % 26i64) as u16);\n\
                         b.push(100000u32 + ((i + p) % 26i64) as u32);\n\
                         p = p + 1i64;\n\
                     }\n\
                     i = i + 1i64;\n\
                 }\n\
                 let mut k = 0i64;\n\
                 let mut sa = 0i64;\n\
                 let mut sb = 0i64;\n\
                 while k < a.len() { sa = sa + (a[k] as i64); sb = sb + (b[k] as i64); k = k + 1i64; }\n\
                 println(f\"{a.len()} {sa} {sb}\");\n\
             }",
        ) {
            assert_eq!(out, "160 161748 16001748\n");
        }
}

/// B-2026-08-27-9, the narrower-width legs. `f32` takes the identical
/// route, and the half-precision types (`f16` / `bf16`) reach it through
/// their prelude wrapper structs rather than as bare scalars — a different
/// path into the same comparator, worth pinning separately so a later
/// change to those wrappers cannot regress them silently.
#[test]
fn test_e2e_narrow_float_fields_compare_by_ieee() {
    assert_eq!(
        run_program(
            r#"
#[derive(PartialEq)]
shared struct S32 { x: f32 }
#[derive(PartialEq)]
shared struct S16 { x: f16 }
#[derive(PartialEq)]
shared struct Sb16 { x: bf16 }
fn nan32(z: f32) -> f32 { return z / z; }
fn negzero32(z: f32) -> f32 { return -z; }
fn main() {
    let p: f32 = 0.0;
    let n = negzero32(p);
    let q = nan32(p);
    let a = S32 { x: p };
    let b = S32 { x: n };
    let q1 = S32 { x: q };
    let q2 = S32 { x: q };
    println(f"f32-negzero={a == b}");
    println(f"f32-nan={q1 == q2}");
    let h1 = S16 { x: 1.5f16 };
    let h2 = S16 { x: 1.5f16 };
    let h3 = S16 { x: 2.5f16 };
    println(f"f16-eq={h1 == h2}");
    println(f"f16-ne={h1 == h3}");
    let g1 = Sb16 { x: 1.5bf16 };
    let g2 = Sb16 { x: 1.5bf16 };
    let g3 = Sb16 { x: 2.5bf16 };
    println(f"bf16-eq={g1 == g2}");
    println(f"bf16-ne={g1 == g3}");
}
"#
        ),
        Some(
            "f32-negzero=true\nf32-nan=false\n\
                 f16-eq=true\nf16-ne=false\n\
                 bf16-eq=true\nbf16-ne=false\n"
                .to_string()
        )
    );
}
