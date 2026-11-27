//! A local closure that an escaping closure names escapes with it, so it
//! captures by move too (core semantics §9.3, B-2026-11-27-22).
//! `fn mk(pre: own String) -> Fn(String) -> String { let c = || pre.clone();
//! |s| f"{c()}{s}" }` was E0509 ("the result borrows `pre`, which this
//! function owns"): the returned closure moved `c` in, but `c` itself still
//! borrowed `pre`. An owned parameter, an owned local and a `Copy` local
//! now move into the inner closure, in a free function and a method alike;
//! a borrowed parameter's reference is still a view the result holds.

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

fn file(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-local-closure-moves-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    (dir, f)
}

/// `karac check` on `src`: whether it passed, and its output.
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

#[test]
fn an_owned_capture_through_a_local_closure_moves_in() {
    // An `own` parameter, through one local closure and through a chain.
    runs_everywhere(
        "fn mk(pre: own String) -> Fn(String) -> String { let c = || pre.clone(); |s| f\"{c()}{s}\" }\n\
         fn mk2(pre: own String) -> Fn() -> String { let c = || pre.clone(); let d = || c(); || d() }\n\
         fn main() { let f = mk(\"<\".to_string()); println(f(\"a\")); println(f(\"b\")); \
         let g = mk2(\">\".to_string()); println(g()); }\n",
        "<a\n<b\n>\n",
    );
    // A `Copy` local and an owned local.
    runs_everywhere(
        "fn n() -> Fn() -> i64 { let n = 1; let c = || n + 1; || c() }\n\
         fn s() -> Fn() -> String { let s = \"x\".to_string(); let c = || s.clone(); || c() }\n\
         fn main() { let f = n(); println(f()); let g = s(); println(g()); }\n",
        "2\nx\n",
    );
}

#[test]
fn a_method_returning_a_closure_through_a_local_closure_moves_in() {
    runs_everywhere(
        "struct K { n: i64 }\n\
         impl K {\n\
         fn own_pre(self, pre: own String) -> Fn(String) -> String { let c = || pre.clone(); |s| f\"{c()}{s}\" }\n\
         fn copied(self) -> Fn(String) -> String { let n = self.n; let c = || n + 1; |s| f\"{c()}{s}\" }\n\
         }\n\
         fn main() { let k = K { n: 1 }; let f = k.own_pre(\"<\".to_string()); println(f(\"a\")); \
         let g = k.copied(); println(g(\"a\")); }\n",
        "<a\n2a\n",
    );
}

#[test]
fn a_borrowed_parameter_through_a_local_closure_is_still_a_view() {
    // A free function's result may borrow its argument.
    runs_everywhere(
        "fn mk(pre: String) -> Fn(String) -> String { let c = || pre.clone(); |s| f\"{c()}{s}\" }\n\
         fn main() { let p = \"<\".to_string(); let f = mk(p); println(f(\"a\")); println(p); }\n",
        "<a\n<\n",
    );
    // A method's result borrows only what `self` does (B-2026-11-26-89).
    for body in [
        "let c = || pre.clone(); |s| f\"{c()}{s}\"",
        "let c = || pre.clone(); let d = || c(); |s| f\"{d()}{s}\"",
    ] {
        let (ok, out) = check(&format!(
            "struct K {{ n: i64 }}\n\
             impl K {{ fn mk(self, pre: String) -> Fn(String) -> String {{ {body} }} }}\n\
             fn main() {{ let k = K {{ n: 1 }}; let f = k.mk(\"<\".to_string()); println(f(\"a\")); }}\n"
        ));
        assert!(!ok && out.contains("E0515"), "{body}\n{out}");
    }
}

#[test]
fn a_local_closure_no_escaping_closure_names_still_borrows() {
    // Called in the body, the closure borrows, so the local is usable after.
    runs_everywhere(
        "fn f() -> Fn() -> i64 { let s = \"xy\".to_string(); let c = || s.len(); let k = c(); \
         println(s); || k + 1 }\n\
         fn main() { let g = f(); println(g()); }\n",
        "xy\n3\n",
    );
}
