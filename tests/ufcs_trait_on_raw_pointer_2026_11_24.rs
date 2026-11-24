//! B-2026-11-24-21 (design.md §9 Raw pointers): `Show.is_null(p)` names the
//! trait, so it calls the trait's method even where the raw pointer has its
//! own `is_null`; the pointer's own methods come first only for
//! `p.is_null()`. The checker typed the UFCS call as the pointer's `bool` and
//! lowering called the pointer's native, so both backends printed `false`.

use std::path::PathBuf;
use std::process::Command;

fn file(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-ufcs-ptr-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

fn karac(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Checks clean and prints `want` on the interpreter, and compiled with the
/// backend.
fn runs(tag: &str, src: &str, want: &str) {
    let f = file(tag, src);
    let f = f.to_str().unwrap();
    let (ok, out, err) = karac(&["check", f]);
    assert!(ok, "check refused:\n{out}{err}");
    let (ok, out, err) = karac(&["__mir-run", f]);
    assert!(ok, "interp failed:\n{out}{err}");
    assert_eq!(out, want, "interp\n{err}");
    #[cfg(feature = "llvm")]
    {
        let (ok, out, err) = karac(&["__mir-llvm-run", f]);
        assert!(ok, "mir-llvm failed:\n{out}{err}");
        assert_eq!(out, want, "mir-llvm\n{err}");
    }
}

const POINTER: &str = "trait Show {\n\
    \x20   fn is_null(self) -> String;\n\
    \x20   fn tag(self) -> String;\n\
    }\n\
    impl Show for *mut i64 {\n\
    \x20   fn is_null(self) -> String { \"trait\".to_string() }\n\
    \x20   fn tag(self) -> String { \"tag\".to_string() }\n\
    }\n\
    fn main() {\n\
    \x20   let mut x: i64 = 1;\n\
    \x20   let p: *mut i64 = ptr.mut(x);\n\
    \x20   println(p.is_null());\n\
    \x20   let s: String = Show.is_null(p);\n\
    \x20   println(s);\n\
    \x20   println(Show.tag(p));\n\
    \x20   println(p.tag());\n\
    }\n";

#[test]
fn trait_ufcs_on_a_raw_pointer_calls_the_traits_method() {
    runs("pointer", POINTER, "false\ntrait\ntag\ntag\n");
}

#[test]
fn trait_ufcs_on_a_struct_with_an_inherent_namesake_calls_the_traits_method() {
    runs(
        "struct",
        "trait T { fn f(self) -> i64; }\n\
         struct S {}\n\
         impl S { fn f(self) -> i64 { 1 } }\n\
         impl T for S { fn f(self) -> i64 { 2 } }\n\
         fn main() { let s = S {}; println(s.f()); println(T.f(s)); }\n",
        "1\n2\n",
    );
}
