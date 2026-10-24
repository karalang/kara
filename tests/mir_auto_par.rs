//! Automatic parallelism in the builder (M4 item 14): under
//! `KARAC_AUTO_PAR=1` a `for` over `a..b` whose verdict says its iterations
//! may run apart is cut into runs of iterations, each a task of one group,
//! joined in order after the loop, a reduction folded at the join. The
//! program's output, exit code and panic message are the sequential
//! program's. Checked on the MIR interpreter and on the built binary.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

fn source(dir: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-auto-par-{dir}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    (dir, file)
}

fn karac(auto: bool) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_HASH_SEED", "1");
    if auto {
        c.env("KARAC_AUTO_PAR", "1")
            .env("KARAC_AUTO_PAR_DEBUG", "1");
    } else {
        c.env_remove("KARAC_AUTO_PAR");
    }
    c
}

/// `karac __mir-run`, sequential or with auto-par.
fn interp_run(name: &str, src: &str, auto: bool) -> Output {
    let (dir, file) = source(name, src);
    let out = karac(auto)
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// `karac build --mir` and the binary run, or `None` when the runtime
/// archive does not link; the build's stderr comes back with it.
fn build_and_run(name: &str, src: &str, auto: bool) -> Option<(Output, String)> {
    let (dir, file) = source(name, src);
    let exe = dir.join("main");
    let built = karac(auto)
        .args([
            "build",
            "--mir",
            file.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    assert!(
        built.status.success() || err.contains("Linker failed"),
        "{name} does not build: {err}"
    );
    common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err.clone())
    })?;
    let out = Command::new(&exe).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    Some((out, err))
}

/// What a run shows: stdout, exit code, and stderr without the auto-par
/// report.
fn shown(o: &Output) -> (String, Option<i32>, String) {
    let err: Vec<&str> = std::str::from_utf8(&o.stderr)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with("auto-par:"))
        .collect();
    (
        String::from_utf8(o.stdout.clone()).unwrap(),
        o.status.code(),
        err.join("\n"),
    )
}

/// The loops the auto-par build ran as tasks, from its report: each
/// `instance@line (n reductions)`.
fn tasked(stderr: &[u8]) -> Vec<String> {
    std::str::from_utf8(stderr)
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("auto-par: loops as tasks: "))
        .map(|l| l.split(", ").map(String::from).collect())
        .unwrap_or_default()
}

/// `got` is one loop per entry of `want`, each that entry, a `*` in it
/// standing for anything (a function value's instance name).
fn assert_loops(got: &[String], want: &[&str], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: {got:?}");
    for w in want {
        let hit = |g: &String| match w.split_once('*') {
            Some((a, b)) => g.starts_with(a) && g.ends_with(b),
            None => g == w,
        };
        assert!(got.iter().any(hit), "{what}: {w} in {got:?}");
    }
}

/// Runs `src` both ways on both backends: the same output each time, and
/// the auto-par builds report `loops` as the loops they ran as tasks.
fn same_both_ways(name: &str, src: &str, loops: &[&str]) -> (String, Option<i32>, String) {
    let seq = interp_run(name, src, false);
    let par = interp_run(name, src, true);
    assert_eq!(shown(&seq), shown(&par), "{name}: interpreter");
    assert_loops(&tasked(&par.stderr), loops, name);
    if let (Some((seq_b, _)), Some((par_b, built))) = (
        build_and_run(name, src, false),
        build_and_run(name, src, true),
    ) {
        assert_eq!(shown(&seq_b), shown(&par_b), "{name}: binary");
        assert_eq!(shown(&seq), shown(&seq_b), "{name}: interpreter and binary");
        assert_loops(&tasked(built.as_bytes()), loops, name);
    }
    shown(&seq)
}

/// Reductions of every operator over several integer types, in methods
/// and generic functions, with an empty range, a negative one and fewer
/// iterations than tasks; a loop that prints keeps its order, and a
/// closure's loop stays sequential, since the closure would suspend while
/// called through a function value.
#[test]
fn mir_auto_par_reductions_match_the_sequential_loop() {
    let src = r#"struct Grid {
    cells: Vec[i64],
    w: i64,
}

impl Grid {
    fn row_sum(ref self, r: i64) -> i64 {
        let mut s = 0;
        for c in 0..self.w {
            s += self.cells[(r * self.w + c) as usize];
        }
        s
    }
}

