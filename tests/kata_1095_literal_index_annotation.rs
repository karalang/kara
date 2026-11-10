//! A binary search over an `i32` interface (LeetCode's `MountainArray`,
//! kata 1095) written as Rust writes it: `let mut lo = 0; let mut hi = n -
//! 1;` with `n: i32`. The literal keeps the default `i64` beside the `i32`
//! (design.md §8 Literals), so `mid = (lo + hi) / 2` is an `i64` and its
//! use as an `i32` index is refused. The refusal now names `lo` and `karac
//! fix` declares it, `lo: i32`, as it did for a copy of `lo`; for a tuple
//! `let mut (lo, hi) = (0, n - 1)` and a `for (lo, hi, s) in [(0, ..), ..]`
//! the fix is the literal's suffix, `0i32` (B-2026-11-10-30).

use std::process::{Command, Stdio};

/// `karac fix` rounds until nothing changes.
fn fixed_fully(src: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "karac-kata1095-{}-{}.kara",
        std::process::id(),
        src.len()
    ));
    std::fs::write(&path, src).unwrap();
    for _ in 0..6 {
        let before = std::fs::read_to_string(&path).unwrap();
        let _ = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["fix", path.to_str().unwrap()])
            .stdin(Stdio::null())
            .output()
            .expect("karac");
        if std::fs::read_to_string(&path).unwrap() == before {
            break;
        }
    }
    let out = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    out
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .unwrap_or_else(|e| panic!("build: {e}\n{src}"))
        .output
}

const AT: &str = "fn at(v: Vec[i32], i: i32) -> i32 {\n    v[i as i64]\n}\n\n";

#[test]
fn a_value_computed_from_a_literal_binding_names_it() {
    let src = format!(
        "{AT}fn first_at_least(v: Vec[i32], target: i32) -> i32 {{\n\
         \x20   let n = v.len() as i32;\n\
         \x20   let mut lo = 0;\n\
         \x20   let mut hi = n;\n\
         \x20   while lo < hi {{\n\
         \x20       let mid = (lo + hi) / 2;\n\
         \x20       if at(v, mid) < target {{\n\
         \x20           lo = mid + 1;\n\
         \x20       }} else {{\n\
         \x20           hi = mid;\n\
         \x20       }}\n\
         \x20   }}\n\
         \x20   lo\n}}\n\n\
         fn main() {{\n\
         \x20   let v: Vec[i32] = [1, 3, 5, 7];\n\
         \x20   println(f\"{{first_at_least(v, 5)}} {{first_at_least(v, 8)}}\");\n}}\n"
    );
    let Err(e) = karac::mir::lower::run_source(&src) else {
        panic!("refused before the fix");
    };
    let e = e.to_string();
    assert!(e.contains("lo"), "the refusal names the binding: {e}");
    let out = fixed_fully(&src);
    assert!(out.contains("let mut lo: i32 = 0;"), "{out}");
    assert_eq!(run(&out), "2 4\n");
}

#[test]
fn a_tuple_literal_and_a_for_over_tuples_take_the_suffix() {
    let src = format!(
        "{AT}fn find(v: Vec[i32], target: i32) -> i32 {{\n\
         \x20   let n = v.len() as i32;\n\
         \x20   let mut (l, r) = (0, n - 1);\n\
         \x20   while l <= r {{\n\
         \x20       let m = l + (r - l) / 2;\n\
         \x20       let x = at(v, m);\n\
         \x20       if x == target {{\n\
         \x20           return m;\n\
         \x20       }} else if x < target {{\n\
         \x20           l = m + 1;\n\
         \x20       }} else {{\n\
         \x20           r = m - 1;\n\
         \x20       }}\n\
         \x20   }}\n\
         \x20   -1\n}}\n\n\
         fn halves(v: Vec[i32]) -> i32 {{\n\
         \x20   let n = v.len() as i32;\n\
         \x20   let mut total = 0;\n\
         \x20   for (lo, hi) in [(0, n / 2), (n / 2, n)] {{\n\
         \x20       total += at(v, lo) * at(v, hi - 1);\n\
         \x20   }}\n\
         \x20   total\n}}\n\n\
         fn main() {{\n\
         \x20   let v: Vec[i32] = [1, 3, 5, 7];\n\
         \x20   println(f\"{{find(v, 5)}} {{find(v, 4)}} {{halves(v)}}\");\n}}\n"
    );
    assert!(
        karac::mir::lower::run_source(&src).is_err(),
        "refused before the fix"
    );
    let out = fixed_fully(&src);
    assert!(out.contains("let mut (l, r) = (0i32, n - 1);"), "{out}");
    assert!(
        out.contains("for (lo, hi) in [(0i32, n / 2), (n / 2, n)]"),
        "{out}"
    );
    assert_eq!(run(&out), "2 -1 38\n");
}

#[test]
fn a_wide_operand_beside_the_literal_gets_no_annotation() {
    // `k` is an `i64` of its own, so `lo: i32` would not make `m` an `i32`:
    // the refusal keeps its `as` advice and the fix leaves `lo` alone.
    let src = format!(
        "{AT}fn main() {{\n\
         \x20   let v: Vec[i32] = [1, 3, 5, 7];\n\
         \x20   let k: i64 = 1;\n\
         \x20   let mut lo = 0;\n\
         \x20   lo += 1;\n\
         \x20   let m = lo + k;\n\
         \x20   println(f\"{{at(v, m)}}\");\n}}\n"
    );
    let out = fixed_fully(&src);
    assert!(!out.contains("lo: i32"), "{out}");
}
