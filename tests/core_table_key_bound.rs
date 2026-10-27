//! B-2026-10-27-25: `K: Hash + Eq` is a bound of every `Map` and `Set`,
//! held wherever the table type is written (collections.md § Map). A key
//! that is a type parameter needs the bound on the item declaring the
//! parameter, and `karac fix` adds it there; a concrete key needs the impls,
//! and a generic type's derive holds only at arguments that have the trait.

use std::process::Command;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-table-key-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `karac check` on `src`: whether it passes, and its stderr.
fn check(tag: &str, src: &str) -> (bool, String) {
    let dir = scratch(tag);
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// `src` after `karac fix`, and whether the fixed program checks.
fn fixed(tag: &str, src: &str) -> (String, bool) {
    let dir = scratch(tag);
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let fix = Command::new(karac).arg("fix").arg(&file).output().unwrap();
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let after = std::fs::read_to_string(&file).unwrap();
    let ok = Command::new(karac)
        .arg("check")
        .arg(&file)
        .output()
        .unwrap()
        .status
        .success();
    let _ = std::fs::remove_dir_all(&dir);
    (after, ok)
}

const PARAM_KEYS: &str = "struct Matcher[T] { b2j: Map[T, Vec[i64]], junk: Set[T] }\n\
                          fn pairs[T](xs: Vec[T]) -> Set[(T, i64)] { Set.new() }\n\
                          fn count[T](xs: Vec[T]) -> i64 { let s: Set[T] = Set.new(); s.len() }\n\
                          enum E[K, V] { M(Map[K, V]) }\n\
                          fn main() { println(count(vec![1])); }\n";

#[test]
fn a_type_parameter_key_needs_the_bound_where_it_is_declared() {
    let (ok, err) = check("param", PARAM_KEYS);
    assert!(!ok);
    for want in [
        "main.kara:1:26: the key type `T` of this `Map` must implement `Hash + Eq`, but the type parameter `T` is not bounded by `Hash + Eq`",
        "main.kara:2:28: the element type `(T, i64)` of this `Set` must implement `Hash + Eq`",
        "main.kara:3:41: the element type `T` of this `Set` must implement `Hash + Eq`",
        "main.kara:4:18: the key type `K` of this `Map` must implement `Hash + Eq`",
    ] {
        assert!(err.contains(want), "want {want:?} in\n{err}");
    }
    // `Matcher`'s two tables share one declaration of `T`, so one report.
    assert_eq!(err.matches("main.kara:1:").count(), 1, "{err}");
}

#[test]
fn karac_fix_writes_the_bound_on_each_declaration() {
    let (after, ok) = fixed("param-fix", PARAM_KEYS);
    assert!(ok, "{after}");
    for want in [
        "struct Matcher[T: Hash + Eq] {",
        "fn pairs[T: Hash + Eq](xs: Vec[T])",
        "fn count[T: Hash + Eq](xs: Vec[T])",
        "enum E[K: Hash + Eq, V] {",
    ] {
        assert!(after.contains(want), "want {want:?} in\n{after}");
    }
}

#[test]
fn a_bound_through_a_supertrait_or_a_where_clause_holds() {
    let (ok, err) = check(
        "supertrait",
        "trait Key: Hash + Eq {}\n\
         struct A[K: Key] { m: Map[K, i64] }\n\
         struct B[K: Ord + Hash] { s: Set[K] }\n\
         struct C[K] where K: Hash + Eq { m: Map[K, i64] }\n\
         impl[K: Hash + Eq] C[K] { fn n(self) -> i64 { let m: Map[K, i64] = Map.new(); m.len() } }\n\
         fn main() {\n\
             let a: Map[Option[i64], (String, bool)] = Map.new();\n\
             let b: Set[Vec[(i64, char)]] = Set.new();\n\
             let c: Set[Result[i64, String]] = Set.new();\n\
             println(a.len() + b.len() + c.len());\n\
         }\n",
    );
    assert!(ok, "{err}");
}

#[test]
fn a_concrete_key_without_the_impls_is_refused_where_the_type_is_written() {
    let (ok, err) = check(
        "concrete",
        "struct P { x: f64 }\n\
         #[derive(Hash, PartialEq, Eq)]\n\
         struct Pair[T] { a: T }\n\
         struct H { m: Map[P, i64] }\n\
         fn f(m: ref Map[f64, i64]) -> i64 { 0 }\n\
         fn main() { let s: Set[Pair[f64]] = Set.new(); println(s.len()); }\n",
    );
    assert!(!ok);
    for want in [
        "main.kara:4:15: the key type `P` of this `Map` must implement `Hash + Eq`, but it does not implement `Hash` and `Eq`",
        "main.kara:5:13: the key type `f64` of this `Map` must implement `Hash + Eq`, but it does not implement `Hash` and `Eq`. A float has neither",
        "main.kara:6:20: the element type `Pair[f64]` of this `Set` must implement `Hash + Eq`, but it does not implement `Hash` and `Eq`. A derive on `Pair` holds only where its type arguments have the traits.",
    ] {
        assert!(err.contains(want), "want {want:?} in\n{err}");
    }
    // The method call on the refused table does not report the key again.
    assert_eq!(err.matches("main.kara:6:").count(), 1, "{err}");
}

#[test]
fn karac_fix_derives_the_impls_on_a_key_whose_fields_have_them() {
    let src = "struct P { x: i64, s: String }\n\
               fn main() {\n\
                   let mut m: Map[P, i64] = Map.new();\n\
                   m.insert(P { x: 1, s: \"a\".to_string() }, 2);\n\
                   println(m.len());\n\
               }\n";
    let (after, ok) = fixed("derive-fix", src);
    assert!(ok, "{after}");
    assert!(
        after.starts_with("#[derive(Hash, PartialEq, Eq)]\nstruct P {"),
        "{after}"
    );
    let out = karac::mir::lower::run_source(&after).expect("MIR interpreter");
    assert_eq!(out.output, "1\n");
}
