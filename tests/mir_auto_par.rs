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

/// Reductions of every operator but a checked `*` over several integer
/// types, in methods and generic functions, with an empty range, a
/// negative one and fewer iterations than tasks; a product, a loop that
/// prints, and a closure's loop (which would suspend while called through
/// a function value) keep their order.
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

/// A signed sum whose runs overflow on their own (`i64.max`, five 1s, then
/// 100000 -1s) gives the sequential sum, and a product whose later runs
/// overflow after an earlier 0 stays sequential.
#[test]
fn mir_auto_par_sums_overflow_only_where_the_loop_does() {
    let src = r#"fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(9223372036854775807);
    for _ in 0..5 {
        v.push(1);
    }
    for _ in 0..100000 {
        v.push(-1);
    }
    let mut acc = -10;
    for i in 0..v.len() {
        acc = acc + v[i];
    }
    println(f"{acc}");
    let mut w: Vec[u64] = Vec.new();
    w.push(0);
    for _ in 0..40 {
        w.push(4000000000);
    }
    let mut p: u64 = 1;
    for i in 0..w.len() {
        p *= w[i];
    }
    println(p);
}
"#;
    let (out, code, err) = same_both_ways("wide", src, &["main@11"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, "9223372036854675802\n0\n");
    // The sum overflows where the in-order sum does, not in a run.
    let over = src.replace("let mut acc = -10;", "let mut acc = 10;");
    let (out, code, err) = same_both_ways("wide-over", &over, &["main@11"]);
    assert_eq!(out, "");
    assert_eq!(code, Some(101), "{err}");
    assert!(err.contains("overflow"), "{err}");
}

/// A reduction under an `if` or in a `match` arm folds as one at the top
/// does, a signed sum's overflow included; two statements writing the
/// same accumulator keep the loop in order.
#[test]
fn mir_auto_par_reductions_under_if_and_match() {
    let src = r#"fn pick(i: i64) -> Option[i64] {
    if i % 3 == 0 { Some(i * 2) } else { None }
}

fn main() {
    let mut s = 0;
    for i in 0..3000 {
        match pick(i) {
            Some(v) => { s = s + v; }
            None => {}
        }
    }
    let mut hits = 0;
    for c in 0..500 {
        if c % 7 == 1 { hits = hits + 1; }
    }
    let mut odd: u64 = 0;
    for i in 0..400 {
        if i % 2 == 1 {
            odd += i as u64;
        } else if i % 5 == 0 {
            odd += 1;
        }
    }
    println(f"{s} {hits} {odd}");
    let mut v: Vec[i64] = Vec.new();
    v.push(9223372036854775807);
    for _ in 0..500 {
        v.push(-1);
    }
    let mut acc = START;
    for i in 0..v.len() {
        if v[i] != 0 {
            acc += v[i];
        }
    }
    println(acc);
}
"#;
    let ok = src.replace("START", "-10");
    let (out, code, err) = same_both_ways("nested", &ok, &["main@7", "main@14", "main@32"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, "2997000 72 40040\n9223372036854775297\n");
    let over = src.replace("START", "10");
    let (out, code, err) = same_both_ways("nested-over", &over, &["main@7", "main@14", "main@32"]);
    assert_eq!(out, "2997000 72 40040\n");
    assert_eq!(code, Some(101), "{err}");
    assert!(err.contains("overflow"), "{err}");
}

/// A chain of one operator holding the accumulator once folds as a single
/// operation does (`s + i + 1`, `i + t + 1 + i * 2`, `m | a | b`, an
/// unsigned `u + a + 3`, a signed `v[i] + acc + 0` overflowing where the
/// loop does); `i + (r + 1)`, whose overflow would sit inside the
/// statement, keeps its order.
#[test]
fn mir_auto_par_operator_chains_fold() {
    let src = r#"fn main() {
    let mut s = 0;
    for i in 0..1000 {
        s = s + i + 1;
    }
    let mut t = 5;
    for i in 0..1000 {
        t = i + t + 1 + i * 2;
    }
    let mut m: u32 = 0;
    for i in 0..30 {
        m = m | (i as u32) | ((i as u32) * 4);
    }
    let mut r = 0;
    for i in 0..100 {
        r = i + (r + 1);
    }
    let mut u: u64 = 0;
    for i in 0..300 {
        u = u + (i as u64) + 3;
    }
    println(f"{s} {t} {m} {r} {u}");
    let mut v: Vec[i64] = Vec.new();
    v.push(9223372036854775807);
    for _ in 0..500 {
        v.push(-1);
    }
    let mut acc = START;
    for i in 0..v.len() {
        acc = v[i] + acc + 0;
    }
    println(acc);
}
"#;
    let loops = ["main@3", "main@7", "main@11", "main@19", "main@29"];
    let ok = src.replace("START", "-10");
    let (out, code, err) = same_both_ways("chain", &ok, &loops);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, "500500 1499505 127 5050 45750\n9223372036854775297\n");
    let over = src.replace("START", "10");
    let (out, code, err) = same_both_ways("chain-over", &over, &loops);
    assert_eq!(out, "500500 1499505 127 5050 45750\n");
    assert_eq!(code, Some(101), "{err}");
    assert!(err.contains("overflow"), "{err}");
}
