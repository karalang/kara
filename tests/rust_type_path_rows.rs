//! B-2026-11-19-34: a Rust `::` path in a type position
//! (`let set: std::collections::HashSet<char> = ..`, a parameter, a return,
//! a field) stopped the parse with E0002 "Expected `=`, found `::`" and no
//! fix, though the same path in an expression was fixed. The type parser now
//! reports and fixes it the same way and parses on at the kept name.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// `karac check`'s output, the file after `karac fix` run until it changes
/// nothing (at most four rounds), and the check after.
fn check_fix_check(tag: &str, src: &str) -> (String, String, String) {
    let path = std::env::temp_dir().join(format!(
        "karac-rust-type-path-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let before = String::from_utf8_lossy(&karac(&["check"], &path).stderr).into_owned();
    for _ in 0..4 {
        let was = std::fs::read_to_string(&path).unwrap();
        let _ = karac(&["fix"], &path);
        if std::fs::read_to_string(&path).unwrap() == was {
            break;
        }
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    let after = String::from_utf8_lossy(&karac(&["check"], &path).stderr).into_owned();
    let _ = std::fs::remove_file(&path);
    (before, fixed, after)
}

#[test]
fn a_std_path_in_a_let_annotation_is_fixed_to_the_prelude_type() {
    let src = "fn main() {\n\
               \x20   let set: std::collections::HashSet<char> = \"abc\".chars().collect();\n\
               \x20   println(set.len());\n\
               }\n";
    let (before, fixed, after) = check_fix_check("let", src);
    assert!(!before.contains("E0002"), "{before}");
    assert!(
        before.contains("`std::collections::HashSet` is a Rust path"),
        "{before}"
    );
    assert!(fixed.contains("let set: Set[char] = "), "{fixed}");
    assert!(after.contains("All checks passed"), "{after}");
}

#[test]
fn std_paths_in_parameter_return_and_field_types_are_fixed() {
    let src = "fn f(s: std::collections::HashSet<char>) -> std::collections::HashMap<i32, i32> {\n\
               \x20   let m: std::collections::HashMap<i32, i32> = std::collections::HashMap::new();\n\
               \x20   m\n\
               }\n\
               struct W { m: std::collections::HashMap<i32, i32>, o: std::cmp::Ordering }\n\
               fn main() {\n\
               \x20   let s: std::collections::HashSet<char> = \"ab\".chars().collect();\n\
               \x20   let w = W { m: f(s), o: std::cmp::Ordering::Less };\n\
               \x20   println(w.m.len());\n\
               }\n";
    let (before, fixed, after) = check_fix_check("positions", src);
    assert!(!before.contains("E0002"), "{before}");
    assert!(
        fixed.contains("fn f(s: Set[char]) -> Map[i32, i32] {"),
        "{fixed}"
    );
    assert!(
        fixed.contains("struct W { m: Map[i32, i32], o: Ordering }"),
        "{fixed}"
    );
    assert!(after.contains("All checks passed"), "{after}");
}

#[test]
fn a_module_path_in_a_type_is_written_with_dots() {
    let src = "fn f(o: cmp::Ordering) -> bool { o == Ordering.Less }\n\
               fn main() { println(f(Ordering.Less)); }\n";
    let (before, fixed, _) = check_fix_check("module", src);
    assert!(!before.contains("E0002"), "{before}");
    assert!(fixed.contains("fn f(o: Ordering) -> bool"), "{fixed}");
}
