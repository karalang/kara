//! Library-side costs the kata re-bench of 2026-11-02 found on the MIR
//! backend (B-2026-11-02-4, -6, -11): an f-string of text and integer
//! pieces is built in one exact allocation without the runtime's format
//! buffer, and a signed `abs` checks its overflow with the negation's own
//! flag. The compiled program prints what the interpreter prints.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

fn ir(src: &str) -> String {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR")
}

/// The compiled program's exit code, standard output and standard error.
fn compiled(src: &str, tag: &str) -> Option<(i32, String, String)> {
    let ir = ir(src);
    let dir = std::env::temp_dir().join(format!("karac-mir-fills-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    let _ = std::fs::remove_dir_all(&dir);
    Some((
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    ))
}

/// The body of the function `name` in `ir`.
fn body<'a>(ir: &'a str, name: &str) -> &'a str {
    let at = ir
        .match_indices(&format!("@kara.{name}("))
        .map(|(i, _)| i)
        .find(|&i| {
            ir[..i]
                .rsplit('\n')
                .next()
                .is_some_and(|l| l.starts_with("define"))
        })
        .unwrap_or_else(|| panic!("no definition of {name}"));
    let end = ir[at..].find("\n}\n").map_or(ir.len(), |e| at + e);
    &ir[at..end]
}

const MIXED: &str = r#"fn piece(c: i64) -> String {
    f"{c},"
}
fn main() {
    let a: i64 = -42;
    let b: u8 = 255;
    let c: i32 = 0;
    let m: i64 = 0 - 9223372036854775807 - 1;
    let u: u64 = 18446744073709551615;
    let s = "xy";
    let t = String.from("zz");
    println(f"{a},{b};{c}|{m}|{u}");
    println(f"[{s}{a}{t}{c}]");
    println(f"{c}");
    let mut out = String.new();
    let mut i = 0;
    while i < 12 {
        out.push_str(piece(i * 7 - 30));
        i = i + 1;
    }
    println(out);
}
"#;

const MIXED_WANT: &str = "-42,255;0|-9223372036854775808|18446744073709551615\n[xy-42zz0]\n0\n-30,-23,-16,-9,-2,5,12,19,26,33,40,47,\n";

#[test]
fn mir_format_direct_writes_integer_pieces_in_place() {
    let interp = karac::mir::lower::run_source(MIXED).expect("MIR interpreter");
    assert_eq!(interp.output, MIXED_WANT);
    let piece = body(&ir(MIXED), "piece").to_string();
    assert!(
        !piece.contains("karac_mir_fmt_begin"),
        "f\"{{c}},\" went through the format buffer:\n{piece}"
    );
    if let Some((code, out, err)) = compiled(MIXED, "mixed") {
        assert_eq!(code, 0, "{err}");
        assert_eq!(out, MIXED_WANT);
    }
}

const ABS: &str = r#"fn main() {
    let xs: Vec[i64] = [5, -3, 0, -9223372036854775807, 9223372036854775807];
    let mut i = 0;
    while i < xs.len() {
        println(xs[i].abs());
        i = i + 1;
    }
    let a: i32 = -7;
    let b: i8 = -128;
    println(a.abs());
    println(b.abs());
}
"#;

#[test]
fn mir_abs_overflows_only_at_the_minimum() {
    let want = "5\n3\n0\n9223372036854775807\n9223372036854775807\n7\n";
    let interp = karac::mir::lower::run_source(ABS).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    assert!(
        matches!(interp.outcome, karac::mir::interp::Outcome::Aborted(_)),
        "{:?}",
        interp.outcome
    );
    if let Some((code, out, err)) = compiled(ABS, "abs") {
        assert_eq!(out, want);
        assert_ne!(code, 0);
        assert!(err.contains("integer overflow"), "{err}");
    }
}
