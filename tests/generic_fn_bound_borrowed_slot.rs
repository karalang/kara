//! A closure handed to a generic function's bare `f: F` parameter, with `F`
//! bounded by a function trait, gets its signature from the bound. The slot
//! of a bare parameter is `ref F` (core-semantics §4.1: it borrows), and the
//! bound was looked up only for a plain `F`, so the closure was checked
//! against nothing. A body that fixed its parameter (`|x| x + 1`) worked; one
//! that only passed it through (`|x| x`, `|x| { println("a"); x }`) passed
//! `karac check` and stopped every backend at MIR validation with `ap[Fn(())
//! -> ()]` (B-2026-11-09-39). A named function handed to such a parameter
//! now solves the parameters only the bound mentions, as the bound's own
//! `U` in `F: Fn(i64) -> U`.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-fn-bound-borrowed-slot-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

const PROGRAM: &str = r#"fn ap[F: Fn(i64) -> i64](x: i64, f: F) -> i64 {
    f(x)
}

fn ap_where[F](f: F, x: i64) -> i64 where F: Fn(i64) -> i64 {
    f(x)
}

fn app[F: Fn(i64) -> U, U](f: F, x: i64) -> U {
    f(x)
}

fn double(n: i64) -> i64 {
    n * 2
}

fn main() {
    println(ap(2, |x| x + 1));
    println(ap(1, |x| { println("a"); x }));
    println(ap(3, |x| x));
    println(ap(4, |x| { let y = x; y }));
    println(ap_where(|x| x, 5));
    println(app(|x| x, 6));
    println(app(|x| f"<{x}>", 7));
    println(app(double, 8));
}
"#;

const EXPECTED: &str = "3\na\n1\n3\n4\n5\n6\n<7>\n16\n";

/// The default executor (the JIT on an llvm build, the interpreter
/// otherwise) and the interpreter.
const LANES: [&[&str]; 2] = [&["run"], &["run", "--interp"]];

#[test]
fn a_closure_for_a_bounded_borrowed_fn_parameter_takes_the_bound_signature() {
    let path = temp("run", PROGRAM);
    for lane in LANES {
        let out = karac(lane, &path);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            EXPECTED,
            "{lane:?}; stderr:\n{stderr}"
        );
        assert!(out.status.success(), "{lane:?}; stderr:\n{stderr}");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_unannotated_closure_held_in_a_binding_is_still_refused() {
    // The binding's closure has no context to type its parameter from, and
    // handing it on afterwards gives it none: it stays refused, rather than
    // being solved from the bound into an instance that does not validate.
    let path = temp(
        "binding",
        "fn twice[F: MutFn(String) -> String](f: mut ref F, s: String) -> String {
    let a = f(s);
    f(a)
}

fn main() {
    let mut g = |s| s;
    println(twice(mut g, \"hi\".to_string()));
}
",
    );
    let out = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains("cannot infer closure parameter type"),
        "{stderr}"
    );
}
