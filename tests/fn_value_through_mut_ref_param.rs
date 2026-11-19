//! B-2026-11-19-71: a function value passed to a `mut ref F` (or `ref F`)
//! parameter of a generic function, and a named function's value passed to
//! a concrete `mut ref Fn() -> i64` one, is called through. The instance
//! called the reference itself as a function value: compiled builds
//! overflowed the stack or segfaulted, ASan reported a stack-buffer-overflow,
//! and the interpreter refused "call through Agg([Ref(..)]), which is not a
//! function value". The instance is specialised to the closure's or the
//! function item's own type, as a `mut ref MutFn()` parameter already was
//! for a closure (B-2026-10-19-68).

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-fn-value-mut-ref-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

fn runs(name: &str, src: &str, want: &str) {
    let path = scratch(name, src);
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{}", text(&run));
    if cfg!(feature = "llvm") {
        let exe = path.with_extension("bin");
        let build = karac(&["build", "-o", exe.to_str().unwrap()], &path);
        assert!(build.status.success(), "{}", text(&build));
        let ran = Command::new(&exe).output().unwrap();
        assert!(ran.status.success(), "{}", text(&ran));
        assert_eq!(String::from_utf8_lossy(&ran.stdout), want, "{}", text(&ran));
    }
}

#[test]
fn a_generic_mut_ref_f_calls_the_closure_and_the_function() {
    runs(
        "generic.kara",
        "fn five() -> i64 { 5 }
fn twice[F: MutFn() -> i64](f: mut ref F) -> i64 { f() + f() }
fn twice_ref[F: Fn() -> i64](f: ref F) -> i64 { f() + f() }
fn main() {
    let mut n = 0;
    let mut g = || { n = n + 1; n };
    println(twice(mut g));
    println(n);
    let k = || 7;
    println(twice_ref(k));
    let mut h = five;
    println(twice(mut h));
}
",
        "3\n2\n14\n10\n",
    );
}

#[test]
fn a_concrete_mut_ref_fn_calls_a_named_function() {
    runs(
        "named.kara",
        "fn five() -> i64 { 5 }
fn call(f: mut ref Fn() -> i64) -> i64 { f() + f() }
fn call_mut(f: mut ref MutFn() -> i64) -> i64 { f() + 1 }
fn main() {
    let mut h = five;
    println(call(mut h));
    let mut h2 = five;
    println(call_mut(mut h2));
}
",
        "10\n6\n",
    );
}
