//! DataFrame, LazyFrame, Column, stats, CSV/Arrow IPC -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer frames::
//!
//! New fixtures about DataFrame, LazyFrame, Column, stats, CSV/Arrow IPC belong in this file.

use super::*;

#[test]
fn asan_column_string_index_clone_out_no_leak() {
    // S6c-12 Slice 5: `Column[String]` indexing `c[i] -> Option[String]`
    // under `karac build` DEEP-CLONES the element so the returned Option
    // owns an independent heap and the column keeps its copy. Exercises
    // both a direct `c[i].unwrap()` and the `self[i]` form inside a user
    // `impl … for Column[String]` (the Slice 5 headline). Loops 40× with
    // >=36-byte payloads for LSan reachability; the per-round `a`/`b`
    // clones AND the column's 3 owned strings must all free with no
    // double-free / UAF (mac ASAN) and no leak (Linux LSan CI).
    assert_clean_asan_run(
            r#"
trait Pick { fn at(ref self, i: i64) -> String; }
impl Pick for Column[String] {
    fn at(ref self, i: i64) -> String { self[i].unwrap() }
}
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let v: Vec[String] = Vec[
            "col-string-index-alpha-aaaaaaaaaaaaaaaaaaaa".to_string(),
            "col-string-index-bravo-bbbbbbbbbbbbbbbbbbbb".to_string(),
            "col-string-index-charlie-cccccccccccccccccc".to_string()
        ];
        let c: Column[String] = Column.from_vec(v);
        let a: String = c[0i64].unwrap();
        let b: String = c.at(2i64);
        println(f"{c.len()} {a} {b}");
        round = round + 1i64;
    }
}
"#,
            [
                "3 col-string-index-alpha-aaaaaaaaaaaaaaaaaaaa col-string-index-charlie-cccccccccccccccccc",
            ]
            .repeat(40)
            .as_slice(),
            "asan_column_string_index_clone_out_no_leak",
        );
}

#[test]
fn asan_column_from_vec_temp_string_move_no_leak() {
    // B-2026-07-06-1: `Column.from_vec(<temporary Vec[String]>)` under
    // `karac build` MOVES the source's String structs into the column
    // (bitwise memcpy transfers each heap) and frees ONLY the source's
    // OUTER buffer — the elements are not drained, so the column becomes
    // their sole owner. No clone, no double-free, no leak — mirroring the
    // POD-temp path. Covers BOTH temp shapes: an inline array literal and a
    // function-call result. Loops 40x with >=36-byte payloads for LSan
    // reachability; each round the columns' 5 moved strings + the 2
    // index-clones (`a`/`e`) must all free exactly once (mac ASAN: no
    // double-free/UAF; Linux LSan CI: no leak). Sibling of
    // `asan_column_string_index_clone_out_no_leak` (a let-bound source).
    assert_clean_asan_run(
        r#"
fn mk() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("from-vec-temp-call-delta-dddddddddddddddddddd".to_string());
    v.push("from-vec-temp-call-echo-eeeeeeeeeeeeeeeeeeeeee".to_string());
    v
}
fn main() {
    let mut round: i64 = 0i64;
    let mut total: i64 = 0i64;
    while round < 40i64 {
        let c: Column[String] = Column.from_vec([
            "from-vec-temp-lit-alpha-aaaaaaaaaaaaaaaaaaaa".to_string(),
            "from-vec-temp-lit-bravo-bbbbbbbbbbbbbbbbbbbb".to_string(),
            "from-vec-temp-lit-charlie-cccccccccccccccccc".to_string()
        ]);
        let d: Column[String] = Column.from_vec(mk());
        let a: String = c[0i64].unwrap();
        let e: String = d[1i64].unwrap();
        total = total + c.len() + d.len() + a.len() + e.len();
        round = round + 1i64;
    }
    println(f"{total}");
}
"#,
        &["3800"], // 40 * (3 + 2 + 44 + 46)
        "asan_column_from_vec_temp_string_move_no_leak",
    );
}

/// Column heap lifecycle (phase-11 data-science stdlib, Arrow codegen
/// core slice): each `Column[T]` is a control block + a separate data
/// buffer + a separate validity bitmap, all freed once at scope exit
/// via `FreeColumn`'s null-guard (three `free`s). Exercises every
/// ownership-transfer shape — construction (new / with_capacity /
/// from_vec, the last with a temporary-Vec eager free), push growth
/// (realloc of both buffers), `let b = a;` move (source slot nulled —
/// double-free would trip ASAN), and fn-boundary moves (owned arg +
/// tail return). Leak detection on Linux (detect_leaks=1) additionally
/// catches a missing free of the data buffer / bitmap / control block.
#[test]
fn asan_column_lifecycle_clean() {
    let label = "column_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn make() -> Column[i64] {
    let mut c: Column[i64] = Column.new();
    c.push(7);
    c.push_null();
    c.push(9);
    c
}

fn take(c: Column[f64]) -> i64 {
    c.null_count()
}

