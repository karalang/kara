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
    let loops = ["main@4", "main@7", "main@11", "main@17"];
    let (out, code, err) = same_both_ways("wide", src, &loops);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, "9223372036854675802\n0\n");
    // The sum overflows where the in-order sum does, not in a run.
    let over = src.replace("let mut acc = -10;", "let mut acc = 10;");
    let (out, code, err) = same_both_ways("wide-over", &over, &loops);
    assert_eq!(out, "");
    assert_eq!(code, Some(101), "{err}");
    assert!(err.contains("overflow"), "{err}");
}

/// A reduction under an `if` or in a `match` arm folds as one at the top
/// does, a signed sum's overflow included, and so do two statements
/// writing the same accumulator.
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
    let loops = ["main@7", "main@14", "main@18", "main@28", "main@32"];
    let (out, code, err) = same_both_ways("nested", &ok, &loops);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, "2997000 72 40040\n9223372036854775297\n");
    let over = src.replace("START", "10");
    let (out, code, err) = same_both_ways("nested-over", &over, &loops);
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
    let loops = [
        "main@3", "main@7", "main@11", "main@19", "main@25", "main@29",
    ];
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

/// The runs of statements the auto-par build ran as tasks, from its
/// report: each `instance@line` of the run's first statement.
fn grouped(stderr: &[u8]) -> Vec<String> {
    std::str::from_utf8(stderr)
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("auto-par: statements as tasks: "))
        .map(|l| l.split(", ").map(String::from).collect())
        .unwrap_or_default()
}

