//! Adaptors on a value of generic iterator type (B-2026-10-24-4): under
//! `I: Iterator[Item = i64]`, `it.map(f)` is the instance's `map`, whose
//! output type `OutV` only its return type names, so the builder reads it
//! off the call's type. iterators.md: every iterator gets the adaptor and
//! terminal methods.

use karac::mir::interp::Outcome;

fn run(src: &str) -> Result<(String, i32), String> {
    let r = karac::mir::lower::run_source(src)?;
    match (&r.outcome, r.exit_code()) {
        (_, Some(code)) => Ok((r.output, code)),
        (Outcome::Error(e), None) => Err(format!("run: {e}\n--- output so far\n{}", r.output)),
        _ => Err("no exit code".into()),
    }
}

fn assert_runs(src: &str, want: &str) {
    match run(src) {
        Ok((out, 0)) if out == want => {}
        other => panic!("want exit 0 and\n{want}--- got {other:?}"),
    }
}

/// `map` then a terminal, the row's own program.
#[test]
fn a_generic_iterator_maps_then_counts() {
    let src = r#"
fn tot[I: Iterator[Item = i64]](it: own I) -> i64 {
    it.map(|x| x + 1).count()
}
fn big[I: Iterator[Item = i64]](it: own I) -> i64 {
    match it.map(|x| x * 2).max() {
        Some(m) => m,
        None => 0,
    }
}
fn main() {
    let w: Vec[i64] = [5, 6];
    println(f"{tot(w.into_iter())}");
    let u: Vec[i64] = [1, 3, 2];
    println(f"{big(u.into_iter())}");
}
"#;
    assert_runs(src, "2\n6\n");
}

/// A map to another item type collected, a chain of adaptors iterated by
/// `for`, a `flat_map`, and `sum`, each on a generic receiver.
#[test]
fn a_generic_iterator_chains_adaptors() {
    let src = r#"
fn total[I: Iterator[Item = i64]](it: own I) -> i64 {
    it.sum()
}
fn names[I: Iterator[Item = i64]](it: own I) -> Vec[String] {
    it.map(|x| f"n{x}").collect()
}
fn pairs[I: Iterator[Item = i64]](it: own I) -> i64 {
    let mut s = 0;
    for (i, x) in it.enumerate().map(|(i, x)| (i as i64, x * 10)) {
        s += i * x;
    }
    s
}
fn flat[I: Iterator[Item = i64]](it: own I) -> i64 {
    it.flat_map(|x| 0..x).count()
}
fn main() {
    let w: Vec[i64] = [5, 6];
    println(total(w.into_iter()));
    let n = names(0..3);
    println(f"{n[0]} {n[2]}");
    let p: Vec[i64] = [1, 1, 1];
    println(pairs(p.into_iter()));
    let q: Vec[i64] = [1, 2, 3];
    println(flat(q.into_iter()));
}
"#;
    assert_runs(src, "11\nn0 n2\n30\n6\n");
}
