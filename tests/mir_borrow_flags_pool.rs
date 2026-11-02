//! Under the task pool each task keeps its own borrow flags, carried with
//! it across suspensions and swapped in while it runs, so tasks that write
//! `mut` fields of their own `shared` values take no lock and never meet
//! each other's flags (B-2026-11-01-25). The pins: eight tasks on four
//! workers, each holding loans across `sleep_ms` suspensions, finish; and
//! one task's alias conflict, held across suspensions, still panics.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const APART: &str = r#"shared struct Bag { mut items: Vec[i64] }
fn add_all(src: Bag, dst: Bag) {
    for x in src.items {
        sleep_ms(1);
        dst.items.push(x);
    }
}
fn work(n: i64) -> i64 {
    let a = Bag { items: Vec.new() };
    let b = Bag { items: Vec.new() };
    for i in 0..n { a.items.push(i); }
    for _ in 0..3 { add_all(a, b); }
    b.items.len()
}
fn main() {
    let group = TaskGroup.new();
    let mut hs: Vec[TaskHandle[i64]] = Vec.new();
    for _ in 0..8 { hs.push(group.spawn(|| work(5))); }
    let mut total = 0;
    for h in hs.into_iter() { total = total + h.join(); }
    println(f"{total}");
}
"#;

const ALIAS: &str = r#"shared struct Bag { mut items: Vec[i64] }
fn add_all(src: Bag, dst: Bag) {
    for x in src.items {
        sleep_ms(1);
        dst.items.push(x);
    }
}
fn work(n: i64) -> i64 {
    let a = Bag { items: Vec.new() };
    let b = Bag { items: Vec.new() };
    for i in 0..n { a.items.push(i); }
    for _ in 0..3 { add_all(a, b); }
    if n == 4 { add_all(b, b); }
    b.items.len()
}
fn main() {
    let group = TaskGroup.new();
    let mut hs: Vec[TaskHandle[i64]] = Vec.new();
    for t in 0..8 { let n = if t == 5 { 4 } else { 5 }; hs.push(group.spawn(|| work(n))); }
    let mut total = 0;
    for h in hs.into_iter() { total = total + h.join(); }
    println(f"{total}");
}
"#;

/// Each run's stdout and exit code at four workers, or `None` when no
/// runtime archive is there to link.
fn runs(src: &str, tag: &str) -> Option<Vec<(String, Option<i32>)>> {
    let d = std::env::temp_dir().join(format!("karac-flags-pool-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("main.kara"), src).unwrap();
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&d)
        .args(["build", "main.kara", "-o", "main"])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    })?;
    let out = (0..4)
        .map(|_| {
            let o = Command::new(d.join("main"))
                .env("KARAC_THREADS", "4")
                .output()
                .unwrap();
            (
                String::from_utf8_lossy(&o.stdout).into_owned(),
                o.status.code(),
            )
        })
        .collect();
    let _ = std::fs::remove_dir_all(&d);
    Some(out)
}

#[test]
fn pooled_tasks_keep_their_own_borrow_flags() {
    let interp = karac::mir::lower::run_source(APART).expect("MIR interpreter");
    assert_eq!(
        interp.output,
        "120
"
    );
    for (i, (out, code)) in runs(APART, "apart").into_iter().flatten().enumerate() {
        assert_eq!(
            (out.as_str(), code),
            (
                "120
",
                Some(0)
            ),
            "run {i}"
        );
    }
}

#[test]
fn a_pooled_task_still_panics_on_its_own_alias() {
    for (i, (out, code)) in runs(ALIAS, "alias").into_iter().flatten().enumerate() {
        assert_eq!((out.as_str(), code), ("", Some(101)), "run {i}");
    }
}
