//! `a + b` on `String`s whose left side is a temporary appends in place
//! (`mir::concat`): the compiled program prints what the interpreter
//! prints, with or without the pass, and allocates less with it.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"fn g(e: ref String, i: i64) -> String {
    let piece = f"{i}";
    if i % 2 == 0 {
        return e.clone() + "+" + piece.clone();
    }
    e.clone() + "-" + piece
}
fn main() {
    let mut acc = String.from("x");
    let mut i = 0;
    while i < 50 {
        acc = g(acc, i);
        i = i + 1;
    }
    let a = String.from("keep");
    let b = a + "!";
    println(f"{a} {b} {acc.len()}");
    println(acc.substring(0, 12));
}
"#;

const WANT: &str = "keep keep! 141\nx+0-1+2-3+4-\n";

fn compiled(src: &str) -> Option<String> {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::codegen::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-mir-concat-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::codegen::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    common::link_or_skip(karac::codegen::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).unwrap())
}

#[test]
fn mir_concat_appends_to_temporaries_on_both_backends() {
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, WANT);
    if let Some(out) = compiled(SRC) {
        assert_eq!(out, WANT);
    }
}
