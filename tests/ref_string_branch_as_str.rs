//! A `ref String` branch beside a `Str` or a string literal (B-2026-11-13-74,
//! design.md §8): Rust's `if c { ".." } else { &arg }` reaches the check as
//! `ref arg`, which is a `Str` only as a call argument, so `karac fix` writes
//! `arg.as_str()`; a string literal beside a `Str` branch is then a `Str`,
//! with or without an annotation.

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-ref-string-branch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

/// `karac fix` until it has nothing left to apply, as the Mend loop runs it.
fn fix_to_fixpoint(path: &std::path::Path) {
    for _ in 0..5 {
        let out = karac(&["fix"], path);
        if text(&out).contains("no fixable") {
            return;
        }
    }
    panic!("karac fix did not settle on {}", path.display());
}

#[test]
fn fix_writes_as_str_for_a_borrowed_string_branch_beside_a_literal() {
    let path = scratch(
        "fix.kara",
        "fn show(s: Str) { println(s); }
fn main() {
    let arg = \"x\".to_string();
    let up = false;
    show(if up { \"..\" } else { &arg });
    let s: Str = if up { \"..\" } else { &arg };
    show(s);
    let w = match up { true => \"..\", false => &arg };
    show(w);
    let v = if not up { let k = 1; println(k); &arg } else { \"..\" };
    show(v);
}
",
    );
    fix_to_fixpoint(&path);
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(
        src.contains("show(if up { \"..\" } else { arg.as_str() });"),
        "{src}"
    );
    assert!(
        src.contains("let s: Str = if up { \"..\" } else { arg.as_str() };"),
        "{src}"
    );
    assert!(
        src.contains("let w = match up { true => \"..\", false => arg.as_str() };"),
        "{src}"
    );
    assert!(src.contains("println(k); arg.as_str() }"), "{src}");
    assert!(!src.contains("ref arg"), "{src}");
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "x\nx\nx\n1\nx\n",
        "{}",
        text(&run)
    );
}

#[test]
fn a_ref_string_branch_beside_a_literal_is_refused_with_the_as_str_hint() {
    let path = scratch(
        "refused.kara",
        "fn main() {
    let arg = \"x\".to_string();
    let up = false;
    let s = if up { \"..\" } else { ref arg };
    println(s);
}
",
    );
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    assert!(
        text(&check).contains("a `ref String` is a `Str` only as a call argument"),
        "{}",
        text(&check)
    );
}

#[test]
fn a_string_literal_branch_beside_a_str_branch_joins_as_str_unannotated() {
    let path = scratch(
        "join.kara",
        "fn show(s: Str) { println(s); }
fn main() {
    let arg = \"x\".to_string();
    let up = false;
    let s = if up { \"..\" } else { arg.as_str() };
    show(s);
    let u = if not up { \"..\" } else { arg.as_str() };
    show(u);
    let w = match up { true => \"..\", false => arg.as_str() };
    show(w);
}
",
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "x\n..\nx\n",
        "{}",
        text(&run)
    );
}

#[test]
fn a_string_literal_branch_beside_a_str_branch_checks_against_a_str_annotation() {
    let path = scratch(
        "annotated.kara",
        "fn main() {
    let arg = \"x\".to_string();
    let up = true;
    let s: Str = if up { \"..\" } else { arg.as_str() };
    let t: Str = if up { arg.as_str() } else { \"..\" };
    println(f\"{s} {t}\");
}
",
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        ".. x\n",
        "{}",
        text(&run)
    );
}