fn weigh[T](xs: ref Vec[T], f: Fn(ref T) -> i64) -> i64 {
    let mut s = 0;
    for i in 0..xs.len() {
        s = f(xs[i]) + s;
    }
    s
}

fn len_of(s: ref String) -> i64 { s.len() as i64 }

fn main() {
    let g = Grid { cells: Vec.filled(100, 3), w: 10 };
    println(g.row_sum(4));
    let ws: Vec[String] = ["a".to_string(), "abc".to_string(), "abcd".to_string()];
    println(weigh(ws, len_of));
    let mut sq: i32 = 0;
    for i in -5i32..3i32 {
        sq = sq + i * i;
    }
    println(sq);
    let mut none = 7;
    for i in 10..3 {
        none += i;
    }
    println(none);
    let mut p: u64 = 1;
    for i in 1..15 {
        p *= i as u64;
    }
    println(p);
    let mut bits: u8 = 255;
    for i in 0..40 {
        let b: u8 = if i % 7 == 6 { 254 } else { 255 };
        bits = bits & b;
    }
    let mut any: u16 = 0;
    for i in 0..20 {
        any = any | (1u16 << (i % 9) as u16);
    }
    let mut x = 0;
    for i in 0..1000 {
        x = x ^ (i * 3);
    }
    println(f"{bits} {any} {x}");
    for i in 0..3 {
        println(i);
    }
    let f = |n: i64| {
        let mut s = 0;
        for i in 0..n {
            s += i;
        }
        s
    };
    println(f(100));
}
"#;
    let (out, code, _) = same_both_ways(
        "reductions",
        src,
        &[
            "Grid.row_sum@9",
            "main@32",
            "main@37",
            "main@42",
            "main@47",
            "main@52",
            "main@56",
            "weigh[String]*@18",
        ],
    );
    assert_eq!(code, Some(0));
    assert_eq!(
        out,
        "30\n8\n60\n7\n87178291200\n254 511 3176\n0\n1\n2\n4950\n"
    );
}

/// An overflow folded at the join panics as the sequential loop does,
/// naming the loop's operation, for `+=` and for `acc = acc + e`.
#[test]
fn mir_auto_par_overflow_panics_where_the_loop_does() {
    for (name, op) in [("compound", "s += 1;"), ("plain", "s = s + 1;")] {
        let src = format!(
            "fn main() {{\n    let mut s: i8 = 0;\n    for i in 0..200 {{\n        {op}\n    }}\n    println(s);\n}}\n"
        );
        let (out, code, err) = same_both_ways(name, &src, &["main@3"]);
        assert_eq!(out, "");
        assert_eq!(code, Some(101), "{err}");
        assert!(err.contains("overflow"), "{err}");
    }
}

/// Iterations that wait run at once: eight 100 ms sleeps take about one
/// sleep in the auto-par binary and all eight in the sequential one.
#[test]
fn mir_auto_par_iterations_wait_concurrently() {
    let src = r#"fn slow(i: i64) -> i64 {
    sleep_ms(100);
    i * 2
}

fn main() {
    let mut total = 0;
    for i in 0..8 {
        total += slow(i);
    }
    println(total);
}
"#;
    let timed = |auto: bool| {
        let (dir, file) = source(&format!("t-{auto}"), src);
        let exe = dir.join("main");
        let built = karac(auto)
            .args([
                "build",
                "--mir",
                file.to_str().unwrap(),
                "-o",
                exe.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&built.stderr).to_string();
        common::link_or_skip(if built.status.success() {
            Ok(())
        } else {
            Err(err.clone())
        })?;
        assert_loops(
            &tasked(err.as_bytes()),
            if auto { &["main@8"] } else { &[] },
            "sleeps",
        );
        let start = std::time::Instant::now();
        let out = Command::new(&exe).output().unwrap();
        let took = start.elapsed();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(String::from_utf8(out.stdout).unwrap(), "56\n");
        Some(took)
    };
    let (Some(seq), Some(par)) = (timed(false), timed(true)) else {
        return;
    };
    assert!(seq >= std::time::Duration::from_millis(800), "{seq:?}");
    assert!(par < std::time::Duration::from_millis(500), "{par:?}");
}
