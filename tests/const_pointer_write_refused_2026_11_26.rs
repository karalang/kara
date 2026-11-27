//! The `*const T` write refusal (B-2026-11-26-95): a `*const T` has no
//! write capability (design.md § Raw pointers), so `*p = v`, a compound
//! assignment, a field write through it and the `write*` methods are refused,
//! while the same writes through a `*mut T` still check. Before the fix every
//! one was accepted and wrote, even into an immutable `let`.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac_const_ptr_write_{}_{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
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

fn refused(name: &str, body: &str) {
    let src = format!(
        "struct P {{\n    x: i64,\n}}\nfn main() {{\n    let x: i64 = 3;\n    let s = P {{ x: 1 }};\n    {body}\n    println(f\"{{x}} {{s.x}}\");\n}}\n"
    );
    let (ok, out) = check(name, &src);
    assert!(!ok, "{name} was accepted: {out}");
    assert!(out.contains("E_PTR_WRITE_THROUGH_CONST"), "{name}: {out}");
}

#[test]
fn assigning_through_a_const_pointer_is_refused() {
    refused(
        "assign",
        "let p = ptr.const(x);\n    // Safety: test\n    unsafe { *p = 4; }",
    );
}

#[test]
fn compound_assigning_through_a_const_pointer_is_refused() {
    refused(
        "compound",
        "let p = ptr.const(x);\n    // Safety: test\n    unsafe { *p += 4; }",
    );
}

#[test]
fn a_field_write_through_a_const_pointer_is_refused() {
    refused(
        "field",
        "let p = ptr.const(s);\n    // Safety: test\n    unsafe { (*p).x = 4; }",
    );
}

#[test]
fn the_write_methods_on_a_const_pointer_are_refused() {
    for m in ["write", "write_unaligned", "write_volatile"] {
        refused(
            m,
            &format!("let p = ptr.const(x);\n    // Safety: test\n    unsafe {{ p.{m}(4); }}"),
        );
    }
}

#[test]
fn the_same_writes_through_a_mut_pointer_still_check() {
    let (ok, out) = check(
        "mut_ok",
        "struct P {\n    x: i64,\n}\nfn main() {\n    let mut x: i64 = 3;\n    let mut s = P { x: 1 };\n    let p = ptr.mut(x);\n    let q = ptr.mut(s);\n    // Safety: test\n    unsafe {\n        *p = 4;\n        *p += 1;\n        (*q).x = 2;\n        p.write(6);\n        p.write_unaligned(7);\n        p.write_volatile(8);\n    }\n    println(f\"{x} {s.x}\");\n}\n",
    );
    assert!(ok, "{out}");
    assert!(!out.contains("E_PTR_WRITE_THROUGH_CONST"), "{out}");
}
