//! design.md §9 Method resolution: in a generic body, lookup goes through
//! the bound, not the concrete type, and UFCS (`A.m(s)`) names the trait
//! whose method it calls. The pins: `T.label()` under `T: Named` calls
//! `Named`'s function over an inherent one (B-2026-11-04-9); `A.m(s)` is
//! accepted and calls `A`'s impl, also beside a `B.m` and in a generic body
//! under `T: A + B` (B-2026-11-04-8); `x.m()` under one bound calls that
//! bound's method when another trait of the type has an `m`
//! (B-2026-11-04-11); a program's associated function named like a
//! reflection method (`name`) is the program's (B-2026-11-04-10); and of
//! two `From` impls, `Out.from(b)` calls the one `b`'s type names. Each
//! checks clean and prints the same through the MIR interpreter and, with
//! the backend, `karac run`.

#[cfg(feature = "llvm")]
mod common;

use std::path::PathBuf;
use std::process::Command;

const CASES: &[(&str, &str, &str)] = &[
    (
        "assoc_fn_through_bound",
        r#"trait Named { fn label() -> String; }
struct P {}
impl P { fn label() -> String { "inherent".to_string() } }
impl Named for P { fn label() -> String { "trait".to_string() } }
fn via[T: Named](x: T) -> String { T.label() }
fn main() {
    println(P.label());
    println(via(P {}));
}
"#,
        "inherent\ntrait\n",
    ),
    (
        "ufcs_one_trait",
        r#"trait A {
    fn m(self) -> i64;
    fn d(self) -> i64 { self.m() + 10 }
}
struct S {}
impl A for S { fn m(self) -> i64 { 1 } }
fn main() {
    let s = S {};
    println(A.d(s));
    println(A.m(s));
}
"#,
        "11\n1\n",
    ),
    (
        "ufcs_two_traits",
        r#"trait A { fn m(self) -> i64; }
trait B { fn m(self) -> i64; }
struct S {}
impl A for S { fn m(self) -> i64 { 1 } }
impl B for S { fn m(self) -> i64 { 2 } }
fn g[T: A + B](x: T) -> i64 { A.m(x) * 10 + B.m(x) }
fn main() {
    let s = S {};
    println(A.m(s));
    println(B.m(s));
    println(g(S {}));
}
"#,
        "1\n2\n12\n",
    ),
    (
        "method_through_one_of_two_traits",
        r#"trait A { fn m(self) -> i64; }
trait B { fn m(self) -> i64; }
struct S {}
impl A for S { fn m(self) -> i64 { 1 } }
impl B for S { fn m(self) -> i64 { 2 } }
fn g[T: B](x: T) -> i64 { x.m() }
fn h[T: A](x: T) -> i64 { x.m() }
fn main() {
    println(g(S {}));
    println(h(S {}));
}
"#,
        "2\n1\n",
    ),
    (
        "assoc_fn_named_like_reflection",
        r#"trait Named { fn name() -> String; }
struct P {}
impl Named for P { fn name() -> String { "trait".to_string() } }
struct Q {}
impl Q { fn name() -> String { "inherent".to_string() } }
fn via[T: Named](x: T) -> String { T.name() }
fn main() {
    println(P.name());
    println(via(P {}));
    println(Q.name());
}
"#,
        "trait\ntrait\ninherent\n",
    ),
    (
        "from_impl_by_argument",
        r#"struct A { x: i64 }
struct B { y: i64 }
struct Out { v: i64 }
impl From[A] for Out { fn from(a: A) -> Out { Out { v: a.x + 1 } } }
impl From[B] for Out { fn from(b: B) -> Out { Out { v: b.y + 100 } } }
fn main() {
    println(Out.from(A { x: 1 }).v);
    println(Out.from(B { y: 1 }).v);
    let o: Out = B { y: 2 }.into();
    println(o.v);
}
"#,
        "2\n101\n102\n",
    ),
];

fn file(name: &str, src: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-trait-bound-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let f = d.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

fn karac(args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_AUTO_PAR", "0")
        .args(args)
        .output()
        .unwrap();
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

#[test]
fn trait_bound_dispatch_checks_and_interprets() {
    for (name, src, want) in CASES {
        let f = file(name, src);
        let path = f.to_str().unwrap();
        let (ok, out, err) = karac(&["check", path]);
        assert!(ok, "{name}: check: {out}{err}");
        let (ok, out, err) = karac(&["__mir-run", path]);
        assert!(ok, "{name}: interp: {err}");
        assert_eq!(out, *want, "{name}: interp");
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }
}

#[cfg(feature = "llvm")]
#[test]
fn trait_bound_dispatch_runs_compiled() {
    for (name, src, want) in CASES {
        let f = file(&format!("{name}-run"), src);
        let (ok, out, err) = karac(&["run", f.to_str().unwrap()]);
        let linked = if ok { Ok(()) } else { Err(err.clone()) };
        if common::link_or_skip(linked).is_none() {
            return;
        }
        assert_eq!(out, *want, "{name}: run");
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }
}
