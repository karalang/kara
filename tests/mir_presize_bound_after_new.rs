//! A vector made before its counting loop's bound is worked out (kata
//! 765's `seat_of`, made ahead of `for _ in 0..row.len()`) is reserved
//! for the trips at the loop's way in, past any seed pushes, since the
//! bound is not known yet where `Vec.new()` is (B-2026-11-09-10). Compiled,
//! the capacities show the reservation: 10 for ten pushes rather than the
//! 16 doubling reaches, and 11 for a seed and ten more. The interpreter
//! keeps no capacity but a reservation's.

#[cfg(feature = "llvm")]
mod common;

const SRC: &str = r#"
fn zeros(row: Vec[i64]) -> Vec[i64] {
    let mut seat_of: Vec[i64] = Vec.new();
    for _ in 0..row.len() {
        seat_of.push(0);
    }
    seat_of
}

fn seeded(row: Vec[i64]) -> Vec[i64] {
    let mut out: Vec[i64] = Vec.new();
    out.push(9);
    let n = row.len();
    for i in 0..n {
        out.push(row[i]);
    }
    out
}

fn moved(row: own Vec[i64]) -> i64 {
    let mut row = row;
    let mut seat_of: Vec[i64] = Vec.new();
    for _ in 0..row.len() {
        seat_of.push(0);
    }
    for i in 0..row.len() {
        seat_of[row[i]] = i;
    }
    row[0] = seat_of[9];
    row[0] * 100 + seat_of.capacity()
}

fn show(v: Vec[i64]) -> String {
    let mut sum = 0;
    for x in v {
        sum += x;
    }
    f"{sum} {v.len()} {v.capacity()}"
}

fn main() {
    let row: Vec[i64] = [9, 8, 7, 6, 5, 4, 3, 2, 1, 0];
    let none: Vec[i64] = Vec.new();
    println(f"{show(zeros(row))}|{show(seeded(row))}|{moved(row)}|{show(zeros(none))}");
}
"#;

#[test]
fn mir_presize_reserves_past_a_late_bound() {
    let r = karac::mir::lower::run_source(SRC).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        r.output, "0 10 10|54 11 11|10|0 0 0\n",
        "stderr: {}",
        r.stderr
    );
}

#[cfg(feature = "llvm")]
#[test]
fn mir_llvm_presize_reserves_past_a_late_bound() {
    use std::process::Command;
    let lowered = karac::mir::lower::build_source(SRC).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-presize-late-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    if linked.is_some() {
        let out = Command::new(&exe).output().expect("run");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "0 10 10|54 11 11|10|0 0 0\n"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
