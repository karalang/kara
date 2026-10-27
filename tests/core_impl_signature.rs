//! B-2026-10-27-9: an impl method's signature is its trait method's, read at
//! the impl (`Self` the impl's type, `Self.A` the impl's own binding, the
//! trait's and the method's generics the impl's). A caller through a bound
//! types the call from the trait, so `fn mk2(self) -> Option[i64]` under
//! `type A = bool` printed `7` as a `bool`. `karac check` now refuses it,
//! and an associated type the trait does not declare.

use std::process::Command;

fn check(tag: &str, src: &str) -> (bool, String) {
    let dir =
        std::env::temp_dir().join(format!("karac-impl-signature-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn an_impl_return_type_other_than_the_traits_is_refused() {
    let (ok, err) = check(
        "ret",
        r#"
trait Mk2 { type A; fn mk2(self) -> Option[Self.A]; }
struct S { }
impl Mk2 for S { type A = bool; fn mk2(self) -> Option[i64] { Some(7) } }
fn get[T: Mk2[A = bool]](t: T) -> bool { t.mk2().unwrap() }
fn main() { println(get(S { })); }
"#,
    );
    assert!(!ok, "{err}");
    assert!(
        err.contains(
            "the return type of impl method 'S.mk2' is `Option[i64]`, but trait 'Mk2' declares \
             `Option[bool]` there"
        ),
        "{err}"
    );
}

#[test]
fn an_impl_parameter_or_generic_return_other_than_the_traits_is_refused() {
    let (ok, err) = check(
        "param",
        r#"
trait T2 { type A; fn put(mut ref self, a: Self.A); fn gen[U](self, u: own U) -> U; }
struct S { v: bool }
impl T2 for S {
    type A = bool;
    fn put(mut ref self, a: i64) { }
    fn gen[V](self, u: own V) -> i64 { 1 }
}
fn main() { }
"#,
    );
    assert!(!ok, "{err}");
    assert!(
        err.contains(
            "parameter 'a' of impl method 'S.put' is `i64`, but trait 'T2' declares `bool` there"
        ),
        "{err}"
    );
    assert!(
        err.contains(
            "the return type of impl method 'S.gen' is `i64`, but trait 'T2' declares `V` there"
        ),
        "{err}"
    );
}

#[test]
fn an_impl_associated_type_the_trait_does_not_declare_is_refused() {
    let (ok, err) = check(
        "extra",
        r#"
trait Tr { type A; fn get(self) -> Self.A; }
struct S { }
impl Tr for S { type A = i64; type B = bool; fn get(self) -> i64 { 1 } }
fn main() { println(S { }.get()); }
"#,
    );
    assert!(!ok, "{err}");
    assert!(
        err.contains(
            "impl of trait 'Tr' for 'S' defines associated type 'B', which 'Tr' does not declare"
        ),
        "{err}"
    );
}

#[test]
fn an_impl_matching_its_trait_at_the_impl_is_accepted() {
    let src = r#"
trait Mk2 { type A; fn mk2(self) -> Option[Self.A]; fn put(mut ref self, a: Self.A); fn gen[U](self, u: own U) -> U; fn me(self) -> Self; }
struct S { v: bool }
impl Mk2 for S {
    type A = bool;
    fn mk2(self) -> Option[Self.A] { Some(self.v) }
    fn put(mut ref self, a: bool) { self.v = a; }
    fn gen[V](self, u: own V) -> V { u }
    fn me(self) -> Self { S { v: self.v } }
}
trait Conv[T] { fn conv(self) -> T; }
impl Conv[i64] for S { fn conv(self) -> i64 { 1 } }
struct W[T] { x: T }
impl[T: Copy] Conv[T] for W[T] { fn conv(self) -> T { self.x } }
trait R { fn fold[A](ref self, init: own A, f: own Fn(A, i64) -> A) -> A; }
impl R for S { fn fold[A](ref self, init: own A, f: own Fn(A, i64) -> A) -> A { f(init, 2) } }
fn main() {
    let mut s = S { v: true };
    s.put(false);
    println(s.mk2().unwrap());
    println(s.gen(3));
    println(s.conv());
    println(W { x: 5 }.conv());
    println(s.fold(1, |a, b| a + b));
}
"#;
    let (ok, err) = check("ok", src);
    assert!(ok, "{err}");
    let out = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(out.output, "false\n3\n1\n5\n3\n");
}
