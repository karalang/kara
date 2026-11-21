//! B-2026-11-15-22: a tuple literal of owned places passed for a bare tuple
//! parameter whose elements are `ref` (`g((a[0], b[0]))` for
//! `p: (ref i64, ref i64)`, and a closure's `|(x, y): (ref i64, ref i64)|`)
//! passed check, then the callee read each value as a reference: the
//! interpreter stopped with "deref of non-reference Int(1)" and the
//! compiled program crashed. The literal is now built at the parameter's
//! element types, each place lent, and then lent whole. Each program runs
//! on the interpreter, compiled and under ASan.

mod common;

use std::process::{Command, Stdio};

fn run(sub: &str, tag: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-tuple-ref-elems-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([sub, "cell.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn lanes(tag: &str, src: &str, want: &str) {
    let (ok, out, err) = run("__mir-run", tag, src);
    assert!(ok, "{tag} interpreted: {err}");
    assert_eq!(out, want, "{tag} interpreted: {err}");
    if !cfg!(feature = "llvm") {
        return;
    }
    for sub in ["__mir-llvm-run", "__mir-llvm-asan"] {
        let (ok, out, err) = run(sub, tag, src);
        if err.contains("Linker failed") && common::link_or_skip(Err(err.to_string())).is_none() {
            eprintln!("skipping {sub}: no runtime archive");
            continue;
        }
        assert!(ok, "{tag} {sub}: {err}");
        assert_eq!(out, want, "{tag} {sub}: {err}");
    }
}

#[test]
fn tuple_of_places_lent_to_a_ref_tuple_parameter() {
    lanes(
        "fn",
        r#"fn g(p: (ref i64, ref i64)) -> i64 { let (x, y) = p; x + y }
fn h(p: (ref String, i64)) -> i64 { p.0.len() + p.1 }
fn main() {
    let a = [1, 2];
    let b = [10, 20];
    println(g((a[0], b[0])));
    println(g((a[1], b[1])));
    let s = "hello".to_string();
    println(h((s, 2)));
}
"#,
        "11\n22\n7\n",
    );
}

#[test]
fn tuple_of_places_lent_to_a_closure_ref_tuple_parameter() {
    lanes(
        "closure",
        r#"fn main() {
    let mut a = [1, 2];
    let b = [10, 20];
    let f = |(x, y): (ref i64, ref i64)| x + y;
    println(f((a[0], b[0])));
    let g = |p: (ref i64, ref i64)| p.0 * p.1;
    println(g((a[1], b[1])));
    a[0] = 7;
    println(f((a[0], a[1])));
}
"#,
        "11\n40\n9\n",
    );
}
