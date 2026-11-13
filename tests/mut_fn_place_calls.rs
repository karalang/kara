//! B-2026-11-13-3 (core-semantics §9.6): a `MutFn` is called only through a
//! unique access, and that holds for one stored in a struct field or a
//! collection as for a local. `(self.f)()` under a bare `self`, `(h.f)()` on
//! a bare parameter, and `(h.f)()` / `(fs[0])()` on a plain `let` were
//! accepted, and the closure's state changed through each shared path;
//! `let mut h` drew `unused_mut`.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-mut-fn-place-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

/// `check` refuses with `expect`; `karac fix`, run to a fixed point, makes
/// the program pass and writes `edit`.
fn refused_then_fixed(tag: &str, src: &str, expect: &str, edit: &str) {
    let (dir, path) = fixture(tag, src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{tag}: check must refuse: {err}");
    assert!(err.contains(expect), "{tag}: expected `{expect}` in: {err}");
    for _ in 0..4 {
        karac().arg("fix").arg(&path).output().unwrap();
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{tag}: check must pass after `karac fix`; source now:\n{fixed}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fixed.contains(edit),
        "{tag}: expected `{edit}` in:\n{fixed}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn accepted_quietly(tag: &str, src: &str) {
    let (dir, path) = fixture(tag, src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{tag}: check must accept: {err}");
    assert!(!err.contains("unused_mut"), "{tag}: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

const HOLDER: &str = "struct Holder { f: MutFn() -> i64 }\n";

#[test]
fn mut_fn_field_through_a_borrowed_self_needs_mut_ref_self() {
    refused_then_fixed(
        "self",
        &format!(
            "{HOLDER}impl Holder {{ fn call(self) -> i64 {{ (self.f)() }} }}\n\
             fn main() {{\n    let mut n = 0;\n    \
             let mut h = Holder {{ f: || {{ n += 1; n }} }};\n    \
             println(h.call());\n}}\n"
        ),
        "Declare the method `mut ref self`",
        "fn call(mut ref self)",
    );
}

#[test]
fn mut_fn_field_through_a_bare_parameter_needs_mut_ref() {
    refused_then_fixed(
        "param",
        &format!(
            "{HOLDER}fn via(h: Holder) -> i64 {{ (h.f)() }}\n\
             fn main() {{\n    let mut n = 0;\n    \
             let mut h = Holder {{ f: || {{ n += 1; n }} }};\n    \
             println(via(mut h));\n}}\n"
        ),
        "a bare parameter",
        "h: mut ref Holder",
    );
}

#[test]
fn mut_fn_field_on_a_plain_let_needs_let_mut() {
    refused_then_fixed(
        "let-field",
        &format!(
            "{HOLDER}fn main() {{\n    let mut n = 0;\n    \
             let h = Holder {{ f: || {{ n += 1; n }} }};\n    \
             println((h.f)());\n    println((h.f)());\n}}\n"
        ),
        "declared without `mut`",
        "let mut h",
    );
}

#[test]
fn mut_fn_element_on_a_plain_let_needs_let_mut() {
    refused_then_fixed(
        "let-element",
        "fn main() {\n    let mut n = 0;\n    \
         let fs: Vec[MutFn() -> i64] = [|| { n += 1; n }];\n    \
         println((fs[0])());\n}\n",
        "declared without `mut`",
        "let mut fs",
    );
}

#[test]
fn mut_fn_field_call_uses_the_lets_mut() {
    accepted_quietly(
        "uses-mut",
        &format!(
            "{HOLDER}fn main() {{\n    let mut n = 0;\n    \
             let mut h = Holder {{ f: || {{ n += 1; n }} }};\n    \
             println((h.f)());\n    println((h.f)());\n}}\n"
        ),
    );
    accepted_quietly(
        "element-uses-mut",
        "fn main() {\n    let mut n = 0;\n    \
         let mut fs: Vec[MutFn() -> i64] = [|| { n += 1; n }];\n    \
         println((fs[0])());\n}\n",
    );
}

#[test]
fn unique_access_and_plain_fn_fields_stay_callable() {
    accepted_quietly(
        "mut-ref-self",
        &format!(
            "{HOLDER}impl Holder {{ fn call(mut ref self) -> i64 {{ (self.f)() }} }}\n\
             fn main() {{\n    let mut n = 0;\n    \
             let mut h = Holder {{ f: || {{ n += 1; n }} }};\n    \
             println(h.call());\n}}\n"
        ),
    );
    accepted_quietly(
        "fn-field",
        "struct Holder { f: Fn() -> i64 }\n\
         impl Holder { fn call(self) -> i64 { (self.f)() } }\n\
         fn via(h: Holder) -> i64 { (h.f)() }\n\
         fn main() {\n    let h = Holder { f: || 7 };\n    \
         println(h.call());\n    println(via(h));\n    println((h.f)());\n}\n",
    );
}
