//! A counting loop that starts at a local, `for i in low..low + k`, runs
//! `k` trips, and `mir::presize` reserves `k` for its pushes (kata 658's
//! window copy grew its vector 4, 8, 16, ... a query, B-2026-11-09-6).
//! Only a start the bound adds to counts, and only while neither changes
//! between the add and the loop: `moved` steps its start after working the
//! bound out, so its vector still grows by doubling. Compiled, the
//! capacities show which fills were reserved for; the interpreter keeps no
//! capacity but a reservation's, so there an unreserved vector's is its
//! length.

#[cfg(feature = "llvm")]
mod common;

const SRC: &str = r#"
fn window(arr: Vec[i64], low: i64, k: i64) -> Vec[i64] {
    let mut out: Vec[i64] = Vec.new();
    for i in low..low + k {
        out.push(arr[i]);
    }
    out
}

fn closest(arr: Vec[i64], k: i64, x: i64) -> Vec[i64] {
    let mut low = 0;
    let mut high = arr.len() - k;
    while low < high {
        let middle = (low + high) / 2;
        if x - arr[middle] > arr[middle + k] - x {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    let mut out: Vec[i64] = Vec.new();
    for i in low..low + k {
        out.push(arr[i]);
    }
    out
}

fn through(arr: Vec[i64], low: i64, k: i64) -> Vec[i64] {
    let mut out: Vec[i64] = Vec.new();
    for i in low..=low + k {
        out.push(arr[i]);
    }
    out
}

fn moved(arr: Vec[i64], low: i64, k: i64) -> Vec[i64] {
    let mut lo = low;
    let end = lo + k;
    lo += 1;
    let mut out: Vec[i64] = Vec.new();
    for i in lo..end {
        out.push(arr[i]);
    }
    out
}

fn show(v: Vec[i64]) -> String {
    let mut sum = 0;
    for x in v {
        sum += x;
    }
    f"{sum} {v.len()} {v.capacity()}"
}

fn main() {
    let arr: Vec[i64] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let a = show(window(arr, 2, 5));
    let b = show(closest(arr, 3, 5));
    let c = show(through(arr, 1, 4));
    let d = show(moved(arr, 0, 6));
    let e = show(window(arr, 3, -2));
    println(f"{a}|{b}|{c}|{d}|{e}");
}
"#;

#[test]
fn mir_presize_counts_trips_from_a_local_start() {
    let r = karac::mir::lower::run_source(SRC).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        r.output, "25 5 5|15 3 3|20 5 5|20 5 5|0 0 0\n",
        "stderr: {}",
        r.stderr
    );
}

#[cfg(feature = "llvm")]
#[test]
fn mir_llvm_presize_counts_trips_from_a_local_start() {
    use std::process::Command;
    let lowered = karac::mir::lower::build_source(SRC).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-presize-offset-{}", std::process::id()));
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
            "25 5 5|15 3 3|20 5 5|20 5 8|0 0 0\n"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
