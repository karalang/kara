//! B-2026-11-17-115: a constructor seed's numbers stay open for the closure to
//! fix. `it.fold(SortedMap.from([(-1, 0)]), |dp, a| ..)` over `i32` items
//! was refused (expected 'SortedMap[i64, i64]', found 'SortedMap[i32,
//! i32]') with no fix, where `fold(0, ..)` and `fold([0], ..)` took the
//! closure's type. A list of tuple literals is open as a list of literals
//! is, and a number whose type no use has fixed yet binds by value out of a
//! borrowed tuple, as a fixed one does.

use std::process::Command;

fn run(tag: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-ctor-seed-literals-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn a_sorted_map_seed_takes_the_closures_number_type() {
    let (ok, out) = run(
        "sorted",
        "fn main() {\n    let v: Vec[i32] = [1, 2, 3];\n    \
         let dp = v.iter().fold(SortedMap.from([(-1, 0)]), |dp, a| {\n        \
         let mut next = SortedMap.new();\n        \
         for (prev, ops) in ref dp {\n            \
         if a > prev {\n                next.insert(a, ops + 1);\n            }\n        }\n        \
         next\n    });\n    \
         let total: i32 = dp.values().cloned().sum();\n    println(f\"{dp.len()} {total}\");\n}\n",
    );
    assert!(ok, "{out}");
    assert_eq!(out, "1 3\n");
}

#[test]
fn a_map_seed_takes_the_closures_number_type() {
    let (ok, out) = run(
        "map",
        "fn main() {\n    let v: Vec[i32] = [1, 2];\n    \
         let m = v.iter().fold(Map.from([(-1, 0)]), |m, x| { let mut n = Map.new(); n.insert(x, x); n });\n    \
         let s = v.iter().fold(SortedSet.from([0]), |s, x| { let mut t = SortedSet.new(); t.insert(x); t });\n    \
         println(f\"{m.len()} {s.len()}\");\n}\n",
    );
    assert!(ok, "{out}");
    assert_eq!(out, "1 1\n");
}

/// A seed whose closure annotates its parameters keeps working: the
/// annotation, not the literal, says what the numbers are.
#[test]
fn an_annotated_closure_over_a_filled_seed_still_checks() {
    let (ok, out) = run(
        "annotated",
        "fn main() {\n    let rows: Vec[Vec[i64]] = [[1, 2], [3, 4]];\n    \
         let t = rows.iter().fold(Vec.filled(2, 0), |total: Vec[i64], below: Vec[i64]| \
         total.iter().zip(below.iter()).map(|(a, b)| a + b).collect());\n    \
         let u = rows.iter().fold([0, 0], |total: Vec[i64], below: Vec[i64]| \
         total.iter().zip(below.iter()).map(|(a, b)| a + b).collect());\n    \
         println(f\"{t[1]} {u[0]}\");\n}\n",
    );
    assert!(ok, "{out}");
    assert_eq!(out, "6 4\n");
}

#[test]
fn a_list_of_tuple_literals_takes_a_later_uses_type() {
    let (ok, out) = run(
        "tuples",
        "fn main() {\n    let m = [(1, 0), (-2, 5)];\n    let x: Vec[(i32, i32)] = m;\n    \
         let rook = [(1, 0), (0, -1)];\n    let mut dirs: Vec[(i64, i64)] = Vec.new();\n    \
         dirs.extend(rook.iter());\n    \
         println(f\"{x.len()} {dirs.len()}\");\n}\n",
    );
    assert!(ok, "{out}");
    assert_eq!(out, "2 2\n");
}

#[test]
fn a_list_of_tuple_literals_no_use_fixes_is_i64() {
    let (ok, out) = run(
        "default",
        "fn main() {\n    let m = [(1, 0), (2, 5)];\n    \
         let big = m[0].0 * 4000000000;\n    for (a, b) in ref m {\n        println(f\"{a + b}\");\n    }\n    \
         println(f\"{big}\");\n}\n",
    );
    assert!(ok, "{out}");
    assert_eq!(out, "1\n7\n4000000000\n");
}

#[test]
fn a_suffixed_literal_still_fixes_the_seed() {
    let (ok, out) = run(
        "suffixed",
        "fn main() {\n    let v: Vec[i32] = [1, 2];\n    \
         let s = v.iter().fold(SortedMap.from([(-1i64, 0i64)]), |m, x| { let mut n = SortedMap.new(); n.insert(x, x); n });\n    \
         println(f\"{s.len()}\");\n}\n",
    );
    assert!(!ok, "{out}");
    assert!(out.contains("expected 'SortedMap[i64, i64]'"), "{out}");
}
