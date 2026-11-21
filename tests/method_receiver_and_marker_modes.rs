//! Two method-call mode rules the conformance pass found open
//! (design.md § Method resolution, § Declarations and impls):
//!
//! - a `mut ref self` method on a binding whose type is a return-position
//!   `impl Trait` needs a `let mut` binding, as on the concrete type and on
//!   `dyn Trait` (B-2026-11-20-119);
//! - a `mut` marker on an argument to a method parameter that is not a
//!   mutable borrow is E0219, as at a function's parameter, and `karac fix`
//!   deletes it (B-2026-11-20-124).

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `karac check`'s stderr, the text `karac fix` leaves, whether it then
/// checks clean, and what it prints.
fn mend(name: &str, src: &str) -> (String, String, bool, String) {
    let dir =
        std::env::temp_dir().join(format!("karac-method-modes-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let first = karac(&dir, &["check", "cell.kara"]).2;
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(&file).unwrap();
    let (clean, _, _) = karac(&dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (first, text, clean, out)
}

const SHAPE: &str = "trait Shape {\n    fn area(self) -> i64;\n    fn grow(mut ref self, by: i64);\n}\n\nstruct Sq { s: i64 }\n\nimpl Shape for Sq {\n    fn area(self) -> i64 { self.s * self.s }\n    fn grow(mut ref self, by: i64) { self.s = self.s + by; }\n}\n\nfn make() -> impl Shape { Sq { s: 2 } }\n\n";

#[test]
fn a_mut_ref_self_method_on_an_impl_trait_let_needs_mut() {
    let src = format!(
        "{SHAPE}fn main() {{\n    let a = make();\n    a.grow(1);\n    println(a.area());\n}}"
    );
    let (first, text, clean, out) = mend("impl-let", &src);
    assert!(
        first.contains("cannot call `grow` on `a` — `a` is declared without `mut`"),
        "{first}"
    );
    assert!(text.contains("let mut a = make();"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "9\n");
}

#[test]
fn a_mut_ref_self_method_on_an_impl_trait_let_mut_is_accepted_without_unused_mut() {
    let src = format!("{SHAPE}fn main() {{\n    let mut a = make();\n    a.grow(1);\n    let b = make();\n    println(a.area() + b.area());\n}}");
    let (first, _, clean, out) = mend("impl-let-mut", &src);
    assert!(!first.contains("unused_mut"), "{first}");
    assert!(!first.contains("error"), "{first}");
    assert!(clean);
    assert_eq!(out, "13\n");
}

#[test]
fn an_iterator_from_an_impl_trait_fn_needs_a_let_mut_to_advance() {
    let src = "fn evens() -> impl Iterator[Item = i64] {\n    (0..10).filter(|x| x % 2 == 0)\n}\n\nfn main() {\n    let e = evens();\n    println(e.next().unwrap());\n}";
    let (first, text, clean, out) = mend("iterator", src);
    assert!(
        first.contains("cannot call `next` on `e` — `e` is declared without `mut`"),
        "{first}"
    );
    assert!(text.contains("let mut e = evens();"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "0\n");
}

#[test]
fn a_mut_marker_at_a_methods_borrowed_parameter_is_refused_and_fixed() {
    let src = "struct S { v: i64 }\n\nimpl S {\n    fn poke(self, n: i64) -> i64 { self.v + n }\n    fn bump(self, n: mut ref i64) { *n = *n + self.v; }\n}\n\nfn main() {\n    let s = S { v: 1 };\n    let mut n = 2;\n    s.bump(mut n);\n    println(s.poke(mut n));\n}";
    let (first, text, clean, out) = mend("marker", src);
    assert!(first.contains("error[E0219]"), "{first}");
    assert!(
        first.contains("the method's parameter is not a mutable borrow"),
        "{first}"
    );
    assert_eq!(first.matches("error[E0219]").count(), 1, "{first}");
    assert!(text.contains("println(s.poke(n));"), "{text}");
    assert!(text.contains("s.bump(mut n);"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "4\n");
}
