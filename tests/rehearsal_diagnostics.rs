//! Two rough edges from the v0.9.0 fresh-user walkthrough rehearsal.
//!
//! `println!("{} {}", c.n, s.replace("a", "b"))` got the macro message and no
//! edit, because the rewrite refused any argument holding a quote; an f-string
//! hole balances quotes, so it is carried as written (B-2026-11-01-13).
//!
//! `karac run` inside a `karac new --lib` project said "missing file argument"
//! and suggested running `karac run` inside a project; it now names the library
//! and points at `karac test` (B-2026-11-01-14).

use std::process::Command;

#[test]
fn println_macro_argument_with_a_string_literal_gets_the_f_string_edit() {
    let src = "struct C { n: i64 }\nfn main() {\n    let c = C { n: 1 };\n    let s = \"abc\".to_string();\n    println!(\"{} {}\", c.n, s.replace(\"a\", \"x\"));\n    eprintln!(\"[{}]\", \"lit\");\n}\n";
    let result = karac::parse(src);
    assert_eq!(result.errors.len(), 2, "{:?}", result.errors);
    let mut edits: Vec<_> = result.fix_edits.values().cloned().collect();
    assert_eq!(edits.len(), 2, "{edits:?}");
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut out = src.to_string();
    for e in edits {
        out.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    assert!(
        out.contains("println(f\"{c.n} {s.replace(\"a\", \"x\")}\");"),
        "{out}"
    );
    assert!(out.contains("eprintln(f\"[{\"lit\"}]\");"), "{out}");
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "1 xbc\n");
}

#[test]
fn karac_run_in_a_library_project_names_the_library() {
    let dir = std::env::temp_dir().join(format!("karac-lib-run-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let new = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["new", "mylib", "--lib"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(new.status.success(), "{new:?}");
    let run = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("run")
        .current_dir(dir.join("mylib"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&run.stderr);
    assert_eq!(run.status.code(), Some(1), "{err}");
    assert_eq!(
        err,
        "error: library package `mylib` has no src/main.kara to run\n  help: a library has no `main`: `karac test` runs its tests, and the packages that depend on it run its code\n"
    );
}
