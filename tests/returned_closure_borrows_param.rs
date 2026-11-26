//! A returned closure that captures a borrowed parameter captures the
//! reference the parameter holds, so it is a view result borrowing the
//! argument (core semantics §5.7, §9.3, review ruling 2026-11-26,
//! B-2026-11-26-74). `fn mk(p: String) -> Fn(String) -> String { |s|
//! f"{p}{s}" }` was E0515 ("an escaping closure captures `p` by move"),
//! though the same capture through an inner closure was accepted. The
//! result may not outlive the argument, and every other escape of the view
//! is still refused.

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

const PRELUDE: &str = "fn mk(p: String) -> Fn(String) -> String { |s| f\"{p}{s}\" }\n";

fn file(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-returned-closure-view-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, format!("{PRELUDE}{src}")).unwrap();
    (dir, f)
}

/// `karac check` on the prelude and `src`: whether it passed, and its output.
fn check(src: &str) -> (bool, String) {
    let (dir, f) = file(src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&f)
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

/// The program's stdout under `cmd`, after `karac check` accepted it;
/// `None` when a compiled backend cannot link.
fn run(src: &str, cmd: &str) -> Option<String> {
    let (ok, out) = check(src);
    assert!(ok, "check:\n{out}\n{src}");
    let (dir, f) = file(src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if cmd != "__mir-run" {
        common::link_or_skip(if out.status.success() {
            Ok(())
        } else {
            Err(err.clone())
        })?;
    }
    assert!(out.status.success(), "{cmd}: {err}\n{src}");
    Some(String::from_utf8(out.stdout).unwrap())
}

fn runs_everywhere(src: &str, want: &str) {
    let mut lanes = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        lanes.extend(["__mir-llvm-run", "__mir-llvm-asan"]);
    }
    for cmd in lanes {
        if let Some(out) = run(src, cmd) {
            assert_eq!(out, want, "{cmd}\n{src}");
        }
    }
}

/// `src` is refused with `code`, its message containing `words`.
fn refused(src: &str, code: &str, words: &str) {
    let (ok, out) = check(src);
    assert!(!ok, "{src}\n{out}");
    assert!(out.contains(code) && out.contains(words), "{src}\n{out}");
}

#[test]
fn a_returned_closure_over_a_borrowed_parameter_runs() {
    runs_everywhere(
        "fn main() { let name = \"<\".to_string(); let f = mk(name); \
         println(f(\"a\")); println(f(\"b\")); println(name); }\n",
        "<a\n<b\n<\n",
    );
    // `return` of one, and the tail, from one function.
    runs_everywhere(
        "fn mk2(p: String) -> Fn(String) -> String { if p.len() > 5 { return |s| f\"{p}!{s}\"; } \
         |s| f\"{p}{s}\" }\n\
         fn main() { let a = \"<\".to_string(); let f = mk2(a); println(f(\"a\")); \
         let b = \"<<<<<<\".to_string(); let g = mk2(b); println(g(\"b\")); }\n",
        "<a\n<<<<<<!b\n",
    );
    // A `mut ref` parameter, and a closure bound first.
    runs_everywhere(
        "fn len_of(p: mut ref Vec[i64]) -> Fn() -> i64 { || p.len() }\n\
         fn first(p: Vec[i64]) -> Fn() -> i64 { let c = || p[0]; c }\n\
         fn main() { let mut v = [1, 2].to_vec(); let f = len_of(mut v); println(f()); \
         let w = [1, 2].to_vec(); let g = first(w); println(g()); }\n",
        "2\n1\n",
    );
}

#[test]
fn the_result_may_not_outlive_the_argument() {
    refused(
        "fn get() -> Fn(String) -> String { let name = \"<\".to_string(); mk(name) }\n\
         fn main() { let f = get(); println(f(\"a\")); }\n",
        "E0509",
        "the result borrows `name`",
    );
    refused(
        "fn main() { let f = mk(\"<\".to_string()); println(f(\"a\")); }\n",
        "E0516",
        "cannot drop the temporary value",
    );
    refused(
        "fn main() { let name = \"<\".to_string(); let f = mk(name); let n2 = name; \
         println(f(\"a\")); println(n2); }\n",
        "E0516",
        "cannot move `name` while `f` still holds a borrow of it",
    );
    refused(
        "fn len_of(p: mut ref Vec[i64]) -> Fn() -> i64 { || p.len() }\n\
         fn main() { let mut v = [1, 2].to_vec(); let f = len_of(mut v); v.push(3); println(f()); }\n",
        "E0516",
        "cannot mutably borrow `v` while `f` still holds a borrow of it",
    );
}

#[test]
fn every_other_escape_of_the_view_is_refused() {
    let stored = "a view of `name` is stored in an escaping closure";
    refused(
        "fn main() { let name = \"<\".to_string(); let mut v: Vec[Fn(String) -> String] = Vec.new(); \
         v.push(mk(name)); println(v.len()); }\n",
        "E0509",
        stored,
    );
    refused(
        "struct H { f: Fn(String) -> String }\n\
         fn main() { let name = \"<\".to_string(); let h = H { f: mk(name) }; println((h.f)(\"a\")); }\n",
        "E0509",
        stored,
    );
    refused(
        "fn keep(g: escaping Fn(String) -> String) { let mut v: Vec[Fn(String) -> String] = Vec.new(); \
         v.push(g); println(v.len()); }\n\
         fn main() { let name = \"<\".to_string(); let f = mk(name); keep(f); }\n",
        "E0509",
        stored,
    );
    // Built in a struct the function returns: stored in a field, not the
    // result itself.
    refused(
        "struct G { f: Fn(String) -> String }\n\
         fn mk_g(p: String) -> G { G { f: |s| f\"{p}{s}\" } }\n\
         fn main() { let name = \"<\".to_string(); let g = mk_g(name); println((g.f)(\"a\")); }\n",
        "E0515",
        "an escaping closure captures `p` by move",
    );
}

#[test]
fn an_owned_parameter_moves_in_as_before() {
    runs_everywhere(
        "fn own_mk(p: own String) -> Fn(String) -> String { |s| f\"{p}{s}\" }\n\
         fn main() { let f = own_mk(\"<\".to_string()); println(f(\"a\")); }\n",
        "<a\n",
    );
}
