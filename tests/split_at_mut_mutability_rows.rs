//! B-2026-11-19-23: `v.split_at_mut(i)` on a `Vec` binding declared without
//! `mut` checked clean, and its halves wrote through the binding; with
//! `let mut v` the unused_mut lint said the `mut` was never used, so
//! following it led to the unsound form. The call now counts as a `mut ref
//! self` call, as `Array.split_at_mut` and `Vec.sort` already did.

use std::process::{Command, Stdio};

fn check(tag: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "karac-split-at-mut-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn split_at_mut_on_a_vec_let_without_mut_is_refused() {
    let out = check(
        "let",
        "fn main() { let v = [1, 2, 3, 4]; let (l, r) = v.split_at_mut(2); l[0] = 0; \
         r[0] = 9; println(v); }\n",
    );
    assert!(out.contains("error[E0510]"), "{out}");
    assert!(out.contains("cannot call `split_at_mut` on `v`"), "{out}");
}

#[test]
fn split_at_mut_uses_the_mut_of_a_vec_let() {
    let out = check(
        "let-mut",
        "fn main() { let mut v = [1, 2, 3, 4]; let (l, r) = v.split_at_mut(2); l[0] = 0; \
         r[0] = 9; println(v); }\n",
    );
    assert!(!out.contains("unused_mut"), "{out}");
    assert!(out.contains("All checks passed"), "{out}");
}

#[test]
fn split_at_mut_on_a_field_of_a_let_without_mut_is_refused() {
    let out = check(
        "field",
        "struct S { v: Vec[i64] }\n\
         fn main() { let s = S { v: [1, 2] }; let (l, _r) = s.v.split_at_mut(1); l[0] = 5; \
         println(s.v); }\n",
    );
    assert!(out.contains("error[E0510]"), "{out}");
}
