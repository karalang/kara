//! Kata 1071's `"A".repeat(n)` with `n: ref i64` from `.iter().map(|n| ..)`:
//! a `ref` to a number is read as the number where a library native takes
//! one (core semantics §5.10). The native used to get the address: the MIR
//! interpreter stopped with "expected a library value, found Int" and the
//! LLVM backend panicked compiling the call. The program is the core pin
//! `corpus/core/ok_ref_number_as_native_argument`, run on the interpreter
//! and, with `--features llvm`, compiled.

use std::path::Path;

fn pin() -> (String, String) {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/core/ok_ref_number_as_native_argument");
    (
        std::fs::read_to_string(dir.join("source.kara")).unwrap(),
        std::fs::read_to_string(dir.join("expected.out")).unwrap(),
    )
}

#[test]
fn kata_1071_ref_number_reaches_a_native_as_its_value_interp() {
    let (src, want) = pin();
    let run = karac::mir::lower::run_source(&src).expect("MIR");
    assert!(
        matches!(run.outcome, karac::mir::interp::Outcome::Returned(_)),
        "{:?}\n{}",
        run.outcome,
        run.output
    );
    assert_eq!(run.output, want);
}

#[cfg(feature = "llvm")]
#[test]
fn kata_1071_ref_number_reaches_a_native_as_its_value_compiled() {
    let (src, want) = pin();
    let dir = std::env::temp_dir().join(format!("karac-kata-1071-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("pin.kara");
    std::fs::write(&file, &src).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-llvm-run")
        .arg(&file)
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout, want);
}
