//! B-2026-11-08-82: `Map`, `SortedMap`, `Set` and `SortedSet` implement
//! `PartialEq` and `Eq` by content, order playing no part
//! (docs/library/collections.md § Common traits). The run side is pinned by
//! corpus/core/ok_maps_and_sets_compare_by_content; these pin the checker:
//! where the impls hold, and that a refusal on a library type offers no
//! `#[derive(..)]` the program cannot write.

use std::process::{Command, Stdio};

/// `karac check`'s error lines for `src`.
fn check(name: &str, src: &str) -> Vec<String> {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|l| l.starts_with("error"))
        .map(str::to_string)
        .collect()
}

#[test]
fn map_and_set_equality_checks() {
    let src = "#[derive(PartialEq, Eq)]\n\
               struct Inv { stock: Map[String, i64], tags: SortedSet[String] }\n\
               #[derive(PartialEq)]\n\
               struct Prices { by_name: SortedMap[String, f64], seen: Set[i64] }\n\
               fn main() {\n\
               \x20   let a: Map[String, Vec[i64]] = Map.new();\n\
               \x20   let b: Map[String, Vec[i64]] = Map.new();\n\
               \x20   let s: Set[String] = Set.new();\n\
               \x20   let t: Set[String] = Set.new();\n\
               \x20   println(f\"{a == b} {s != t}\");\n\
               }\n";
    let errs = check("eq_checks", src);
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn a_map_of_values_without_partial_eq_is_refused_without_a_derive_hint() {
    let src = "fn main() {\n\
               \x20   let a: Map[String, Fn(i64) -> i64] = Map.new();\n\
               \x20   let b: Map[String, Fn(i64) -> i64] = Map.new();\n\
               \x20   println(a == b);\n\
               }\n";
    let errs = check("fn_values", src);
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement PartialEq")),
        "{errs:?}"
    );
    assert!(errs.iter().all(|e| !e.contains("#[derive")), "{errs:?}");
}

#[test]
fn a_map_or_set_has_no_order_and_no_derive_hint() {
    let src = "fn main() {\n\
               \x20   let s: Set[i64] = Set.new();\n\
               \x20   let t: Set[i64] = Set.new();\n\
               \x20   println(s < t);\n\
               }\n";
    let errs = check("no_order", src);
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement PartialOrd")),
        "{errs:?}"
    );
    assert!(errs.iter().all(|e| !e.contains("#[derive")), "{errs:?}");
}

#[test]
fn a_program_type_still_gets_the_derive_hint() {
    let src = "struct Q { x: i64 }\n\
               fn main() { println(Q { x: 1 } == Q { x: 1 }); }\n";
    let errs = check("program_type", src);
    assert!(
        errs.iter().any(|e| e.contains("add #[derive(PartialEq)]")),
        "{errs:?}"
    );
}
