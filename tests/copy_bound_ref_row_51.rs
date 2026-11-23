//! A shared reference is `Copy` (core-semantics §1.1: "`ref T`, but not
//! `mut ref T`"), and the type checker's own `Copy` test did not say so: a
//! `T: Copy` bound refused `ref String`, a tuple or `Option` holding one,
//! and `#[derive(Copy)]` refused a `ref` field, while the ownership pass and
//! the MIR already copied all of them (B-2026-11-22-51).

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Every call gets a directory of its own, whatever names two tests pass
/// (B-2026-11-23-13).
static CALL: AtomicUsize = AtomicUsize::new(0);

fn karac(name: &str, src: &str, cmd: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-copy-bound-ref-{name}-{cmd}-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// `karac check` accepts `src`, and the interpreter and, built with the
/// backend, the compiled program and its ASan build print `want`.
fn copies_everywhere(name: &str, src: &str, want: &str) {
    let check = karac(name, src, "check");
    assert!(
        check.status.success(),
        "{name}: check: {}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let cmds: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run", "__mir-llvm-asan"]
    } else {
        &["__mir-run"]
    };
    for cmd in cmds {
        let out = karac(name, src, cmd);
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        // Only a failed link may skip: a refusal, a runtime error or a
        // sanitizer report is the program's own.
        if *cmd != "__mir-run" {
            let linked = common::link_or_skip(if out.status.success() {
                Ok(())
            } else {
                Err(err.clone())
            });
            if linked.is_none() {
                continue;
            }
        }
        assert!(out.status.success(), "{name}: {cmd}: {err}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}: {cmd}");
    }
}

/// `karac check` refuses `src` with `code`, and its message holds `needle`.
fn refused_with(name: &str, src: &str, code: &str, needle: &str) {
    let out = karac(name, src, "check");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{name}: accepted");
    assert!(text.contains(&format!("error[{code}]")), "{name}: {text}");
    assert!(text.contains(needle), "{name}: {text}");
}

const MAIN: &str = "fn main() {\n    let a = \"a\".to_string();\n    let b = \"b\".to_string();\n    let mut v: Vec[ref String] = Vec.new();\n    v.push(a);\n    v.push(b);\n    swap2(mut v);\n    println(f\"{v[0]} {v[1]}\");\n}\n";

/// The row's program: `T` is `ref String`, which the bound accepts.
#[test]
fn a_copy_bound_takes_a_shared_reference_as_the_generic_swap_does() {
    let src = format!(
        "fn swap2[T: Copy](xs: mut Slice[T]) {{\n    let t = xs[0];\n    xs[0] = xs[1];\n    xs[1] = t;\n}}\n{MAIN}"
    );
    copies_everywhere("generic", &src, "b a\n");
}

/// The non-generic form always checked; the two now agree.
#[test]
fn the_swap_written_over_ref_string_prints_the_same() {
    let src = format!(
        "fn swap2(xs: mut Slice[ref String]) {{\n    let t = xs[0];\n    xs[0] = xs[1];\n    xs[1] = t;\n}}\n{MAIN}"
    );
    copies_everywhere("plain", &src, "b a\n");
}

/// A reference binding, and a tuple and an `Option` holding one, satisfy
/// the bound: each is `Copy` when its parts are.
#[test]
fn a_copy_bound_takes_a_reference_binding_a_tuple_and_an_option_of_one() {
    copies_everywhere(
        "parts",
        "fn both[T: Copy](x: T) -> (T, T) {\n    (x, x)\n}\nfn main() {\n    let s = \"a\".to_string();\n    let r = ref s;\n    let (a, b) = both(r);\n    println(f\"{a}{b}\");\n    let t: (ref String, i64) = (s, 2);\n    let (u, w) = both(t);\n    println(f\"{u.0}{w.1}\");\n    let o: Option[ref String] = Some(s);\n    let (p, q) = both(o);\n    println(f\"{p.is_some()} {q.unwrap()}\");\n}\n",
        "aa\na2\ntrue a\n",
    );
}

/// `#[derive(Copy)]` checks field by field, and a `ref` field is `Copy`:
/// `let q = p` copies the struct, and both read the same `String`.
#[test]
fn derive_copy_takes_a_struct_with_a_shared_reference_field() {
    copies_everywhere(
        "derive",
        "#[derive(Copy, Clone)]\nstruct P {\n    r: ref String,\n    n: i64,\n}\nfn main() {\n    let s = \"a\".to_string();\n    let p = P { r: s, n: 1 };\n    let q = p;\n    println(f\"{p.n} {p.r} {q.r}\");\n}\n",
        "1 a a\n",
    );
}

/// `mut ref T` is unique, so it is not `Copy`: neither the bound nor the
/// derive takes it.
#[test]
fn a_copy_bound_and_derive_copy_still_refuse_a_mut_ref() {
    refused_with(
        "bound",
        "fn both[T: Copy](x: T) -> (T, T) {\n    (x, x)\n}\nfn main() {\n    let mut s = \"a\".to_string();\n    let r = mut ref s;\n    let (a, b) = both(mut r);\n    a.push('x');\n    b.push('y');\n    println(s);\n}\n",
        "E0200",
        "`mut ref String` does not implement `Copy`",
    );
    refused_with(
        "field",
        "#[derive(Copy, Clone)]\nstruct P {\n    r: mut ref String,\n}\nfn main() {\n    let mut s = \"a\".to_string();\n    let p = P { r: mut ref s };\n    p.r.push('b');\n    println(s);\n}\n",
        "E0200",
        "field 'r' has non-Copy type 'mut ref String'",
    );
}

/// A tuple with an owned `String` in it is still not `Copy`.
#[test]
fn a_copy_bound_still_refuses_a_tuple_holding_an_owned_string() {
    refused_with(
        "owned",
        "fn both[T: Copy](x: T) -> (T, T) {\n    (x, x)\n}\nfn main() {\n    let s = \"a\".to_string();\n    let t: (i64, String) = (1, s);\n    let u = both(t);\n    println(u.0.0);\n}\n",
        "E0200",
        "`(i64, String)` does not implement `Copy`",
    );
}
