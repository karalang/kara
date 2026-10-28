//! Two ways an auto-par build ran a function wrong, found by a corpus sweep
//! of the default (threads and auto-par on) against `KARAC_THREADS=1
//! KARAC_AUTO_PAR=0`.
//!
//! B-2026-10-28-4: a function with a `mut ref` parameter that calls an
//! auto-par site handed the parameter to its in-order twin (`<name>#seq`) as
//! a copy, which a `mut ref` is not: "invalid MIR: tally: bb3[term]: copy of
//! _2 whose type mut ref i64 is not Copy", on every backend.
//!
//! B-2026-10-28-5: `extend_from_slice` (and the `try_` writers) borrowed its
//! receiver shared, so auto-par saw five appends to one buffer as reads and
//! ran them as tasks at once: the proto3 encoder below crashed or decoded
//! garbage in about 1 run in 160 on several workers. And check let a `ref`
//! parameter be appended to.

use std::path::PathBuf;
use std::process::{Command, Output};

const TWIN: &str = r#"fn squares(n: i64) -> Vec[Vec[i64]] {
    let mut out: Vec[Vec[i64]] = Vec.new();
    let mut i = 0;
    while i < n {
        let mut row: Vec[i64] = Vec.new();
        let mut j = 0;
        while j < i {
            row.push(i * j);
            j = j + 1;
        }
        out.push(row);
        i = i + 1;
    }
    out
}

fn tally(n: i64, acc: mut ref i64) {
    let rows = squares(n);
    *acc = *acc + rows.len();
}

fn main() {
    let mut acc = 0;
    tally(5, mut acc);
    tally(7, mut acc);
    println(acc);
}
"#;

const PROTO: &str = r#"#[proto_schema]
const SCHEMA: String = "
    syntax = \"proto3\";
    message Point { int64 x = 1; int64 y = 2; }
    message Doc {
        string title = 1;
        map<string, int64> counts = 2;
        map<int32, Point> points = 3;
    }
";

fn main() {
    let mut counts: Map[String, i64] = Map.new();
    counts.insert("a", 10);
    counts.insert("b", 20);
    let mut points: Map[i32, Point] = Map.new();
    points.insert(1, Point { x: 5, y: 6 });
    let d = Doc { title: "t", counts: counts, points: points };
    let back = Doc.decode(d.encode());
    println(back.title);
    println(back.counts.len());
    println(match back.counts.get("a") { Option.Some(x) => x, Option.None => -1 });
    println(match back.points.get(1) { Option.Some(p) => p.x, Option.None => -1 });
}
"#;

fn write(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-auto-par-writes-{}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(env: &[(&str, &str)], args: &[&str], path: &PathBuf) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env_remove("KARAC_AUTO_PAR");
    for (k, v) in env {
        c.env(k, v);
    }
    c.args(args).arg(path).output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn mir_auto_par_twin_takes_a_mut_ref_param_in_the_interpreter() {
    let path = write("twin-interp", TWIN);
    for threads in ["1", "4"] {
        let out = karac(&[("KARAC_THREADS", threads)], &["__mir-run"], &path);
        assert_eq!(
            text(&out.stdout),
            "12\n",
            "KARAC_THREADS={threads}: {}",
            text(&out.stderr)
        );
    }
}

#[cfg(feature = "llvm")]
#[test]
fn mir_auto_par_twin_takes_a_mut_ref_param_compiled() {
    let path = write("twin-llvm", TWIN);
    for threads in ["1", "4"] {
        let out = karac(&[("KARAC_THREADS", threads)], &["__mir-llvm-run"], &path);
        let err = text(&out.stderr);
        if err.contains("Linker failed")
            && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none()
        {
            eprintln!("skipping: no runtime archive");
            return;
        }
        assert_eq!(text(&out.stdout), "12\n", "KARAC_THREADS={threads}: {err}");
    }
}

/// The encoder's appends all write `out`, so they stay in order.
#[test]
fn mir_auto_par_keeps_appends_to_one_buffer_in_order() {
    let path = write("proto", PROTO);
    let out = karac(&[("KARAC_AUTO_PAR_DEBUG", "1")], &["__mir-run"], &path);
    let err = text(&out.stderr);
    assert_eq!(text(&out.stdout), "t\n2\n10\n5\n", "{err}");
    assert!(!err.contains("statements as tasks: Doc.encode"), "{err}");
    assert!(
        err.contains(
            "auto-par: in order: Doc.encode@10: Dependence { first: 0, second: 1, local: \"out\" }"
        ),
        "{err}"
    );
}

#[test]
fn ref_param_append_refused() {
    for (tag, call) in [
        ("efs", "v.extend_from_slice(w)"),
        ("try-push", "v.try_push(1)"),
    ] {
        let src = format!(
            "fn add(v: ref Vec[i64], w: ref Vec[i64]) {{\n    let _ = {call};\n}}\n\nfn main() {{\n    let mut v: Vec[i64] = Vec.new();\n    let w: Vec[i64] = vec![1, 2];\n    add(v, w);\n    println(v.len());\n}}\n"
        );
        let path = write(tag, &src);
        let out = karac(&[], &["check"], &path);
        let err = text(&out.stderr);
        assert!(!out.status.success(), "{tag}: check must refuse:\n{err}");
        assert!(
            err.contains("`v` is a `ref` parameter, so `v.")
                && err.contains("(a method that writes its receiver) cannot modify it"),
            "{tag}: {err}"
        );
    }
}
