//! The standard library's impls cohere: two of its impls of one trait whose
//! headers unify would leave the second unregistered, so its methods would
//! be missing from every program with no cause named. `karac check` reports
//! such an overlap even in library text (B-2026-11-06-6); this checks the
//! whole library, unpruned, so one cannot land.

use std::process::Command;

#[test]
fn the_whole_library_checks_with_no_conflicting_impl() {
    let dir = std::env::temp_dir().join(format!("karac-library-coherence-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), "fn main() {\n    println(1);\n}\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "main.kara"])
        .env("KARAC_MIR_PRUNE_LIBRARY", "0")
        .current_dir(&dir)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(!text.contains("conflicting impl"), "{text}");
}
