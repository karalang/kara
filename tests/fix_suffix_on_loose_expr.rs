//! Two `karac fix` edits found porting the mini_crdt app.
//!
//! B-2026-11-03-24: the fixes that append a method call to an expression
//! (`.to_string()` for a `Str` where a `String` is wanted, `.clone()`) were
//! inserted after the expression's last token, so on `*r` they produced
//! `*r.to_string()`, which binds the call to `r` and does not check. An
//! expression that binds looser than a postfix `.` is now parenthesized
//! first: `(*r).to_string()`.
//!
//! B-2026-11-03-25: `*a` on a value that is not a reference had a fix only
//! when the value is `Copy`. In a comparison the operands are borrowed, so
//! the `*` can go whatever the type: `el.id == *a` becomes `el.id == a`.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn fix_until_clean(name: &str, src: &str) -> (String, String) {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    let file = path.to_str().unwrap().to_string();
    std::fs::write(&path, src).unwrap();
    for _ in 0..3 {
        let _ = karac(&["fix", &file]);
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    let check = karac(&["check", &file]);
    assert!(
        check.status.success(),
        "{name}: {}\n{fixed}",
        String::from_utf8_lossy(&check.stderr)
    );
    let _ = std::fs::remove_file(&path);
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    (fixed, out)
}

#[test]
fn a_to_string_fix_on_a_deref_parenthesizes_it() {
    // A `push` wants the owned `String`. (A `contains` took one too until
    // B-2026-11-08-55; it now takes the `Str` as it is, which the last
    // line checks.)
    let src = "fn main() {
    let names: Vec[String] = [\"a\".to_string(), \"b\".to_string()].to_vec();
    let w: Vec[Str] = \"a c\".split(\" \").collect();
    let mut seen: Vec[String] = Vec.new();
    for r in w.iter() {
        seen.push(*r);
    }
    seen.push(w[0]);
    if names.contains(w[0]) {
        println(seen.join(\",\"));
    }
}
";
    let (fixed, out) = fix_until_clean("suffix-deref", src);
    assert!(fixed.contains("seen.push((*r).to_string())"), "{fixed}");
    assert!(fixed.contains("seen.push(w[0].to_string())"), "{fixed}");
    assert!(fixed.contains("names.contains(w[0]) {"), "{fixed}");
    assert_eq!(out, "a,c,a\n");
}

#[test]
fn a_deref_compared_with_eq_loses_its_star() {
    let src = "fn find(ids: Vec[(i64, String)], after: own Option[(i64, String)]) -> i64 {
    match after {
        Some(a) => ids.iter().position(|id| *id == *a) ?? -1,
        None => -1,
    }
}

fn main() {
    let ids = [(1, \"x\".to_string()), (2, \"y\".to_string())].to_vec();
    println(find(ids, Some((2, \"y\".to_string()))));
}
";
    let path = std::env::temp_dir().join(format!("karac-cmp-star-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let check = karac(&["check", path.to_str().unwrap()]);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    let _ = std::fs::remove_file(&path);
    assert!(
        err.contains("a comparison borrows its operands, so drop the `*`"),
        "{err}"
    );
    let (fixed, out) = fix_until_clean("cmp-star", src);
    assert!(fixed.contains("*id == a"), "{fixed}");
    assert_eq!(out, "1\n");
}
