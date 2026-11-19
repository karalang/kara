//! A call through `dyn Trait` to an impl method that takes a parameter or
//! its receiver in a weaker mode than the trait declares (§4.7: a borrow
//! for `own`, a shared borrow or a value for `mut ref`) passes each
//! argument as the trait declares it, and the impl gets it as it takes it
//! (B-2026-11-19-126). The table's slot was the impl's body itself, so a
//! built program read an owned `String` as a borrow (`784` for a 3-byte
//! string's length), a `mut ref i64` as a value (a pointer plus 7), and
//! crashed on an owned receiver it borrowed; the interpreter stopped with
//! "deref of non-reference". Checked on the JIT, the MIR interpreter, a
//! built binary and the ASan lane.

#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn write(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-dyn-weaker-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path.file_name().unwrap())
        .current_dir(path.parent().unwrap())
        .output()
        .unwrap()
}

/// `src`'s stdout on every lane, each checked to exit cleanly.
fn every_lane(tag: &str, src: &str, want: &str) {
    let path = write(tag, src);
    for args in [
        &["run"][..],
        &["run", "--interp"][..],
        &["__mir-llvm-asan"][..],
    ] {
        let out = karac(args, &path);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{args:?}: {err}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{args:?}: {err}"
        );
    }
    let built = karac(&["build", "-o", "exe"], &path);
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let out = Command::new(path.with_file_name("exe")).output().unwrap();
    assert!(
        out.status.success(),
        "built: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "built");
}

#[test]
fn a_dyn_call_owning_an_argument_the_impl_borrows() {
    every_lane(
        "own-param",
        r#"trait Tk { fn tk(self, x: own String) -> i64; }
struct S { k: i64 }
impl Tk for S { fn tk(self, x: String) -> i64 { x.len() + self.k } }
fn main() {
    let e: dyn Tk = S { k: 1 };
    println(e.tk("abc".to_string()));
}
"#,
        "4\n",
    );
}

#[test]
fn a_dyn_call_consuming_a_receiver_and_argument_the_impl_borrows_drops_both() {
    every_lane(
        "own-self",
        r#"struct R { v: i64 }
impl Drop for R { fn drop(mut ref self) { println("drop R"); } }
trait Eat { fn eat(own self, x: own R) -> i64; }
struct S { k: i64, name: String }
impl Drop for S { fn drop(mut ref self) { println("drop S"); } }
impl Eat for S { fn eat(self, x: R) -> i64 { x.v + self.k + self.name.len() } }
fn main() {
    let e: dyn Eat = S { k: 2, name: "s".to_string() };
    let n = e.eat(R { v: 40 });
    println(n);
}
"#,
        "drop R\ndrop S\n43\n",
    );
}

#[test]
fn a_dyn_call_lending_mutably_what_the_impl_takes_by_value() {
    every_lane(
        "mut-ref",
        r#"trait Tk { fn tk(self, x: mut ref i64) -> i64; }
struct S { k: i64 }
impl Tk for S { fn tk(self, x: i64) -> i64 { x + self.k } }
fn main() {
    let e: dyn Tk = S { k: 7 };
    let mut n = 1;
    println(e.tk(mut n));
}
"#,
        "8\n",
    );
}

#[test]
fn a_dyn_vec_mixing_a_weaker_impl_and_an_exact_one() {
    every_lane(
        "mixed",
        r#"trait Tk { fn tk(mut ref self, x: own String) -> i64; }
struct S { k: i64 }
impl Tk for S { fn tk(self, x: String) -> i64 { x.len() + self.k } }
struct U { k: i64 }
impl Tk for U { fn tk(mut ref self, x: own String) -> i64 { self.k += 1; x.len() * self.k } }
fn main() {
    let mut v: Vec[dyn Tk] = Vec.new();
    v.push(S { k: 10 });
    v.push(U { k: 1 });
    let mut t = 0;
    for mut ref e in v { t += e.tk("hello".to_string()); }
    println(t);
}
"#,
        "25\n",
    );
}

#[test]
fn a_dyn_view_call_into_a_generic_weaker_impl() {
    every_lane(
        "view-generic",
        r#"trait Tk { fn tk(self, x: own String, y: mut ref i64) -> i64; }
struct W[T] { v: T, k: i64 }
impl[T] Tk for W[T] { fn tk(self, x: String, y: i64) -> i64 { x.len() + self.k + y } }
fn go(e: ref dyn Tk, n: mut ref i64) -> i64 { e.tk("ab".to_string(), mut n) }
fn main() {
    let e: dyn Tk = W { v: "s".to_string(), k: 100 };
    let mut n = 3;
    println(go(e, mut n));
    let f: dyn Tk = W { v: 1, k: 10 };
    println(f.tk("xyz".to_string(), mut n));
}
"#,
        "105\n16\n",
    );
}
