//! A `mut ref` to a function type is invariant in its effect set (design.md
//! §8 Subtyping and variance; review-thread ruling 2026-11-24,
//! B-2026-11-24-16). Through the reference a callee may store a new function,
//! so a `with ()` place handed to a `mut ref Fn() with writes(Stdout)` slot
//! let a later call through the `with ()` value print. By value and through
//! `ref` the effect set stays covariant.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], src: &str, tag: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-mut-ref-fn-inv-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

const SET: &str = "fn pure_one() -> i64 { 1 }\n\
fn loud() -> i64 { println(\"LOUD\"); 2 }\n\
fn set(r: mut ref Fn() -> i64 with writes(Stdout), g: escaping Fn() -> i64 with writes(Stdout)) { *r = g; }\n";

#[test]
fn mut_ref_fn_invariance_with_unit_place_into_wider_slot_is_refused() {
    let src = format!(
        "{SET}struct Holder {{ f: Fn() -> i64 with () }}\n\
         fn main() {{\n    let mut h = Holder {{ f: pure_one }};\n    set(mut h.f, loud);\n    println((h.f)());\n}}\n"
    );
    let out = karac(&["check"], &src, "narrow");
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("E0404"), "{err}");
    assert!(err.contains("invariant in its effect set"), "{err}");
    assert!(err.contains("`with ()`"), "{err}");
}

#[test]
fn mut_ref_fn_invariance_clause_place_into_no_clause_slot_is_refused() {
    let src = "fn pure_one() -> i64 { 1 }\n\
fn swap_in(r: mut ref Fn() -> i64, g: escaping Fn() -> i64) { *r = g; }\n\
fn main() {\n    let mut f: Fn() -> i64 with () = pure_one;\n    swap_in(mut f, pure_one);\n    println(f());\n}\n";
    let out = karac(&["check"], src, "noclause");
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("no clause (every effect)"), "{err}");
}

#[test]
fn mut_ref_fn_invariance_same_clause_and_ref_covariance_run() {
    let src = format!(
        "{SET}fn peek(r: ref Fn() -> i64 with writes(Stdout)) -> i64 {{ r() }}\n\
         struct Holder {{ f: Fn() -> i64 with writes(Stdout) }}\n\
         struct Pure {{ f: Fn() -> i64 with () }}\n\
         fn main() {{\n    let mut h = Holder {{ f: pure_one }};\n    set(mut h.f, loud);\n    \
         let p = Pure {{ f: pure_one }};\n    println(f\"{{(h.f)()}} {{peek(p.f)}}\");\n}}\n"
    );
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, &src, "ok");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "LOUD\n2 1\n",
            "{lane:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
