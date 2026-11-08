//! Kata 1074's Rust-template arm: `*count.entry(sum).or_insert(0) += 1` on
//! a `Map[i32, i32]`. A number literal takes the type of a closure's return
//! slot and of a map entry's value (B-2026-11-08-69). The closure used to
//! keep the literal's default `i64` (the interpreter printed `5` for an
//! `f32`, a build read garbage), and `entry_or_insert` stored an 8-byte
//! value into a 4-byte slot (ASan stack-buffer-overflow). The program is the
//! core pin `corpus/core/ok_literal_takes_closure_and_entry_slots`, run on
//! the interpreter and, with `--features llvm`, compiled.

use std::path::Path;

fn pin() -> (String, String) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus/core/ok_literal_takes_closure_and_entry_slots");
    (
        std::fs::read_to_string(dir.join("source.kara")).unwrap(),
        std::fs::read_to_string(dir.join("expected.out")).unwrap(),
    )
}

#[test]
fn kata_1074_literal_takes_closure_and_entry_slots_interp() {
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
fn kata_1074_literal_takes_closure_and_entry_slots_compiled() {
    let (src, want) = pin();
    let dir = std::env::temp_dir().join(format!("karac-kata-1074-{}", std::process::id()));
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
