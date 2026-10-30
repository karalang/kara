//! Which traits can be used as `dyn` (design.md § `dyn Trait`): a `dyn`
//! type is `E_DYN_NOT_DISPATCHABLE` when a required method or associated
//! function of the trait or a supertrait cannot go in the method table (it
//! has no receiver, has type parameters of its own, or names `Self` outside
//! the receiver), and the error names the first one. A default method
//! needs no slot, and `Clone` is the one supertrait whose `Self` return is
//! allowed.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-dyndisp-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_dyn_of_a_trait_with_an_undispatchable_required_method_is_refused() {
    for (name, src, method, why) in [
        (
            "generic_method",
            r#"trait T { fn get(self) -> i64; fn conv[U](self, u: U) -> i64; }
struct S { n: i64 }
impl T for S { fn get(self) -> i64 { self.n } fn conv[U](self, u: U) -> i64 { 0 } }
fn main() {
    let d: dyn T = S { n: 2 };
    println(d.get());
}
"#,
            "`conv`",
            "type or const parameters of its own",
        ),
        (
            "associated_function",
            r#"trait T { fn get(self) -> i64; fn make() -> Self; }
struct S { n: i64 }
impl T for S { fn get(self) -> i64 { self.n } fn make() -> Self { S { n: 0 } } }
fn main() {
    let d: dyn T = S { n: 2 };
    println(d.get());
}
"#,
            "`make`",
            "it has no receiver",
        ),
        (
            "self_parameter",
            r#"trait T { fn get(self) -> i64; fn same(self, o: ref Self) -> bool; }
struct S { n: i64 }
impl T for S { fn get(self) -> i64 { self.n } fn same(self, o: ref Self) -> bool { true } }
fn main() {
    let d: dyn T = S { n: 2 };
    println(d.get());
}
"#,
            "`same`",
            "names `Self` outside the receiver",
        ),
        (
            "supertrait_method",
            r#"trait B { fn mk() -> i64; }
trait T: B { fn get(self) -> i64; }
struct S { n: i64 }
impl B for S { fn mk() -> i64 { 1 } }
impl T for S { fn get(self) -> i64 { self.n } }
fn main() {
    let d: dyn T = S { n: 2 };
    println(d.get());
}
"#,
            "`mk` (from its supertrait `B`)",
            "it has no receiver",
        ),
    ] {
        let (ok, err) = check(name, src);
        assert!(!ok, "{name}: expected E_DYN_NOT_DISPATCHABLE");
        assert!(err.contains("E_DYN_NOT_DISPATCHABLE"), "{name}: {err}");
        assert!(err.contains(method), "{name}: {err}");
        assert!(err.contains(why), "{name}: {err}");
    }
}

#[test]
fn a_default_method_and_a_clone_supertrait_need_no_slot() {
    for (name, src, want) in [
        (
            "default_generic_method",
            r#"trait T { fn get(self) -> i64; fn conv[U](self, u: U) -> i64 { self.get() } }
struct S { n: i64 }
impl T for S { fn get(self) -> i64 { self.n } }
fn main() {
    let d: dyn T = S { n: 2 };
    println(d.get());
}
"#,
            "2\n",
        ),
        (
            "clone_supertrait",
            r#"trait T: Clone { fn get(self) -> i64; }
#[derive(Clone)]
struct S { n: i64 }
impl T for S { fn get(self) -> i64 { self.n } }
fn main() {
    let d: dyn T = S { n: 2 };
    let e = d.clone();
    println(d.get() + e.get());
}
"#,
            "4\n",
        ),
    ] {
        let (ok, err) = check(name, src);
        assert!(ok, "{name}: {err}");
        assert_eq!(
            karac::mir::lower::run_source(src)
                .expect("MIR interpreter")
                .output,
            want,
            "{name}"
        );
    }
}
