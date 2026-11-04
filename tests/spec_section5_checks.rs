//! design.md §5 rules `karac check` did not hold (B-2026-11-04-58,
//! B-2026-11-04-60): a list literal builds a `Set` where one is expected, an
//! or-pattern's alternatives bind the same names, an alias's unsatisfied
//! bound is one E0262 naming the whole bound, and a bound the aliased type
//! already requires draws `redundant_alias_bound`.

use std::process::{Command, Stdio};

fn check(tag: &str, src: &str) -> (bool, String) {
    let path = std::env::temp_dir().join(format!("karac-s5-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn an_or_pattern_whose_alternatives_bind_different_names_is_refused() {
    let (ok, err) = check(
        "or-names",
        "enum E { A(i64), B(i64) }\nfn main() {\n    let e = E.A(1);\n    match e {\n        \
         E.A(x) | E.B(y) => println(1),\n    }\n}\n",
    );
    assert!(!ok, "{err}");
    assert!(
        err.contains(":5:18: every alternative of an or-pattern binds the same names"),
        "{err}"
    );
    let (ok, err) = check(
        "or-names-ok",
        "enum E { A(i64), B(i64), C }\nfn f(o: Option[i64]) -> i64 {\n    match o {\n        \
         Some(0) | None => 0,\n        Some(n) => n,\n    }\n}\nfn main() {\n    \
         match E.A(1) {\n        E.A(x) | E.B(x) => println(x + f(None)),\n        \
         E.C => println(0),\n    }\n}\n",
    );
    assert!(
        ok,
        "the same names, and a unit variant, bind nothing new: {err}"
    );
}

#[test]
fn an_unsatisfied_alias_bound_is_one_e0262_naming_the_whole_bound() {
    let (ok, err) = check(
        "alias-bound",
        "type Counts[T: Eq + Hash] = Map[T, i64];\nfn main() {\n    \
         let by_score: Counts[f64] = Map.new();\n    println(by_score.len());\n}\n",
    );
    assert!(!ok, "{err}");
    assert_eq!(
        err.matches("does not satisfy").count(),
        1,
        "one error per parameter: {err}"
    );
    assert!(
        err.contains("'f64' does not satisfy 'T: Eq + Hash' required by alias 'Counts'"),
        "{err}"
    );
    // The spec's showcase bound is not called redundant.
    assert!(!err.contains("redundant_alias_bound"), "{err}");
}

#[test]
fn a_bound_the_aliased_type_already_requires_draws_redundant_alias_bound() {
    let (ok, err) = check(
        "alias-redundant",
        "struct Bag[T: Ord] { items: Vec[T] }\ntype Sk[T: Ord] = SortedMap[T, i32];\n\
         type B[T: Ord] = Bag[T];\n#[allow(redundant_alias_bound)]\ntype Kept[T: Ord] = SortedSet[T];\n\
         type SortedVec[T: Ord] = Vec[T];\nfn main() {\n    let m: Sk[i64] = SortedMap.new();\n    \
         let b: B[i64] = Bag { items: Vec.new() };\n    let k: Kept[i64] = SortedSet.new();\n    \
         let v: SortedVec[i64] = Vec.new();\n    \
         println(m.len() + b.items.len() + k.len() + v.len());\n}\n",
    );
    assert!(ok, "a warning, not an error: {err}");
    assert!(
        err.contains(
            "warning[redundant_alias_bound]: ".to_string().as_str()
        ) && err.contains(
            "bound 'T: Ord' on alias 'Sk' is implied by the underlying type 'SortedMap[T, i32]'; consider removing"
        ),
        "{err}"
    );
    assert!(
        err.contains("alias 'B' is implied by the underlying type 'Bag[T]'"),
        "{err}"
    );
    assert_eq!(
        err.matches("warning[redundant_alias_bound]").count(),
        2,
        "{err}"
    );
}

#[test]
fn fmt_keeps_the_parentheses_of_an_at_binding_over_an_or_pattern() {
    let src = "fn f(n: i64) -> i64 {\n    match n {\n        x @ (1 | 2 | 3) => x * 100,\n        other => other,\n    }\n}\n";
    let out = karac::format_source(src).expect("formats");
    assert!(out.contains("x @ (1 | 2 | 3) =>"), "{out}");
    let (ok, diag) = check("fmt_at_or", &out);
    assert!(ok, "the formatted program still checks: {diag}");
}