fn main() {
    // new() + push growth (forces realloc of data + bitmap from cap 0).
    let mut a: Column[i64] = Column.new();
    a.push(1);
    a.push(2);
    a.push_null();
    a.push(4);
    a.push(5);
    println(a.len());
    println(a.null_count());
    // with_capacity (no growth) + indexing.
    let mut w: Column[i64] = Column.with_capacity(8);
    w.push(11);
    match w[0] { Some(v) => println(v), None => println(-1) }
    // from_vec with a temporary Vec arg (eager-free of the source buffer).
    let v: Column[f64] = Column.from_vec([1.0, 2.0, 3.0]);
    println(take(v));
    // let-rebind move (source slot nulled — no double-free).
    let h: Column[i64] = Column.from_vec([8, 9]);
    let k = h;
    println(k.len());
    // fn-return move (tail return owns the control block).
    let m = make();
    println(m.null_count());
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check FreeColumn double-free/leak on the move-suppression paths",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["5", "1", "11", "0", "2", "1"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// DataFrame heap lifecycle (phase-11 Arrow Q6 codegen): `insert`
/// copies the argument column *in* (freeing a fresh-temp original) and
/// grows the entries buffer from cap 0; a same-name `insert` replaces
/// (frees the old column); `column` copies *out* a fresh independent
/// column (its own `FreeColumn`); the frame is moved (`let df2 = df`,
/// source slot nulled — the `FreeDataFrame` drop runs once); and the
/// `FreeDataFrame` drop loop frees every column (data + bitmap +
/// control) + name buffer, then the entries buffer + control. A
/// missing free leaks (Linux detect_leaks); a double free is caught
/// everywhere.
#[test]
fn asan_dataframe_lifecycle_clean() {
    let label = "dataframe_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let mut df: DataFrame = DataFrame.new();
    df.insert("age", Column.from_vec([30, 25, 40]));
    df.insert("score", Column.from_vec([1.5, 2.5, 3.5]));
    // Replace an existing column (frees the old column's allocations).
    df.insert("age", Column.from_vec([31, 26, 41]));
    println(df.width());
    println(df.height());
    // Copy-out: a fresh independent column, mutated, then dropped.
    let mut a: Column[i64] = df.column("age");
    a.push(99);
    println(a.len());
    println(df.height());
    // Move the frame (source slot nulled — drop runs exactly once).
    let df2: DataFrame = df;
    println(df2.width());
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check FreeDataFrame drop loop + insert copy-in / replace frees",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["2", "3", "4", "3", "2"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// `df.write_csv` heap lifecycle (phase-11 CSV leg): the serializer is
/// runtime-internal Rust (`karac_runtime_df_write_csv` walks the frame's
/// control blocks read-only and frees its own CSV buffer), so the only
/// Kāra-side allocations are the frame + the path/read-back Strings —
/// each freed exactly once. String columns exercise the per-slot
/// {ptr,len,cap} element reads (no cloning in the serializer). A missing
/// free leaks (Linux detect_leaks); a double free is caught everywhere.
#[test]
fn asan_dataframe_write_csv_no_leak() {
    let label = "dataframe_write_csv";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let tmp = std::env::temp_dir().join("kara_asan_df_write_csv.csv");
    let _ = std::fs::remove_file(&tmp);
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        r#"
fn main() with writes(FileSystem) reads(FileSystem) {{
    let mut df: DataFrame = DataFrame.new();
    df.insert("n", Column.from_vec([1, 2, 3]));
    df.insert("s", Column.from_vec(["a,b", "plain", "q\"q"]));
    let nn: Vec[Option[i64]] = vec![Some(7i64), None, Some(9i64)];
    df.insert("opt", Column.from_iter_nullable(nn));
    match df.write_csv("{path}") {{
        Ok(_) => match fs.read_to_string("{path}") {{
            Ok(s) => println(s.len()),
            Err(_) => println(-1),
        }},
        Err(_) => println(-2),
    }}
}}
"#
    );
    let result = run_under_asan(&src, label);
    let _ = std::fs::remove_file(&tmp);
    let Some((stdout, status)) = result else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check karac_runtime_df_write_csv reads + IoResult unpack",
        status.code()
    );
    assert_eq!(
        stdout.trim(),
        "38",
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// Phase-11 Arrow IPC codegen twin: `to_arrow_ipc()` adopts a buffer the
/// RUNTIME allocated (`karac_arrow_*_to_ipc` → `karac_alloc_or_panic`) as
/// an owned `Vec[u8]`. That hand-off is the leak / double-free surface —
/// codegen must set `cap = max(len, 1)` to match the runtime's allocation
/// and free the buffer exactly once when the Vec drops. All three
/// receivers are covered, plus the shapes where the runtime side allocates
/// most: a String column and a String-bearing DataFrame (each cell is
/// cloned into an arrow array, so a mismatch leaks on the Rust side), an
/// empty column and a zero-column frame (the `max(len,1)` corner where
/// len == 0 but a real allocation still happened), and a Tensor (whose
/// FixedSizeList wrapper allocates an extra layer).
///
/// The read direction inverts the risk: there the RUNTIME builds the
/// control-block graph and the compiled caller's ordinary cleanup frees it,
/// so any drift from the layout codegen builds itself shows up as a leak or
/// a double-free. The String cases carry per-cell heaps (freed through the
/// `cap == len` guard) and the frame cases carry a copied name per entry.
/// Both temporary-argument round-trips are here too: the intermediate
/// `Vec[u8]` has no other owner, so the call site must free it — and only
/// after the runtime has read it, which is the ordering a first cut of this
/// lowering got wrong (freed at extraction, handing the runtime a dangling
/// pointer). The Tensor read is the one graph that is a SINGLE allocation
/// (`[rank][dims][data]`), so it fails differently from the tabular pair —
/// a wrong header size would corrupt the data region rather than orphan a
/// buffer.
#[test]
fn asan_arrow_ipc_no_leak() {
    let label = "arrow_ipc";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let src = r#"
fn main() {
    let mut total: i64 = 0;
    let mut c: Column[i64] = Column.new();
    c.push(10); c.push(20); c.push_null(); c.push(40);
    let b1 = c.to_arrow_ipc();
    total = total + b1.len();
    let cs: Column[String] = Column.from_vec(["alpha", "beta"]);
    let b2 = cs.to_arrow_ipc();
    total = total + b2.len();
    let ce: Column[i64] = Column.new();
    let b3 = ce.to_arrow_ipc();
    total = total + b3.len();
    let mut df: DataFrame = DataFrame.new();
    df.insert("age", Column.from_vec([30i64, 25i64]));
    df.insert("name", Column.from_vec(["ada", "bob"]));
    let b4 = df.to_arrow_ipc();
    total = total + b4.len();
    let empty: DataFrame = DataFrame.new();
    let b5 = empty.to_arrow_ipc();
    total = total + b5.len();
    let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);
    let b6 = t.to_arrow_ipc();
    total = total + b6.len();

    // Read direction: the runtime BUILDS these graphs (data buffer, validity
    // bitmap, control block, per-cell String heaps, and for a frame the entry
    // array + copied names), and only the caller's ordinary Column/DataFrame
    // cleanup frees them. A layout that doesn't match what codegen builds
    // itself leaks or double-frees here.
    let r1: Column[i64] = Column.from_arrow_ipc(b1);
    total = total + r1.len();
    let r2: Column[String] = Column.from_arrow_ipc(b2);
    total = total + r2.len();
    let r3: Column[i64] = Column.from_arrow_ipc(b3);
    total = total + r3.len();
    let r4: DataFrame = DataFrame.from_arrow_ipc(b4);
    total = total + r4.width();
    let r5: DataFrame = DataFrame.from_arrow_ipc(b5);
    total = total + r5.width();
    // TEMPORARY argument — the round-trip shape. Nothing else owns the
    // intermediate buffer, so the call site must free it, and only AFTER the
    // runtime has read it.
    let r6: Column[String] = Column.from_arrow_ipc(r2.to_arrow_ipc());
    total = total + r6.len();
    let r7: DataFrame = DataFrame.from_arrow_ipc(r4.to_arrow_ipc());
    total = total + r7.width();
    // Tensor: one allocation for [rank][dims][data], freed as a whole. The
    // `?`-axis case takes the same path with the extent supplied by the stream.
    let r8: Tensor[i64, [2, 3]] = Tensor.from_arrow_ipc(b6);
    total = total + r8.sum();
    let r9: Tensor[i64, [?, 3]] = Tensor.from_arrow_ipc(r8.to_arrow_ipc());
    total = total + r9.sum();
    println(total > 0);
}
"#;
    let Some((stdout, status)) = run_under_asan(src, label) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — check the \
             karac_arrow_*_to_ipc buffer adoption (cap = max(len, 1)), the \
             from_ipc control-block layout, and the temp-argument free ordering",
        status.code()
    );
    assert_eq!(
        stdout.trim(),
        "true",
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// `DataFrame.read_csv` heap lifecycle (phase-11 CSV leg): the runtime
/// builds the WHOLE frame graph (control + entries + names + column
/// controls + data + bitmaps + String cell heaps) with malloc-compatible
/// allocations, and the Ok(df) pattern binding owns it via the ordinary
/// FreeDataFrame cleanup — every allocation freed exactly once,
/// including per-cell String heaps (cap == len) and skipped nulls
/// ({null,0,0}). A missing free leaks (Linux detect_leaks); a double
/// free is caught everywhere.
#[test]
fn asan_dataframe_read_csv_no_leak() {
    let label = "dataframe_read_csv";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let tmp = std::env::temp_dir().join("kara_asan_df_read_csv.csv");
    std::fs::write(&tmp, "n,s,opt\n1,\"a,b\",7\n2,plain,\n3,\"q\"\"q\",9\n").unwrap();
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        r#"
fn main() with reads(FileSystem) {{
    match DataFrame.read_csv("{path}") {{
        Ok(df) => {{
            println(df.width());
            println(df.height());
            let s: Column[String] = df.column("s");
            match s[0] {{ Some(v) => println(v.len()), None => println(-1) }}
            let o: Column[i64] = df.column("opt");
            println(o.null_count());
        }}
        Err(_) => println(-2),
    }}
}}
"#
    );
    let result = run_under_asan(&src, label);
    let _ = std::fs::remove_file(&tmp);
    let Some((stdout, status)) = result else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check karac_runtime_df_read_csv allocations vs FreeDataFrame",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["3", "3", "3", "1"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// `DataFrame` holding a `Column[String]` heap lifecycle (phase-11
/// DataFrame-String integration). A String column inside a frame must
/// keep value semantics with independent String heaps: `insert`
/// deep-clones the column's strings IN (a fresh-temp original is fully
/// freed incl. its strings; an identifier source keeps its own drop),
/// `column(name)` deep-clones OUT, `select` deep-clones into the new
/// frame, `insert`-replace frees the old column's strings, and the
/// frame drop frees every column's per-element strings (`elem_size == 24`
/// runtime branch). A shared heap (memcpy without re-clone) double-frees;
/// a missing per-element free leaks (Linux detect_leaks). Long payloads
/// (>= 23 bytes) force real heap allocation.
#[test]
fn asan_dataframe_string_column_lifecycle_clean() {
    let label = "dataframe_string_column";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let mut df: DataFrame = DataFrame.new();
    // insert from a fresh-temp Column[String] (from_vec of an identifier Vec).
    let names: Vec[String] = ["alpha_padding_aaaaaaaaaaaaaaa", "beta_padding_bbbbbbbbbbbbbbbb"];
    df.insert("name", Column.from_vec(names));
    df.insert("age", Column.from_vec([20, 30]));
    // copy a String column OUT (independent clone; dropped after use).
    let back: Column[String] = df.column("name");
    for s in back.iter_valid() { println(s.len()); }
    // select reorders both a numeric and a String column into a fresh frame.
    let sub: DataFrame = df.select(["age", "name"]);
    let sn: Column[String] = sub.column("name");
    println(sn.valid_count());
    // replace the String column (frees the old column's strings).
    let repl: Vec[String] = ["gamma_padding_ccccccccccccccc", "delta_padding_ddddddddddddddd"];
    df.insert("name", Column.from_vec(repl));
    let back2: Column[String] = df.column("name");
    for s in back2.iter_valid() { println(s.len()); }
    println(df.width());
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check deep_copy String re-clone / column_free_allocations String drain / replace",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["29", "29", "2", "29", "29", "2"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// `DataFrame.describe()` heap lifecycle (phase-11 describe codegen). The
/// result is a fresh frame: a `statistic` `Column[String]` of static
/// labels (`cap == 0`, so drop skips them — no rodata free) and one
/// `Column[f64]` per numeric source column (fresh control / data / bitmap,
/// freed by the result frame's `FreeDataFrame`). Each stats column
/// allocates and frees an f64 scratch buffer. The source frame is borrowed
/// (its own drop unaffected). A missing free leaks (Linux detect_leaks);
/// the scratch buffer or a stats column freed twice trips ASAN. Reuse of
/// the source frame after describe pins it wasn't consumed.
#[test]
fn asan_dataframe_describe_lifecycle_clean() {
    let label = "dataframe_describe_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let mut df: DataFrame = DataFrame.new();
    df.insert("age", Column.from_vec([20, 30, 40, 50]));
    // a String column (skipped by describe) + a null-bearing float column.
    let names: Vec[String] = ["alpha_padding_aaaaaaaaaaaaaaa", "beta_padding_bbbbbbbbbbbbbbbb", "gamma_padding_ccccccccccccccc", "delta_padding_ddddddddddddddd"];
    df.insert("name", Column.from_vec(names));
    let score: Column[f64] = Column.from_iter_nullable([Some(1.0), None, Some(3.0), Some(5.0)]);
    df.insert("score", score);
    // describe builds a fresh frame (statistic + age + score).
    let d: DataFrame = df.describe();
    println(d.width());
    println(d.height());
    let lab: Column[String] = d.column("statistic");
    println(lab.valid_count());
    let a: Column[f64] = d.column("age");
    for v in a.iter_valid() { println(v); }
    // source frame still usable (borrowed by describe, not consumed).
    println(df.width());
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check describe fresh-frame build / static-label cap=0 / f64 scratch free",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec![
            "3",
            "8",
            "8",
            "4",
            "35",
            "12.909944487358056",
            "20",
            "27.5",
            "35",
            "42.5",
            "50",
            "3"
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// LazyFrame codegen twin heap lifecycle (phase-11 LazyDataFrame,
/// `src/codegen/lazyframe.rs` + `runtime/src/lazy.rs`). Exercises every
/// leg of the release-everywhere + retain-on-return ownership model:
/// `lazy()`'s deep-copied frame, chained builder temps (each a fresh +1
/// `Arc` handle released at scope exit), scalar-lit wraps, a BOUND and
/// REUSED `LazyExpr` (one production, two borrows), the `std.lazy`
/// col/lit wrappers (retain-on-return in the wrapper + caller-side
/// release registration), `explain`'s malloc'd-buffer String adoption,
/// and `collect`'s fresh DataFrame control block (ordinary
/// `FreeDataFrame`) — String columns and NULL slots included so the
/// column deep copies and the null bitmap round-trip is covered. A
/// missed release leaks (Linux detect_leaks); an over-release
/// double-frees the `Arc` (caught everywhere).
#[test]
fn asan_lazyframe_no_leak() {
    let label = "lazyframe_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
import std.lazy.{col, lit};
fn main() {
    let mut df: DataFrame = DataFrame.new();
    df.insert("age", Column.from_vec([30i64, 15i64, 41i64, 8i64]));
    df.insert("name", Column.from_vec(["ada_padding_aaaaaaaaaaaaaaaaa", "bob", "eve_padding_ccccccccccccccccc", "kid"]));
    df.insert("score", Column.from_vec([9.5, 3.5, 7.0, 1.0]));
    let nn: Vec[Option[i64]] = vec![Some(30i64), None, Some(41i64), Some(8i64)];
    df.insert("opt", Column.from_iter_nullable(nn));
    // Bound-and-reused expr: one production, two borrowing uses.
    let c = LazyExpr.col("age");
    let plan = df.lazy()
        .filter(c.gt(10).and_(c.lt(40)).or_(col("score").ge(lit(7.0))))
        .select(vec!["name", "opt"])
        .limit(3);
    println(plan.explain());
    let out = plan.collect();
    println(out.height());
    let names: Column[String] = out.column("name");
    match names[0] { Some(v) => println(v), None => println("null") }
    let opt: Column[i64] = out.column("opt");
    match opt[1] { Some(v) => println(v), None => println("null") }
    // Arithmetic + not_ temps, collected into a second frame.
    let out2 = df.lazy().filter(col("age").add(1).gt(11).not_()).collect();
    println(out2.height());
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the ReleaseLazyExpr/ReleaseLazyPlan drains, retain-on-return, \
             and the collect control-block FreeDataFrame path",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec![
            "== logical plan ==",
            "LIMIT 3",
            "  SELECT [name, opt]",
            "    FILTER (((age > 10) and (age < 40)) or (score >= 7))",
            "      SCAN [age, name, score, opt]",
            "== optimized ==",
            "SELECT [name, opt]",
            "  LIMIT 3",
            "    FILTER (((age > 10) and (age < 40)) or (score >= 7))",
            "      SCAN cols=[age, name, score, opt]",
            "3",
            "ada_padding_aaaaaaaaaaaaaaaaa",
            "null",
            "1"
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// Full-surface LazyFrame twin heap lifecycle (sort / group_by+agg /
/// join / with_columns — the ops ported after the MVP twin). Exercises
/// every new runtime allocation class: `Vec[LazyExpr]` key/agg/entry
/// lists (packed handle words — each element a tracked +1 handle),
/// the `LazyGroupBy` intermediate (its OWN handle type released via
/// `karac_lazy_gb_release`), the nested right sub-plan `Arc` a JOIN
/// stores, String group keys + String aggregates (min over Strings),
/// NULL agg slots (all-null sum group), join fan-out gathers over
/// String join keys, and with_columns computed columns (fold inside
/// entries + a same-named replace + a String rename copy). A missed
/// release leaks (Linux detect_leaks); an over-release double-frees
/// the `Arc` (caught everywhere).
#[test]
fn asan_lazyframe_full_surface_no_leak() {
    let label = "lazyframe_full_surface_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
import std.lazy.{col, lit};
fn main() {
    let mut df: DataFrame = DataFrame.new();
    df.insert("city", Column.from_vec(["oslo_padding_aaaaaaaaaaaaaaa", "rome", "oslo_padding_aaaaaaaaaaaaaaa", "rome", "lima"]));
    let nn: Vec[Option[i64]] = vec![Some(10i64), None, Some(30i64), Some(40i64), None];
    df.insert("pop", Column.from_iter_nullable(nn));
    df.insert("score", Column.from_vec([1.5, 2.5, 3.5, 4.5, 5.5]));
    // Stable multi-key sort: String key asc, nullable i64 key desc (NULLs last).
    let sp = df.lazy().sort(vec![col("city"), col("pop").desc()]).limit(4);
    let s = sp.collect();
    println(s.height());
    // group_by on String keys; count/sum over a nullable col (one all-null
    // group), mean, min over Strings, max — alias_ + default output names.
    let gp = df.lazy()
        .group_by(vec![col("city")])
        .agg(vec![col("pop").count().alias_("cnt"), col("pop").sum(), col("score").mean(), col("city").min(), col("score").max()]);
    println(gp.explain());
    let g = gp.collect();
    println(g.width()); println(g.height());
    let cnts: Column[i64] = g.column("cnt");
    match cnts[0] { Some(v) => println(v), None => println(-1i64) }
    let sums: Column[i64] = g.column("pop_sum");
    match sums[2] { Some(v) => println(v), None => println("null-sum") }
    let mins: Column[String] = g.column("city_min");
    match mins[1] { Some(v) => println(v), None => println("null") }
    // Inner join on String keys with fan-out (duplicate right matches) and
    // a pushed-down left select; right sub-plan is a nested Arc'd plan.
    let mut dup: DataFrame = DataFrame.new();
    dup.insert("city", Column.from_vec(["rome", "rome", "oslo_padding_aaaaaaaaaaaaaaa"]));
    dup.insert("tag", Column.from_vec(["a_padding_bbbbbbbbbbbbbbbbbbb", "b", "c"]));
    let jp = df.lazy().select(vec!["city", "score"]).join(dup.lazy(), vec!["city"]);
    let j = jp.collect();
    println(j.height());
    let tags: Column[String] = j.column("tag");
    match tags[0] { Some(v) => println(v), None => println("null") }
    // with_columns: computed col (constant folding inside the entry), a
    // same-named replace, a String rename copy; NULL propagates.
    let wp = df.lazy()
        .with_columns(vec![col("pop").mul(lit(2).add(1)).alias_("pop3"), col("score").add(0.5).alias_("score"), col("city").alias_("label")])
        .filter(col("pop3").gt(29));
    let w = wp.collect();
    println(w.width()); println(w.height());
    let p3: Column[i64] = w.column("pop3");
    match p3[0] { Some(v) => println(v), None => println("null") }
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the ReleaseLazyExpr/ReleaseLazyPlan/ReleaseLazyGroupBy \
             drains, the Vec[LazyExpr] owned-temp path, and the collect \
             control-block FreeDataFrame path",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec![
            "4",
            "== logical plan ==",
            "GROUP BY [city] AGG [count(pop) as cnt, sum(pop), mean(score), min(city), max(score)]",
            "  SCAN [city, pop, score]",
            "== optimized ==",
            "GROUP BY [city] AGG [count(pop) as cnt, sum(pop), mean(score), min(city), max(score)]",
            "  SCAN cols=[city, pop, score]",
            "6",
            "3",
            "2",
            "null-sum",
            "rome",
            "6",
            "c",
            "5",
            "3",
            "30"
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// Column transform heap lifecycle (phase-11 follow-on slice): the
/// Column-returning transforms `fillna` / `dropna` and the
/// `from_iter_nullable` constructor each malloc a *fresh* control
/// block + data buffer + bitmap; the receiver is borrowed (keeps its
/// own `FreeColumn`), and the fresh result is freed once via the
/// let-binding's `FreeColumn`. A missing free leaks (Linux
/// detect_leaks); a wrong free double-frees (caught everywhere).
/// Receiver reuse after the transforms pins it wasn't consumed.
#[test]
fn asan_column_transforms_lifecycle_clean() {
    let label = "column_transforms_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let mut c: Column[i64] = Column.new();
    c.push(10);
    c.push_null();
    c.push(30);
    c.push_null();
    let f = c.fillna(99);
    println(f.len());
    let d = c.dropna();
    println(d.len());
    // receiver still usable (borrowed, not consumed).
    println(c.null_count());
    let opts: Vec[Option[i64]] = [Some(1), None, Some(3)];
    let e: Column[i64] = Column.from_iter_nullable(opts);
    println(e.null_count());
    match e[2] { Some(v) => println(v), None => println(-1) }
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check FreeColumn on fresh transform results / receiver-borrow",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["4", "2", "2", "1", "3"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// Column `fillna(value, treat_nan_as_null)` float-NaN normalization heap
/// lifecycle (phase-11 follow-on): each `fillna` mallocs a fresh control
/// block + data buffer + all-ones bitmap, freed once via the let-binding's
/// `FreeColumn`; the float receiver is borrowed and reused after both
/// transforms. The NaN-normalizing arm doesn't change the allocation
/// shape, so this pins the float fill loop frees cleanly too.
#[test]
fn asan_column_fillna_nan_lifecycle_clean() {
    let label = "column_fillna_nan_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let z: f64 = 0.0;
    let nan: f64 = z / z;
    let mut c: Column[f64] = Column.new();
    c.push(1.5);
    c.push_null();
    c.push(nan);
    c.push(4.0);
    let a = c.fillna(0.0);
    println(a.null_count());
    let b = c.fillna(0.0, treat_nan_as_null: true);
    println(b.null_count());
    // receiver still usable (borrowed, not consumed).
    println(c.null_count());
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check FreeColumn on the fresh fillna results / receiver-borrow",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["0", "0", "1"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// Column Vec-returning iterators heap lifecycle (phase-11 follow-on):
/// `iter() -> Vec[Option[T]]` and `iter_valid() -> Vec[T]` each malloc
/// a fresh Vec buffer (POD elements — no per-element drop), freed once
/// via the result binding's `FreeVecBuffer` / the for-loop's owned-temp
/// materialization. The source column is borrowed (keeps its own
/// `FreeColumn`). Both the let-bound and direct-for-source forms run;
/// a missing free leaks (Linux detect_leaks), a wrong free double-frees.
#[test]
fn asan_column_iter_lifecycle_clean() {
    let label = "column_iter_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let mut c: Column[i64] = Column.new();
    c.push(10);
    c.push_null();
    c.push(30);
    let all: Vec[Option[i64]] = c.iter();
    println(all.len());
    let mut sum = 0;
    for o in all { match o { Some(v) => { sum = sum + v; }, None => { sum = sum - 1; } } }
    println(sum);
    let valid: Vec[i64] = c.iter_valid();
    println(valid.len());
    let mut vs = 0;
    for x in c.iter_valid() { vs = vs + x; }
    println(vs);
    // source column still usable (borrowed, not consumed).
    println(c.null_count());
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check FreeVecBuffer on the iter results / column borrow",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["3", "39", "2", "40", "1"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// Column 3VL-arithmetic heap lifecycle (phase-11 follow-on): every
/// element-wise `+ - * /` / comparison / unary `-` mallocs a fresh
/// result column; operands are borrowed (keep their own `FreeColumn`),
/// and a fresh-temp intermediate in `a + b + c` / `-a` chains is freed
/// after the copy (`column_free_if_fresh_temp`) — a missing free leaks
/// (Linux detect_leaks), a wrong free double-frees. Operand reuse after
/// the ops pins that nothing was wrongly consumed.
///
/// B-2026-08-08-18 — moved onto the AUTO-PAR lane, which is the default
/// configuration and the one this fixture never ran in. On the sequential
/// lane it had been green throughout; with the parallelizer on, `karac
/// build` refused the program outright (a published `Column` binding was
/// sized as an `i64` return slot, so its control-block pointer reached a
/// `ptr` parameter as an integer and LLVM verification rejected the call).
/// Keeping it here rather than adding a second copy: the lifecycle
/// behaviour it pins is the same on both lanes, and the default lane is the
/// one worth pinning it on.
#[test]
fn asan_column_arithmetic_lifecycle_clean() {
    let label = "column_arithmetic_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan_with_concurrency(
        r#"
fn fst(c: Column[i64], i: i64) -> i64 { match c[i] { Some(v) => v, None => -1 } }
fn main() {
    let mut a: Column[i64] = Column.new();
    a.push(10); a.push_null(); a.push(30);
    let mut b: Column[i64] = Column.new();
    b.push(1); b.push(2); b.push(3);
    // chained col-col: a + b + b — the (a + b) intermediate is a fresh
    // temp freed after the second op.
    let s = a + b + b;
    println(fst(s, 2));
    // col-scalar + unary neg chain.
    let m = -(a * 2);
    println(fst(m, 0));
    // comparison -> fresh Column[bool].
    let eq = a == b;
    println(eq.null_count());
    // operands still usable (borrowed, not consumed).
    println(a.null_count());
    println(b.len());
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check FreeColumn on fresh 3VL results / fresh-temp operand free",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["36", "-20", "1", "1", "3"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// Column statistical reductions heap lifecycle (phase-11 stats codegen
/// slice): the scalar reductions (`sum`/`mean`/`var`/`std`/`min`/`max`)
/// allocate no heap and only read the column's buffers, so the column
/// stays intact and is freed once at scope exit. `corr`'s argument may be
/// a *fresh-temp* column (`a.corr(a + a)`) — the temporary is freed after
/// the read via `column_free_if_fresh_temp` (a missing free leaks on
/// Linux detect_leaks; a wrong free double-frees). The receiver stays
/// borrowed and usable after every call.
#[test]
fn asan_column_stats_lifecycle_clean() {
    let label = "column_stats_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let a: Column[f64] = Column.from_vec([2.0, 4.0, 6.0]);
    // Scalar reductions allocate no heap; the column is untouched.
    println(a.sum());
    println(a.mean());
    println(a.var());
    println(a.std());
    // corr with a fresh-temp argument (a + a) — the temporary column is
    // freed after the read; `a` is borrowed.
    println(a.corr(a + a));
    // a still usable after all of the above (borrowed, not consumed).
    println(a.min());
    println(a.max());
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check corr's fresh-temp arg free / reductions not freeing the receiver",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["12", "4", "4", "2", "1", "2", "6"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// Column `median` / `quantile` heap lifecycle (phase-11 stats codegen
/// slice 3): each call mallocs a fresh `f64` scratch buffer, sorts it
/// in place, reads the interpolated result, then frees the buffer — a
/// missing free leaks (Linux detect_leaks), a double-free or read past
/// the free trips ASAN. Several calls in a row on the same (borrowed,
/// untouched) column pin allocate-sort-free balance across iterations.
#[test]
fn asan_column_median_quantile_lifecycle_clean() {
    let label = "column_median_quantile_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let c: Column[f64] = Column.from_vec([4.0, 1.0, 3.0, 2.0, 5.0]);
    // Repeated median/quantile — each mallocs + frees its own scratch buffer.
    println(c.median());
    println(c.quantile(0.25));
    println(c.quantile(0.75));
    // Null-skipping median over an integer column (separate buffer).
    let o: Column[i64] = Column.from_iter_nullable([Some(9), None, Some(1), Some(5)]);
    println(o.median());
    // c still usable afterward (borrowed, not consumed).
    println(c.len());
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the median/quantile sort-buffer malloc/free balance",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["3", "2", "4", "5", "5"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// `Stats.*` free-function codegen lifecycle (phase-11). `Stats.median`
/// mallocs + frees a scratch f64 buffer per call (memcpy-sort-read-free);
/// a fresh `vec![…]` temp argument is read and then freed via
/// `materialize_owned_temp` (the early-dispatch owned-temp leak guard —
/// `builtin-method-early-dispatch-skips-owned-temp-arg-free`). A borrowed
/// `Vec` argument is NOT consumed (still usable after). A missing free
/// leaks (Linux detect_leaks); a double free trips ASAN everywhere.
#[test]
fn asan_stats_free_functions_lifecycle_clean() {
    let label = "stats_free_functions_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let v: Vec[f64] = vec![4.0, 1.0, 3.0, 2.0, 5.0];
    // median mallocs/frees a scratch buffer; the others read in place.
    println(Stats.median(v));
    println(Stats.mean(v));
    println(Stats.stddev(v));
    // v borrowed, not consumed — still usable.
    println(Stats.sum(v));
    // fresh vec![…] temp argument: read then freed (no leak).
    println(Stats.median(vec![30.0, 10.0, 20.0, 40.0]));
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the median scratch malloc/free + fresh-temp arg free",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["3", "3", "1.4142135623730951", "15", "25"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// `Stats.percentile`/`sort`/`argsort` codegen lifecycle (phase-11).
/// `percentile` mallocs + frees an f64 scratch (memcpy-sort-read-free).
/// `sort` / `argsort` each malloc a buffer and hand it back as an OWNED
/// `Vec` whose `let`-binding frees it at scope exit — a missing free leaks
/// (Linux detect_leaks), a double free trips ASAN. The source `Vec` is
/// borrowed (still usable after), and a fresh `vec![…]` temp argument is
/// freed via `materialize_owned_temp`.
#[test]
fn asan_stats_methods_lifecycle_clean() {
    let label = "stats_methods_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let v: Vec[f64] = vec![40.0, 10.0, 30.0, 20.0, 50.0];
    println(Stats.percentile(v, 50.0));
    let s: Vec[f64] = Stats.sort(v);
    println(s[0]);
    println(s[4]);
    let a: Vec[i64] = Stats.argsort(v);
    println(a[0]);
    println(a[4]);
    // fresh vec![…] temp argument: read then freed (no leak).
    let s2: Vec[f64] = Stats.sort(vec![3.0, 1.0, 2.0]);
    println(s2[0]);
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the percentile scratch + sort/argsort owned-Vec free",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["30", "10", "50", "1", "4", "1"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// `Column[String]` heap-element lifecycle (phase-11 Column[String]
/// codegen slice): each String column owns its element heaps. `from_vec`
/// deep-clones the source Vec's strings IN (the source Vec is borrowed and
/// drops its own); `iter_valid` deep-clones them OUT into a fresh
/// `Vec[String]` (which drops its own); the `FreeColumn` drain frees every
/// valid slot's String (cap-guarded) before the buffers; a move
/// (`let d = c`) nulls the source slot so only the new owner frees. A
/// missing free leaks (Linux detect_leaks), a double free / use-after-free
/// trips ASAN everywhere. The moved column's reuse pins that the move
/// transferred ownership cleanly (no double free of the shared heaps).
#[test]
fn asan_column_string_lifecycle_clean() {
    let label = "column_string_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    // Long payloads (>= 23 bytes) force real heap allocation (LSan misses
    // short reachable strings) — see lsan-reachability-short-string-leaks.
    // The source Vec is moved into from_vec (ownership), but its scope drop
    // still frees its own element strings; from_vec deep-clones independent
    // copies into the column, so the two never share a heap.
    let v: Vec[String] = ["alpha_aaaaaaaaaaaaaaaaaaaaaaaa", "beta_bbbbbbbbbbbbbbbbbbbbbbbbb", "gamma_ccccccccccccccccccccccc"];
    let c: Column[String] = Column.from_vec(v);
    // iter_valid clones out into a fresh Vec[String] (dropped after the loop).
    for s in c.iter_valid() { println(s.len()); }
    // Move the column: `d` owns the heaps, `c`'s FreeColumn is suppressed.
    let d = c;
    println(d.valid_count());
    for s in d.iter_valid() { println(s.len()); }
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check from_vec clone-in / iter_valid clone-out / FreeColumn String drain / move",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["30", "30", "29", "3", "30", "30", "29"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

#[test]
fn asan_narrow_literal_arg_sink_buffers() {
    // B-2026-07-02-6: collection literals compiled directly at call-arg
    // sinks (by-value `Vec[i32]`, borrow-only `ref Vec[i32]` and
    // `Slice[i32]` params, bare `[v; n]` repeat in arg position) each
    // malloc a heap buffer at the call site; the borrow sinks must free
    // the temp after the call (the callee never owns it) and the
    // by-value sink's callee-drop must fire exactly once.
    assert_clean_asan_run(
        r#"
fn by_val(v: Vec[i32]) -> i64 {
    let mut t = 0;
    for x in v {
        t = t + (x as i64);
    }
    return t;
}

fn by_ref(v: ref Vec[i32]) -> i64 {
    let mut t = 0;
    for x in v {
        t = t + (x as i64);
    }
    return t;
}

fn by_slice(v: Slice[i32]) -> i64 {
    let mut t = 0;
    for x in v {
        t = t + (x as i64);
    }
    return t;
}

struct Acc {
    base: i64,
}

impl Acc {
    fn tally(self, v: Vec[i32]) -> i64 {
        let mut t = self.base;
        for x in v {
            t = t + (x as i64);
        }
        return t;
    }
}

fn main() {
    let a = by_val([10, 20, 30]);
    let b = by_ref([10, 20, 30]);
    let c = by_slice([10, 20, 30]);
    let d = by_val([7; 3]);
    let acc = Acc { base: 0 };
    let e = acc.tally([1, 2, 3]);
    println(a + b + c + d + e);
}
"#,
        &["207"],
        "narrow_literal_arg_sink_buffers",
    );
}

#[test]
fn asan_column_sorted_argsort_narrow_widths_no_leak() {
    // S6c follow-on: the NARROW-width `Column.sorted` path mallocs a separate
    // `Vec[T]`-width buffer and frees the 8-byte scratch key buffer; the
    // narrow `argsort` path mallocs a widened full-length key view and frees
    // it after the sort. This asserts neither the narrow-back buffer, the
    // widened key view, nor the result `Vec` leaks or double-frees over a
    // loop — for i32, u32, and f32 columns (each with a null). `let`-bound +
    // indexed idiom (the standard `Stats.sort` idiom).
    assert_clean_asan_run(
        r#"
fn inner() -> i64 {
    let mut ci: Column[i32] = Column.with_capacity(4);
    ci.push(5); ci.push(1); ci.push_null(); ci.push(3);
    let cs: Vec[i32] = ci.sorted();
    let ca: Vec[i64] = ci.argsort();
    let cu: Column[u32] = Column.from_vec([30, 10, 20]);
    let us: Vec[u32] = cu.sorted();
    let ua: Vec[i64] = cu.argsort();
    let mut cf: Column[f32] = Column.with_capacity(4);
    cf.push(2.5); cf.push_null(); cf.push(1.5); cf.push(0.5);
    let fs: Vec[f32] = cf.sorted();
    let fa: Vec[i64] = cf.argsort();
    cs.len() + ca[0] + us.len() + ua[0] + fs.len() + fa[0]
}
fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        acc = acc + inner();
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
        // cs.len()=3, ca[0]=1, us.len()=3, ua[0]=1, fs.len()=3, fa[0]=3 → 14;
        // *20 = 280.
        &["280"],
        "asan_column_sorted_argsort_narrow_widths_no_leak",
    );
}
