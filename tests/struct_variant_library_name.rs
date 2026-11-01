//! A struct variant of a program's own enum reads its field types from the
//! enum even when a library struct has the same name (B-2026-11-01-6).
//! `Stmt.Match { cases }` used to take `cases`'s type from the library's
//! `Match` struct, which has no such field: `karac check` passed and the MIR
//! builder refused the arm ("a type that failed to check is not lowered
//! yet"), and an `i64` field bound as a borrow, so `*n` was accepted on that
//! arm alone.

mod common;

use std::process::Command;

/// `src`'s standard output and whether it exited cleanly, on the MIR
/// interpreter (`karac __mir-run`).
fn interp_stdout(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-variant-lib-name-{}-{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The same, compiled at `opt`; `None` without the llvm feature or a
/// runtime archive.
#[cfg(feature = "llvm")]
fn compiled_stdout(src: &str, opt: u8) -> Option<(bool, String)> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "karac-variant-lib-name-b-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, opt).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    let out = linked.map(|()| Command::new(&exe).output().expect("run"));
    let _ = std::fs::remove_dir_all(&dir);
    let out = out?;
    Some((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

#[cfg(not(feature = "llvm"))]
fn compiled_stdout(_: &str, _: u8) -> Option<(bool, String)> {
    None
}

const PROG: &str = r#"enum Stmt {
    Match { cases: Vec[i64], n: i64 },
    Response { body: Vec[String] },
    Client { ids: Vec[i64] },
    Regex { parts: Vec[i64] },
    Stats { xs: Vec[i64] },
    Other,
}

fn visit(s: ref Stmt) -> i64 {
    match s {
        Stmt.Match { cases, n } => cases.len() + n,
        Stmt.Response { body } => body.len(),
        Stmt.Client { ids } => ids.len(),
        Stmt.Regex { parts } => parts.len(),
        Stmt.Stats { xs } => xs.len(),
        Stmt.Other => 0,
    }
}

fn main() {
    let all = [
        Stmt.Match { cases: [1, 2], n: 10 },
        Stmt.Response { body: ["a".to_string()] },
        Stmt.Client { ids: [1, 2, 3] },
        Stmt.Regex { parts: [] },
        Stmt.Stats { xs: [4, 5, 6, 7] },
        Stmt.Other,
    ];
    for s in all.iter() {
        println(f"{visit(s)}");
    }
}
"#;

const WANT: &str = "12\n1\n3\n0\n4\n0\n";

#[test]
fn struct_variant_named_like_a_library_type_uses_its_own_fields() {
    let diags = karac::check_source(PROG);
    assert!(
        diags.is_empty(),
        "{:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let (ok, out) = interp_stdout("run", PROG);
    assert!(ok, "{out}");
    assert_eq!(out, WANT, "interpreter");
    for opt in [0, 2] {
        if let Some((ok, out)) = compiled_stdout(PROG, opt) {
            assert!(ok, "at -O{opt}: {out}");
            assert_eq!(out, WANT, "at -O{opt}");
        }
    }
}

/// A copied field of a borrowed scrutinee binds by value on a variant named
/// like a library type exactly as on any other variant, so `*n` is refused on
/// both arms.
#[test]
fn struct_variant_named_like_a_library_type_binds_copied_fields_by_value() {
    let src = "enum Stmt {\n    Match { n: i64 },\n    Other { n: i64 },\n}\n\nfn get(s: ref Stmt) -> i64 {\n    match s {\n        Stmt.Match { n } => *n,\n        Stmt.Other { n } => *n,\n    }\n}\n\nfn main() {\n    println(f\"{get(Stmt.Match { n: 3 })}\");\n}\n";
    let errors: Vec<(usize, String)> = karac::check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| (d.line, d.message))
        .collect();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert_eq!(errors[0].0, 8, "{errors:?}");
    assert_eq!(errors[1].0, 9, "{errors:?}");
    for (_, m) in &errors {
        assert!(m.contains("so drop the `*`"), "{m}");
    }
}