/// Runs of independent statements that each loop run as tasks and give
/// the sequential program's output on both backends: `let`s that move
/// their argument in, expression statements that fill a `mut ref`, values
/// that own memory or run a `Drop` body, a generic instance, and an
/// expression statement's value dropped. A statement that prints keeps its
/// place, one that reads what an earlier one binds starts the next run,
/// and one that may panic stays after a `?` or `return`, which in order
/// would have kept it from running.
#[test]
fn mir_auto_par_statement_groups_match_in_order() {
    let src = r#"struct Noisy { id: i64 }
impl Drop for Noisy { fn drop(mut ref self) { println(f"drop {self.id}"); } }

struct Pair { x: i64, y: Vec[i64] }

fn work(n: i64) -> i64 {
    let mut s = 0;
    for i in 0..n {
        s = (s + i * i) % 1000007;
    }
    s
}

fn total(v: own Vec[i64]) -> i64 {
    let mut s = 0;
    for x in v.iter() {
        s = s + *x;
    }
    s
}

fn fill(v: mut ref Vec[i64], n: i64) {
    for i in 0..n {
        v.push(i);
    }
}

fn noisy(n: i64) -> Noisy {
    Noisy { id: work(n) }
}

fn pair(n: i64) -> Pair {
    let mut y = Vec.new();
    for i in 0..n {
        y.push(i);
    }
    Pair { x: work(n), y: y }
}

fn count[T](v: ref Vec[T], f: i64) -> i64 {
    let mut s = 0;
    for i in 0..v.len() {
        s = (s * 2 + f) % 1000;
    }
    s
}

fn checked(n: i64) -> Result[i64, String] {
    if n < 0 {
        return Err(f"negative {n}");
    }
    Ok(work(n))
}

fn both(x: i64, y: i64) -> Result[i64, String] {
    let a = checked(x)?;
    let b = checked(y)?;
    Ok(a + b)
}

fn early(x: i64, y: i64) -> i64 {
    let a = if x < 0 { return -1 } else { work(x) };
    let b = if y < 0 { return -2 } else { work(y) };
    a + b
}

fn main() {
    let v = Vec.filled(1000, 3);
    let w = Vec.filled(2000, 5);
    let a = total(v);
    let b = total(w);
    println(f"{a} {b}");
    let mut p: Vec[i64] = Vec.new();
    let mut q: Vec[i64] = Vec.new();
    fill(mut p, 100);
    fill(mut q, 200);
    println(f"{p.len()} {q.len()}");
    let n1 = noisy(100);
    let n2 = noisy(200);
    println(f"{n1.id} {n2.id}");
    let r = pair(100);
    let s = pair(50);
    println(f"{r.x} {r.y.len()} {s.x} {s.y.len()}");
    let g1 = count(p, 3);
    let g2 = count(q, 4);
    println(f"{g1} {g2}");
    println(f"{both(1000, 2000)} {both(-1, 5)} {both(5, -2)} {both(-3, -4)}");
    println(f"{early(10, 20)} {early(-1, 20)} {early(10, -1)} {early(-1, -1)}");
    let t = work(300);
    let u = work(t % 100);
    pair(30);
    pair(40);
    println(f"{t} {u}");
}
"#;
    let (out, code, err) = same_both_ways("groups", src, &["pair@34"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(
        out,
        "3000 10000\n100 200\n328350 646686\n328350 100 40425 50\n\
         125 500\nOk(1479528) Err(negative -1) Err(negative -2) Err(negative -3)\n\
         2755 -1 -2 -1\n954994 272459\ndrop 646686\ndrop 328350\n"
    );
    // `both` and `early` stay in order: their first statement can return,
    // and in order that return keeps the second, whose `work` can
    // overflow, from running at all (B-2026-10-24-35).
    let par = interp_run("groups-report", src, true);
    assert_eq!(
        grouped(&par.stderr),
        ["main@70", "main@75", "main@78", "main@81", "main@84", "main@90"],
        "{}",
        String::from_utf8_lossy(&par.stderr)
    );
}

/// Four statements that each sleep four times run concurrently as tasks.
#[test]
fn mir_auto_par_statements_wait_concurrently() {
    let src = r#"fn slow(n: i64) -> i64 {
    let mut s = 0;
    for i in 0..4 {
        sleep_ms(50);
        s = s * 2 + i * n;
    }
    s
}

fn main() {
    let a = slow(1);
    let b = slow(2);
    let c = slow(3);
    let d = slow(4);
    println(a + b + c + d);
}
"#;
    let timed = |auto: bool| {
        let (dir, file) = source(&format!("s-{auto}"), src);
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
        let want: &[&str] = if auto { &["main@11"] } else { &[] };
        assert_eq!(grouped(err.as_bytes()), want, "{err}");
        assert_loops(&tasked(err.as_bytes()), &[], "statements");
        let start = std::time::Instant::now();
        let out = Command::new(&exe).output().unwrap();
        let took = start.elapsed();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(String::from_utf8(out.stdout).unwrap(), "110\n");
        Some(took)
    };
    let (Some(seq), Some(par)) = (timed(false), timed(true)) else {
        return;
    };
    assert!(seq >= std::time::Duration::from_millis(800), "{seq:?}");
    assert!(par < std::time::Duration::from_millis(500), "{par:?}");
}

/// A counting `while` (`while k < n { ..; k += 1 }`, or `k = k + 1`) runs
/// as a `for` over `k..n` does: a reduction over `i64`, `u64` from a `u32`
/// counter, an `xor` bounded by `v.len()`, and the counter left at `n`, or
/// where it was when the loop never ran, and a push. A body that breaks,
/// or writes its bound, keeps its order.
#[test]
fn mir_auto_par_counting_whiles() {
    let src = r#"fn work(n: i64) -> i64 {
    let mut s = 0;
    let mut i = 0;
    while i < n {
        s = (s + i * i) % 1000007;
        i += 1;
    }
    s
}

fn main() {
    let mut total = 0;
    let mut k = 0;
    let iters = 1000;
    while k < iters {
        let w = work(k);
        total = total + w;
        k = k + 1;
    }
    println(f"{total} {k}");
    let mut j: u32 = 5;
    let mut x: u64 = 0;
    while j < 40 {
        x += j as u64;
        j += 1;
    }
    let mut late: u32 = 50;
    while late < 40 {
        x += late as u64;
        late += 1;
    }
    println(f"{x} {j} {late}");
    let v = Vec.filled(37, 2);
    let mut m = 3;
    let mut bits = 0;
    while m < v.len() {
        bits = bits ^ (v[m] * m);
        m += 1;
    }
    println(f"{bits} {m}");
    let mut q = 0;
    let mut out: Vec[i64] = Vec.new();
    while q < 10 {
        out.push(q);
        q += 1;
    }
    let mut b = 0;
    let mut found = 0;
    while b < 100 {
        if b * b > 50 {
            found = b;
            break;
        }
        b += 1;
    }
    let mut lim = 10;
    let mut c = 0;
    let mut seen = 0;
    while c < lim {
        seen += c;
        lim = 8;
        c += 1;
    }
    println(f"{out.len()} {q} {found} {b} {seen} {c}");
}
"#;
    let (out, code, err) = same_both_ways(
        "whiles",
        src,
        &["main@15", "main@23", "main@28", "main@36", "main@43"],
    );
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, "458838713 1000\n770 40 50\n78 37\n10 10 8 8 28 8\n");
}

