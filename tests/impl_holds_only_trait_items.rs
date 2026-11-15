//! design.md §9 "An impl holds only the trait's items" (B-2026-11-15-98):
//! a method or associated type in `impl Tr for X` that `Tr` does not
//! declare is an error, reported once. A misspelled override (`fn descibe`)
//! was accepted and the trait's default ran; an extra method was accepted
//! and callable.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn check(src: &str) -> (bool, String) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-impl-items-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn errors(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.starts_with("error"))
        .map(|l| l.split("main.kara:").nth(1).unwrap_or(l).to_string())
        .collect()
}

#[test]
fn impl_method_the_trait_lacks_is_refused() {
    let (ok, out) = check(
        r#"trait Tr {
    fn req(self) -> i64;
    fn describe(self) -> String { "default" }
}
struct A { x: i64 }
impl Tr for A {
    fn req(self) -> i64 { self.x }
    fn descibe(self) -> String { "mine" }
    fn extra(self) -> i64 { 99 }
}
fn main() {
    let a = A { x: 1 };
    println(a.req());
    println(a.describe());
}
"#,
    );
    assert!(!ok, "{out}");
    assert_eq!(
        errors(&out),
        [
            "8:5: method 'descibe' is not a member of trait 'Tr': an impl of 'Tr' for 'A' \
             holds only the trait's items; did you mean 'describe'?",
            "9:5: method 'extra' is not a member of trait 'Tr': an impl of 'Tr' for 'A' \
             holds only the trait's items; move it to an inherent `impl A`",
        ],
        "{out}"
    );
}

#[test]
fn impl_associated_type_the_trait_lacks_is_refused() {
    let (ok, out) = check(
        r#"trait Tr {
    type Out;
    fn get(self) -> Self.Out;
}
struct A { x: i64 }
impl Tr for A {
    type Out = i64;
    type Extra = bool;
    fn get(self) -> i64 { self.x }
}
fn main() {
    println(A { x: 3 }.get());
}
"#,
    );
    assert!(!ok, "{out}");
    assert_eq!(
        errors(&out),
        ["8:5: impl of trait 'Tr' for 'A' defines associated type 'Extra', which 'Tr' does not declare"],
        "{out}"
    );
}

#[test]
fn impl_with_the_traits_own_items_is_accepted() {
    let (ok, out) = check(
        r#"trait Tr {
    fn req(self) -> i64;
    fn describe(self) -> String { "default" }
}
struct A { x: i64 }
impl Tr for A {
    fn req(self) -> i64 { self.x }
}
impl A {
    fn extra(self) -> i64 { 99 }
}
struct P { x: i64 }
impl PartialEq for P { fn eq(ref self, other: ref P) -> bool { self.x == other.x } }
impl Eq for P {}
fn main() {
    let a = A { x: 1 };
    println(a.req() + a.extra());
    println(a.describe());
    println(P { x: 1 } == P { x: 1 });
}
"#,
    );
    assert!(ok, "{out}");
}
