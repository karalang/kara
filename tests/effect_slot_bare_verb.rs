//! The probe's effects pass (B-2026-11-13-32): a closure that blocks, put in
//! a slot that declares `reads(Clock) writes(Stdout)`, was refused with
//! "has effect blocks() not declared in slot": the slot diagnostics in
//! src/effectchecker/subtyping.rs printed every effect as `verb(resource)`,
//! so a verb that names no resource came out with empty parentheses. The
//! rest of the effect checker prints it bare, as the language writes it.

use std::process::{Command, Stdio};

fn check(tag: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "karac-effect-slot-bare-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

#[test]
fn a_blocking_closure_in_a_slot_names_blocks_bare() {
    let err = check(
        "field",
        "import std.time.{Duration, sleep};\n\
         struct H { f: Fn() with reads(Clock) writes(Stdout) }\n\
         fn main() {\n    let h = H { f: || { sleep(Duration.ms(1)); println(\"x\"); } };\n    (h.f)();\n}\n",
    );
    assert!(
        err.contains("has effect blocks not declared in slot"),
        "{err}"
    );
    assert!(!err.contains("blocks()"), "{err}");
}

#[test]
fn a_blocking_argument_to_a_slot_names_blocks_bare() {
    let err = check(
        "arg",
        "import std.time.{Duration, sleep};\n\
         fn run(f: Fn() with writes(Stdout)) {\n    f();\n}\n\
         pub fn main() with blocks writes(Stdout) {\n    run(|| { sleep(Duration.ms(1)); println(\"x\"); });\n}\n",
    );
    assert!(
        err.contains("has effect blocks not declared in slot"),
        "{err}"
    );
    assert!(!err.contains("blocks()"), "{err}");
}