/// A loop in a function that runs many times (recursive, or called from a
/// loop) keeps its order, as its tasks would be spawned on every call; the
/// same loop in a function called once runs as tasks.
#[test]
fn mir_auto_par_loops_in_hot_functions_stay_in_order() {
    let src = r#"fn rec(d: i64) -> i64 {
    let mut s = 0;
    let mut i = 0;
    while i < 50 {
        s += i * d;
        i += 1;
    }
    if d == 0 { s } else { s + rec(d - 1) }
}

fn called(d: i64) -> i64 {
    let mut s = 0;
    let mut i = 0;
    while i < 50 {
        s += i * d;
        i += 1;
    }
    s
}

fn cold(d: i64) -> i64 {
    let mut s = 0;
    let mut i = 0;
    while i < 50 {
        s += i * d;
        i += 1;
    }
    s
}

fn main() {
    let mut t = 0;
    for d in 0..20 {
        t += called(d);
        println(t);
    }
    println(f"{rec(10)} {t} {cold(7)}");
}
"#;
    let (out, code, err) = same_both_ways("hot", src, &["cold@24"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.ends_with("67375 232750 8575\n"), "{out}");
    let par = interp_run("hot-report", src, true);
    let report = String::from_utf8_lossy(&par.stderr);
    for at in ["rec@4", "called@14"] {
        let name = at.split('@').next().unwrap();
        let want = format!(
            "auto-par: in order: {at}: Cost(\"{name} is recursive or reached from a loop\")"
        );
        assert!(report.lines().any(|l| l == want), "{want} in {report}");
    }
}

/// The reductions the auto-par build folded, from its report: each
/// `instance@line (acc at line, ..)`.
fn reduced(stderr: &[u8]) -> String {
    std::str::from_utf8(stderr)
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("auto-par: reductions: "))
        .unwrap_or_default()
        .to_string()
}

/// A loop whose only use of a `Vec` is `v.push(e)` (under an `if` too, of
/// owned `String`s too) runs as tasks that push onto `Vec`s of their own,
/// appended in iteration order; an accumulator written at several sites
/// (`u = u + 1` in one arm, `u += 1` in the other) folds, and where it
/// overflows the run is run again in order, so the panic names the site
/// the sequential loop's does, either arm's. A loop that also reads the
/// `Vec` (`seen.len()`), or mixes `*` and `+` on one accumulator, keeps
/// its order.
#[test]
fn mir_auto_par_pushes_and_sums_at_several_sites() {
    let src = r#"fn label(i: i64) -> String {
    let mut s = "";
    for k in 0..(i % 4) {
        s = s + "x";
    }
    f"{i}{s}"
}

fn main() {
    let mut keep: Vec[i64] = Vec.new();
    for t in 0..200 {
        if t % 7 == 3 {
            keep.push(t * t);
        }
    }
    println(f"{keep.len()} {keep[0]} {keep[1]} {keep[keep.len() - 1]}");
    let mut names: Vec[String] = Vec.new();
    for t in 0..12 {
        names.push(label(t));
    }
    let mut all = "";
    for n in names.iter() {
        all = all + n + ",";
    }
    println(all);
    let mut seen: Vec[i64] = Vec.new();
    for t in 0..10 {
        seen.push(t + seen.len());
    }
    let mut mixed = 1;
    for i in 0..10 {
        if i % 2 == 0 {
            mixed = mixed * 2;
        } else {
            mixed = mixed + 1;
        }
    }
    println(f"{seen[9]} {mixed}");
    let mut sink = 0;
    for it in 0..100 {
        if it % 3 == 0 {
            sink = sink + (it + 1);
        } else {
            sink = sink + 1;
        }
    }
    println(sink);
    let mut u: u8 = START;
    for i in 0..60 {
        if i % 2 == 0 {
            u = u + 1;
        } else {
            u += 1;
        }
    }
    println(u);
}
"#;
    let loops = ["main@11", "main@18", "main@40", "main@49"];
    let reductions = "main@11 (keep at 13), main@18 (names at 19), \
                      main@40 (sink at 42, 44), main@49 (u at 51, 53)";
    let head = "29 9 100 39601\n0,1x,2xx,3xxx,4,5x,6xx,7xxx,8,9x,10xx,11xxx,\n18 63\n1783\n";
    for (start, out_want, line) in [
        ("100", "160\n", None),
        ("200", "", Some("53:13")),
        ("199", "", Some("51:17")),
    ] {
        let src = src.replace("START", start);
        let (out, code, err) = same_both_ways("pushes", &src, &loops);
        assert_eq!(out, format!("{head}{out_want}"), "{start}: {err}");
        match line {
            None => assert_eq!(code, Some(0), "{err}"),
            Some(at) => {
                assert_eq!(code, Some(101), "{err}");
                assert!(err.contains(&format!("main.kara:{at}")), "{start}: {err}");
            }
        }
        let par = interp_run("pushes-report", &src, true);
        assert_eq!(reduced(&par.stderr), reductions);
    }
}

