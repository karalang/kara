//! B-2026-11-04-45 — core-semantics §5.5: a `let` that keeps a borrow of a
//! temporary is refused at the statement, "a reference to a temporary
//! outlives its statement". The list literal `["a"]` got that rule; a
//! prefix (`Vec["a"]`, `vec!["a"]`), repeat or map literal was refused only
//! later, by MIR's E0516, which another refusal could hide.
//!
//! B-2026-11-04-44 — design.md § Collection literals: "An empty literal,
//! `[]` or `[:]`, takes its element types from the expected type". An
//! unannotated `let w = [];` has none; it was refused with MIR's internal
//! "a type that failed to check is not lowered yet".

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp_file(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("karac-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn a_let_keeping_a_borrow_of_any_collection_literal_is_refused_at_the_statement() {
    for (i, lit) in [
        r#"["a", "b"]"#,
        r#"Vec["a", "b"]"#,
        r#"vec!["a", "b"]"#,
        r#"[["a"]; 2]"#,
    ]
    .into_iter()
    .enumerate()
    {
        let src =
            format!("fn main() {{\n    let g = {lit}.last();\n    println(g.unwrap());\n}}\n");
        let path = temp_file(&format!("lit-temp-{i}"), &src);
        let check = karac(&["check", path.to_str().unwrap()]);
        let _ = std::fs::remove_file(&path);
        let err = String::from_utf8_lossy(&check.stderr).to_string();
        assert!(!check.status.success(), "{lit}: {err}");
        assert!(
            err.contains(":2:13: a reference to a temporary outlives its statement"),
            "{lit}: {err}"
        );
        assert!(!err.contains("E0516"), "{lit}: {err}");
    }
    let src = "fn main() {\n    let g = [\"a\": 1].get(\"a\");\n    println(g.unwrap());\n}\n";
    let path = temp_file("map-lit-temp", src);
    let check = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    let err = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        err.contains(":2:13: a reference to a temporary outlives its statement"),
        "{err}"
    );
}

#[test]
fn an_unannotated_let_of_an_empty_list_literal_is_named_and_fixed() {
    let src = "fn main() {\n    let mut w = [];\n    w.push(1);\n    println(w);\n}\n";
    let path = temp_file("empty-let", src);
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(!check.status.success(), "{err}");
    assert!(
        (err.contains("error[E_EMPTY_PREFIX_LITERAL_NEEDS_ANNOTATION]: ")
            && err.contains(":2:17: cannot infer the element type of the empty list literal `[]`")),
        "{err}"
    );
    assert!(!err.contains("not lowered yet"), "{err}");
    karac(&["fix", file]);
    let out = std::fs::read_to_string(&path).unwrap();
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(out.contains("let mut w = Vec.new();"), "{out}");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "[1]\n", "{out}");
    // An annotation, or a call argument, gives `[]` its type as before.
    let src = "fn n(v: Vec[i64]) -> i64 {\n    v.len()\n}\n\
               fn main() {\n    let w: Vec[i64] = [];\n    println(w.len() + n([]));\n}\n";
    let path = temp_file("empty-typed", src);
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "0\n");
}
