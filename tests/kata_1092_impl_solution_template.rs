//! LeetCode's Rust template is `impl Solution { pub fn f(..) }` with no
//! `struct Solution;`, which the site supplies (kata 1092). An inherent
//! impl of a type nothing declares, whose functions take no `self`, uses
//! the type only as a namespace, so `karac fix` declares it,
//! `struct Solution {}`, above the impl (B-2026-11-10-21). Before, `karac
//! check` said only "undefined type" and the fix had nothing to apply.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("kata-1092-impl-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

fn both(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout)
}

const NAMESPACE: &str = "impl Solution {
    pub fn twice(x: i64) -> i64 {
        Solution.add(x, x)
    }

    fn add(a: i64, b: i64) -> i64 {
        a + b
    }
}

fn main() {
    println(f\"{Solution.twice(21)}\");
}
";

#[test]
fn fix_declares_the_struct_a_namespace_impl_names() {
    let (dir, path) = fixture("ns", NAMESPACE);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let text = both(&out);
    assert!(
        !out.status.success() && text.contains("`struct Solution {}`"),
        "check must refuse and name the struct: {text}"
    );
    let out = karac().arg("fix").arg(&path).output().unwrap();
    assert!(out.status.success(), "fix: {}", both(&out));
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.starts_with("struct Solution {}\n\nimpl Solution {"),
        "fix must declare the struct above the impl:\n{after}"
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "the fixed program must check: {}",
        both(&out)
    );
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "42\n",
        "{}",
        both(&out)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A method taking `self` needs fields the fix cannot know, so the type
/// is left undeclared and the file untouched.
#[test]
fn fix_leaves_an_impl_with_a_self_method_alone() {
    let src = "impl Counter {
    fn get(self) -> i64 {
        0
    }
}

fn main() {}
";
    let (dir, path) = fixture("self", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let text = both(&out);
    assert!(
        !out.status.success() && !text.contains("struct Counter {}"),
        "check must refuse without offering a struct: {text}"
    );
    let _ = karac().arg("fix").arg(&path).output().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), src);
    let _ = std::fs::remove_dir_all(&dir);
}