/// Shapes from the corpus run with auto-par on, each of which once changed
/// a program's output or made the parallel build fall back: a sum of
/// `ref` terms, a range of `i8`s with more iterations than an `i8` holds,
/// a collecting loop in a program that reads `capacity()` (kept in
/// order), statements reading a table of literals built by the `let`
/// before them, statements that move a local into a builtin call, values
/// that borrow what their task would own (kept in order), a statement
/// that reads on its way to a `return` what a sibling writes (kept in
/// order), and `|| T.zero()` in a generic function's closure. No build
/// falls back.
#[test]
fn mir_auto_par_corpus_shapes_keep_their_output() {
    let src = r#"#[derive(Hash, Eq, PartialEq)]
enum Tag { Named { s: String }, Anon }

trait Zero { fn zero() -> Self; }
impl Zero for i64 { fn zero() -> i64 { 0 } }

struct P { name: String, age: i64 }

fn total(v: ref Vec[i64]) -> i64 {
    let mut t = 0;
    for x in v.iter() { t = t + *x; }
    t
}

fn ones(s: own String) -> i64 {
    let mut n = 0;
    for b in s.bytes() { if b == b'1' { n = n + 1; } }
    n
}

fn filled[T: Zero + Copy](n: i64) -> Vec[T] {
    let zero = || T.zero();
    let mut v: Vec[T] = Vec.new();
    for _ in 0..n { v.push(zero()); }
    v
}

fn fill(v: mut ref Vec[i64], n: i64) {
    for i in 0..n { v.push(i); }
}

fn early(c: bool) -> i64 {
    let mut v: Vec[i64] = Vec.new();
    fill(mut v, 100);
    if c {
        let mut t = 0;
        for x in v.iter() { t = t + *x; }
        return t;
    }
    total(v)
}

fn main() {
    let mut m: Map[Tag, i64] = Map.new();
    m.insert(Tag.Named { s: "k-3" }, 7);
    let mut hits = 0;
    let mut j = 0;
    while j < 6 {
        match m.get(Tag.Named { s: f"k-{j}" }) {
            Some(v) => { hits = hits + v; }
            None => {}
        }
        j += 1;
    }
    let lo: i8 = -100;
    let hi: i8 = 100;
    let mut n: i32 = 0;
    for q in lo..hi { n = n + (q as i32); }
    println(f"{hits} {n}");
    let mut c: Vec[i64] = Vec.new();
    for i in 0..26 { c.push(i); }
    println(f"{c.len()} {c.capacity()}");
    let w: Vec[i64] = [3, 1, 4, 1, 5, 9, 2, 6];
    let a = total(w);
    let b = total(w);
    println(a + b);
    let mut back: Vec[u8] = Vec.new();
    let mut alt: Vec[u8] = Vec.new();
    for i in 0..40 {
        back.push(if i < 20 { b'1' } else { b'0' });
        alt.push(if i % 2 == 0 { b'0' } else { b'1' });
    }
    let x = ones(String.from_utf8(back).unwrap());
    let y = ones(String.from_utf8(alt).unwrap());
    println(f"{x} {y}");
    let ps: Vec[P] = vec![P { name: "ann".to_string(), age: 30 }, P { name: "bob".to_string(), age: 25 }];
    let young = ps.iter().min_by_key(|p| p.age);
    let old = ps.iter().max_by_key(|p| p.age);
    match (young, old) {
        (Some(y), Some(o)) => println(f"{y.name} {o.name}"),
        _ => println("none"),
    }
    let z: Vec[i64] = filled(3);
    println(f"{z.len()} {early(true)} {early(false)}");
}
"#;
    let (out, code, err) = same_both_ways("corpus-shapes", src, &["main@48", "main@58"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, "7 -100\n26 26\n62\n20 20\nbob ann\n3 4950 4950\n");
    let par = interp_run("corpus-shapes-report", src, true);
    let report = String::from_utf8_lossy(&par.stderr);
    assert_eq!(grouped(&par.stderr), ["main@64", "main@73"], "{report}");
    for fallback in ["was refused", "keeps its order"] {
        assert!(!report.contains(fallback), "{report}");
    }
}
