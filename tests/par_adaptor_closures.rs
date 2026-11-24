//! B-2026-11-23-19: a lazy iterator adaptor (`map`, `filter`,
//! `filter_map`) keeps the closure it is given in a field and calls it from
//! its `next`. A `par` branch running such a chain has the closure's own
//! effects (core semantics §12), traced from the field back to the closure
//! the branch's code passed, where it used to have every effect, so two
//! branches each summing their own `v.iter().map(|x| x * 2)` were refused
//! as conflicting. A closure that prints, or writes a place another branch
//! reads, is still refused.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, sub: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-par-adaptor-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(sub)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn refused(tag: &str, src: &str, want: &str) {
    let out = karac(tag, "check", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{tag}: accepted");
    assert!(stderr.contains(want), "{tag}: {stderr}");
}

#[test]
fn par_branches_running_pure_adaptor_chains_on_their_own_locals_are_accepted() {
    let src = "fn f(k: i64) -> i64 { let v = [k, 2, 3]; v.iter().map(|x| x * 2).sum() }
fn g(k: i64) -> i64 { let v = [k, 2, 3]; v.iter().filter(|x| *x > 1).count() }
fn h(k: i64) -> i64 {
    let v = [k, 2, 3];
    v.iter().filter_map(|x| if x > 1 { Some(x * 10) } else { None }).map(|x| x + 1).sum()
}
fn run(f: Fn(i64) -> i64) -> i64 { let v = [1, 2, 3]; v.iter().map(|x| f(x)).sum() }
fn main() {
    let (a, b) = par { f(1), f(2) };
    let (c, d) = par { g(1), g(2) };
    let (e, i) = par { h(1), h(2) };
    let k = 3;
    let (m, n) = par { run(|x| x + k), run(|x| x * k) };
    let w = par for x in [1, 2].iter() { f(x) };
    println(f\"{a} {b} {c} {d} {e} {i} {m} {n} {w}\");
}
";
    for sub in ["check", "__mir-run"] {
        let out = karac("pure", sub, src);
        assert!(
            out.status.success(),
            "{sub}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        if sub == "__mir-run" {
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "12 14 2 3 52 73 15 18 [12, 14]\n"
            );
        }
    }
}

#[test]
fn par_branches_whose_adaptor_closures_print_are_refused() {
    refused(
        "map-print",
        "fn h(k: i64) -> i64 { let v = [k, 2, 3]; v.iter().map(|x| { println(x); x * 2 }).sum() }
fn main() { let (a, b) = par { h(1), h(2) }; println(a + b); }
",
        "`writes(Stdout)` and `writes(Stdout)`",
    );
    refused(
        "filter-print",
        "fn g(k: i64) -> i64 { let v = [k, 2, 3]; v.iter().filter(|x| { println(x); *x > 1 }).count() }
fn main() { let (a, b) = par { g(1), g(2) }; println(a + b); }
",
        "`writes(Stdout)` and `writes(Stdout)`",
    );
    refused(
        "param-print",
        "fn run(f: Fn(i64) -> i64) -> i64 { let v = [1, 2, 3]; v.iter().map(|x| f(x)).sum() }
fn main() { let (a, b) = par { run(|x| { println(x); x }), run(|x| { println(x); x }) }; println(a + b); }
",
        "`writes(Stdout)` and `writes(Stdout)`",
    );
}

#[test]
fn a_par_branch_whose_adaptor_closure_writes_a_place_another_reads_is_refused() {
    refused(
        "map-writes",
        "fn main() {
    let v = [1, 2, 3];
    let mut n = 0;
    let (a, b) = par { v.iter().map(|x| { n += 1; x * 2 }).sum(), n };
    println(a + b);
}
",
        "`mutable borrow of n` and `read of n`",
    );
}
