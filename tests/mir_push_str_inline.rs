//! `String.push_str` appends in line while the bytes fit, and grows
//! through the runtime's reservation only when they do not (kata 763's
//! letter-at-a-time build, B-2026-11-09-9). A borrowed literal has no
//! capacity, so even an empty append copies it out before writing; a
//! long append grows past doubling; multi-byte text is copied whole.

#[cfg(feature = "llvm")]
mod common;

const SRC: &str = r#"
fn main() {
    let mut out = String.new();
    for round in 0..2 {
        let mut s = "lit";
        s.push_str("");
        s.push_str("eral");
        out.push_str(f"{s}{round} ");
    }
    let letters = "abcdefghijklmnopqrstuvwxyz";
    let mut t = String.new();
    for i in 0..100 {
        t.push_str(letters[i % 26..i % 26 + 1]);
    }
    let mut w = "ab";
    w.push_str(t);
    let mut u = String.new();
    u.push_str("");
    u.push_str("é→");
    println(f"{out}{t.len()} {t[24..30]} {w.len()} {w[0..5]} {u} {u.len()}");
}
"#;

#[test]
fn mir_push_str_appends_in_line() {
    let r = karac::mir::lower::run_source(SRC).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        r.output, "literal0 literal1 100 yzabcd 102 ababc é→ 5\n",
        "stderr: {}",
        r.stderr
    );
}

#[cfg(feature = "llvm")]
#[test]
fn mir_llvm_push_str_appends_in_line() {
    use std::process::Command;
    let lowered = karac::mir::lower::build_source(SRC).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-push-str-{}", std::process::id()));
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
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "literal0 literal1 100 yzabcd 102 ababc é→ 5\n"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
