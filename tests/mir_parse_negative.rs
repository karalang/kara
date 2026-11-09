//! `T.parse(s)` reads a `-` and up to 18 digits inline for a signed `T`
//! (kata 682's negative scores each called the runtime, B-2026-11-09-7). The
//! verdicts must not change: a lone `-`, two signs, a `+` and whitespace
//! (still the runtime's), the 19-digit `i64.MIN` and one past it, and an
//! `i8`'s `-128` and `-129`; an unsigned type still refuses `-0`.

#[cfg(feature = "llvm")]
mod common;

const SRC: &str = r#"
fn show(s: String) -> String {
    match i64.parse(s).ok() {
        Some(n) => f"{n}",
        None => "-",
    }
}
fn show8(s: String) -> String {
    match i8.parse(s).ok() {
        Some(n) => f"{n}",
        None => "-",
    }
}
fn showu(s: String) -> String {
    match u8.parse(s).ok() {
        Some(n) => f"{n}",
        None => "-",
    }
}
fn main() {
    let xs = ["0", "-0", "-7", "42", "-", "--1", "-+1", "+5", " -3", "-123456789012345678", "-1234567890123456789", "-9223372036854775808", "-9223372036854775809", "-1a"];
    let mut out = "";
    for x in xs {
        out = out + show(x.to_string()) + " ";
    }
    println(out);
    let ys = ["-128", "-129", "127", "128", "-0", "-1"];
    let mut o8 = "";
    for y in ys {
        o8 = o8 + show8(y.to_string()) + "/" + showu(y.to_string()) + " ";
    }
    println(o8);
}
"#;

const EXPECTED: &str = "0 0 -7 42 - - - 5 -3 -123456789012345678 -1234567890123456789 -9223372036854775808 - - \n-128/- -/- 127/127 -/128 0/- -1/- \n";

#[test]
fn mir_parse_reads_a_minus_sign() {
    let r = karac::mir::lower::run_source(SRC).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(r.output, EXPECTED, "stderr: {}", r.stderr);
}

#[cfg(feature = "llvm")]
#[test]
fn mir_llvm_parse_reads_a_minus_sign() {
    use std::process::Command;
    let lowered = karac::mir::lower::build_source(SRC).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-parse-neg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    if linked.is_some() {
        let out = Command::new(&exe).output().expect("run");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), EXPECTED);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
