//! `ptr.mut(place)` / `ptr.const(place)` of an element, field or tuple
//! part whose declared type is a reference point at the slot that holds the
//! reference: `ptr.mut(xs[1])` on a `mut Slice[ref i32]` is a
//! `*mut ref i32`, as the same line is at `T = ref i32` in a generic
//! function. It was typed `*mut i32`, a pointer to what a shared reference
//! refers to, so the concrete spelling of the library heap's sift did not
//! check (B-2026-11-22-50).

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Every call gets a directory of its own, whatever names two tests pass
/// (B-2026-11-23-13).
static CALL: AtomicUsize = AtomicUsize::new(0);

fn karac(name: &str, src: &str, cmd: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-ptr-to-ref-slot-{name}-{cmd}-{}-{}",
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
fn slot_pointer_runs_everywhere(name: &str, src: &str, want: &str) {
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

/// `karac check`'s output for `src`, which it refuses.
fn slot_pointer_refused(name: &str, src: &str) -> String {
    let out = karac(name, src, "check");
    assert!(!out.status.success(), "{name}: accepted");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

const MAIN: &str = "fn main() {\n    let a = 1i32;\n    let b = 2i32;\n    let mut v: Vec[ref i32] = Vec.new();\n    v.push(a);\n    v.push(b);\n    show(mut v);\n    println(f\"{v[0]} {v[1]}\");\n}\n";

/// The row's function, written out for `ref i32`: the pointers are to the
/// slots, and writing through one stores a reference.
#[test]
fn a_pointer_to_a_reference_element_is_a_pointer_to_the_slot() {
    let src = format!(
        "fn show(xs: mut Slice[ref i32]) {{\n    let p: *mut ref i32 = ptr.mut(xs[1]);\n    let q: *const ref i32 = ptr.const(xs[0]);\n    // Safety: both slots are in bounds and hold references that outlive the call.\n    unsafe {{\n        let t = *p;\n        *p = *q;\n        println(t);\n    }}\n}}\n{MAIN}"
    );
    slot_pointer_runs_everywhere("concrete", &src, "2\n1 1\n");
}

/// The generic spelling always checked; the two print the same.
#[test]
fn the_generic_spelling_of_the_slot_pointer_prints_the_same() {
    let src = format!(
        "fn store[T: Copy](xs: mut Slice[T]) {{\n    let p: *mut T = ptr.mut(xs[1]);\n    let q: *const T = ptr.const(xs[0]);\n    // Safety: both slots are in bounds.\n    unsafe {{\n        *p = *q;\n    }}\n}}\nfn show(xs: mut Slice[ref i32]) {{\n    println(xs[1]);\n    store(mut xs);\n}}\n{MAIN}"
    );
    slot_pointer_runs_everywhere("generic", &src, "2\n1 1\n");
}

/// The pointee is the slot's declared type: a `*mut i32` is refused for a
/// reference element, field and tuple part, and a binding that is itself a
/// reference still gives a pointer to what it refers to.
#[test]
fn the_pointee_of_a_place_is_its_declared_type() {
    let shown = slot_pointer_refused(
        "types",
        "struct H {\n    r: ref i32,\n    n: i32,\n}\nfn show(xs: mut Slice[ref i32], ys: mut ref Vec[String], h: mut ref H, t: mut ref (ref i32, i32)) {\n    let a: *mut bool = ptr.mut(xs[1]);\n    let b: *mut bool = ptr.mut(ys[0]);\n    let c: *mut bool = ptr.mut(h.r);\n    let d: *mut bool = ptr.mut(h.n);\n    let e: *mut bool = ptr.mut(t.0);\n    let f: *const bool = ptr.const(ys);\n}\nfn main() {}\n",
    );
    for (line, found) in [
        (6, "*mut ref i32"),
        (7, "*mut String"),
        (8, "*mut ref i32"),
        (9, "*mut i32"),
        (10, "*mut ref i32"),
        (11, "*const Vec[String]"),
    ] {
        let want = format!("main.kara:{line}:");
        assert!(
            shown
                .lines()
                .any(|l| l.contains(&want) && l.ends_with(&format!("found '{found}'"))),
            "line {line} wants {found}:\n{shown}"
        );
    }
}
