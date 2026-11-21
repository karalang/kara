//! B-2026-11-21-80: a method a raw pointer does not have (`p.expose_addr()`,
//! `p.addr()`, `p.clone()`, any typo) poisoned to `Type::Error` with no
//! diagnostic, so `karac check` printed only MIR's root-less E0516 "a type
//! that failed to check is not lowered yet". It is now E0236, naming the
//! pointer's methods and, for an address-shaped name, the `ptr` functions.

use std::process::Command;

fn check(tag: &str, body: &str) -> (bool, String) {
    let dir =
        std::env::temp_dir().join(format!("karac-ptr-no-method-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), body).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "main.kara"])
        .current_dir(&dir)
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

fn program(line: &str) -> String {
    format!(
        "fn main() {{\n    let mut x: i64 = 5;\n    let p: *mut i64 = ptr.mut(x);\n{line}\n}}\n"
    )
}

#[test]
fn unknown_raw_pointer_method_is_e0236_not_e0516() {
    for (tag, line, extra) in [
        ("expose_addr", "    let a = p.expose_addr();", "ptr.addr(p)"),
        ("addr", "    let a = p.addr();", "ptr.expose_mut(p)"),
        ("clone", "    let q = p.clone();", "`Copy`"),
        (
            "typo_into_string",
            "    let s: String = p.bogus();",
            "is_null",
        ),
    ] {
        let (ok, out) = check(tag, &program(line));
        assert!(!ok, "{tag} must be refused: {out}");
        assert!(out.contains("E0236"), "{tag}: {out}");
        assert!(out.contains("on type '*mut i64'"), "{tag}: {out}");
        assert!(out.contains(extra), "{tag}: {out}");
        assert!(
            !out.contains("E0516"),
            "{tag}: a root error, not E0516: {out}"
        );
    }
    let (ok, out) = check(
        "known",
        &program("    if p.is_null() { println(\"null\"); }\n    unsafe { println(p.read()); }"),
    );
    assert!(ok, "the pointer's own methods still check: {out}");
}
