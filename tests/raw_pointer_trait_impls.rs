//! A trait impl for a raw-pointer type (B-2026-11-21-81, review-thread
//! ruling 2026-11-21: dispatch, don't refuse). Such an impl was accepted and
//! then never callable: every call was a root-less E0516, then E0236 "no
//! method". Now a pointer to a concrete type answers its impl's methods,
//! defaults, `Trait.m(p)`, bounds and `dyn`, `*mut T` and `*const T` apart
//! and each pointee apart, on the tuple pattern of B-2026-11-21-30. A
//! pointee that is a type parameter, and an inherent impl, stay refused
//! (deferred.md § Generic trait impls for raw-pointer types). On the way: a
//! pointer's own methods through a borrow of it (`self` in such an impl, a
//! `ref *mut T` parameter) read the pointer through the reference.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn cell(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-ptr-impls-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    dir
}

fn modes() -> Vec<&'static str> {
    let mut m = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        m.push("__mir-llvm-run");
    }
    m
}

/// Clean under `check` (no warning either), and printing `expected` under
/// every backend.
fn runs_to(tag: &str, src: &str, expected: &str) {
    let dir = cell(tag, src);
    let (ok, out) = karac(&dir, &["check"]);
    assert!(ok, "{tag}: check: {out}");
    assert!(!out.contains("warning"), "{tag}: {out}");
    for mode in modes() {
        let (ok, out) = karac(&dir, &[mode]);
        assert!(ok, "{tag}: {mode}: {out}");
        assert_eq!(out, expected, "{tag}: {mode}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn refused(tag: &str, src: &str) -> String {
    let dir = cell(tag, src);
    let (ok, out) = karac(&dir, &["check"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok, "{tag}: accepted");
    out
}

const SHOW: &str = "trait Show {
    fn show(self) -> String;
    fn twice(self) -> String { f\"{self.show()}{self.show()}\" }
}
impl Show for *mut i64 {
    fn show(self) -> String {
        if self.is_null() {
            \"null\"
        } else {
            // Safety: every pointer shown here points at a live local.
            unsafe { f\"*{self.read()}\" }
        }
    }
}
impl Show for *const i64 {
    fn show(self) -> String {
        // Safety: every pointer shown here points at a live local.
        unsafe { f\"c{self.read()}\" }
    }
}
fn g[T: Show](x: T) -> String { x.show() }
";

#[test]
fn pointer_impl_answers_methods_defaults_ufcs_bounds_and_dyn() {
    runs_to(
        "surface",
        &format!(
            "{SHOW}fn main() {{
    let mut x: i64 = 5;
    let p: *mut i64 = ptr.mut(x);
    let c: *const i64 = ptr.const(x);
    println(p.show());
    println(c.show());
    println(p.twice());
    println(Show.show(p));
    println(g(c));
    let d: dyn Show = p;
    println(d.show());
    let n: *mut i64 = ptr.null_mut();
    println(n.show());
}}
"
        ),
        "*5\nc5\n*5*5\n*5\nc5\n*5\nnull\n",
    );
}

/// `*mut i64` and `*mut i32` are different types with their own impls.
#[test]
fn pointer_impls_are_keyed_by_their_pointee() {
    runs_to(
        "pointees",
        "trait Width { fn width(self) -> i64; }
impl Width for *mut i64 { fn width(self) -> i64 { 64 } }
impl Width for *mut i32 { fn width(self) -> i64 { 32 } }
fn g[T: Width](x: T) -> i64 { x.width() }
fn main() {
    let mut a: i64 = 1;
    let mut b: i32 = 2;
    let pa: *mut i64 = ptr.mut(a);
    let pb: *mut i32 = ptr.mut(b);
    println(pa.width());
    println(pb.width());
    println(g(pb));
}
",
        "64\n32\n32\n",
    );
    let out = refused(
        "const-has-none",
        "trait Width { fn width(self) -> i64; }
impl Width for *mut i64 { fn width(self) -> i64 { 64 } }
fn main() { let a: i64 = 1; let c: *const i64 = ptr.const(a); println(c.width()); }
",
    );
    assert!(out.contains("width"), "{out}");
    assert!(!out.contains("E0516"), "{out}");
}

/// `own self` and `mut ref self` on a pointer impl, and a pointer's own
/// methods through a `ref *mut T` parameter.
#[test]
fn pointer_impl_receiver_modes_and_borrowed_pointers() {
    runs_to(
        "modes",
        "trait Bump { fn bump(mut ref self); fn get(own self) -> i64; }
impl Bump for *mut i64 {
    fn bump(mut ref self) {
        // Safety: the pointer points at a live local.
        unsafe { self.write(self.read() + 1); }
    }
    fn get(own self) -> i64 {
        // Safety: the pointer points at a live local.
        unsafe { self.read() }
    }
}
fn peek(p: ref *mut i64) -> i64 {
    if p.is_null() { return -1; }
    // Safety: the pointer points at a live local.
    unsafe { p.read() }
}
fn main() {
    let mut x: i64 = 5;
    let mut p: *mut i64 = ptr.mut(x);
    p.bump();
    p.bump();
    println(peek(p));
    println(p.get());
}
",
        "7\n7\n",
    );
}

/// A pointer impl method's effects reach its callers.
#[test]
fn pointer_impl_effects_reach_the_caller() {
    let out = refused(
        "effects",
        "trait Say { fn say(self); }
impl Say for *mut i64 { fn say(self) { println(\"hi\") } }
pub fn quiet(p: *mut i64) { p.say(); }
fn main() { let mut x: i64 = 1; quiet(ptr.mut(x)); }
",
    );
    assert!(out.contains("E0400"), "{out}");
    assert!(out.contains("writes(Stdout)"), "{out}");
}

#[test]
fn pointer_impl_over_a_type_parameter_and_inherent_impls_are_refused() {
    let out = refused(
        "generic",
        "trait Show { fn show(self) -> String; }
impl[T] Show for *mut T { fn show(self) -> String { \"p\" } }
fn main() { }
",
    );
    assert!(
        out.contains(
            "a trait impl for a raw pointer whose pointee is a type parameter is not supported yet"
        ),
        "{out}"
    );
    assert!(out.contains("deferred.md"), "{out}");
    let out = refused(
        "inherent",
        "impl *mut i64 { fn twice(self) -> i64 { 2 } }\nfn main() { }\n",
    );
    assert!(
        out.contains("a raw pointer type has no inherent impl"),
        "{out}"
    );
}
