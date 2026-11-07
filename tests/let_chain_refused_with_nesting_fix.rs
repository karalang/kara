//! `if let P = v and c { .. }` is Rust's let-chain, which Kāra does not
//! have. It reported a `bool` operand error and a pattern-against-`bool`
//! mismatch with no word about the shape (B-2026-11-06-39). It is now one
//! parse error naming it, and with no `else` `karac fix` nests the test.

use std::process::Command;

fn karac(args: &[&str], dir: &std::path::Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-letchain-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    dir
}

const CHAIN: &str = "fn main() {
    let v: Option[String] = Some(\"x\".to_string());
    let kind = \"group\";
    if let Some(s) = v and kind == \"group\" {
        println(s);
    }
}
";

#[test]
fn a_let_chain_is_named_and_fixed_by_nesting() {
    let dir = scratch("fix", CHAIN);
    let (ok, out) = karac(&["check", "main.kara"], &dir);
    assert!(!ok);
    assert!(out.contains("no let-chains"), "{out}");
    assert!(!out.contains("logical operator requires"), "{out}");
    let (_, out) = karac(&["fix", "main.kara"], &dir);
    let fixed = std::fs::read_to_string(dir.join("main.kara")).unwrap();
    assert!(
        fixed.contains("if let Some(s) = v { if kind == \"group\" {"),
        "{out}\n{fixed}"
    );
    let (ok, out) = karac(&["run", "main.kara"], &dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{out}");
    assert_eq!(out, "x\n");
}

#[test]
fn a_let_chain_with_an_else_is_named_but_not_rewritten() {
    let src = CHAIN.replace(
        "        println(s);\n    }\n",
        "        println(s);\n    } else {\n        println(\"no\");\n    }\n",
    );
    let dir = scratch("else", &src);
    let (ok, out) = karac(&["check", "main.kara"], &dir);
    assert!(!ok);
    assert!(out.contains("no let-chains"), "{out}");
    let _ = karac(&["fix", "main.kara"], &dir);
    let after = std::fs::read_to_string(dir.join("main.kara")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(after, src, "nesting would change what the `else` covers");
}

#[test]
fn a_bool_pattern_over_an_and_condition_is_not_a_let_chain() {
    let dir = scratch(
        "bool",
        "fn main() {
    let a = true;
    let b = false;
    if let true = a and b {
        println(\"both\");
    } else {
        println(\"not both\");
    }
}
",
    );
    let (ok, out) = karac(&["run", "main.kara"], &dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{out}");
    assert_eq!(out, "not both\n");
}

/// The same row's side item: Rust's `Vec.last_mut` is not in the library,
/// and the bare "no method" now says to write through an index.
#[test]
fn vec_last_mut_points_at_indexing() {
    let src = "fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    if let Some(x) = v.last_mut() {
        println(x);
    }
}
";
    let dir = scratch("lastmut", src);
    let (ok, out) = karac(&["check", "main.kara"], &dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok);
    assert!(out.contains("no method 'last_mut'"), "{out}");
    assert!(out.contains("v[v.len() - 1]"), "{out}");
}
