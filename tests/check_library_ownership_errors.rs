//! An ownership error inside a library body is dropped from what `karac
//! check` reports, and it must not stop the checks that run on MIR either
//! (B-2026-10-27-31). The serde bodies a `std.log` import reaches raise one
//! (B-2026-10-27-32), and it hid every `par` conflict of the program.

use std::process::Command;

#[test]
fn library_ownership_error_keeps_par_conflict_reported() {
    let dir = std::env::temp_dir().join(format!("karac-libown-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(
        &path,
        "import std.log.{info};\n\nfn main() {\n    info(\"start\");\n    par { println(\"a\"), println(\"b\") };\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must refuse:\n{err}");
    assert!(
        err.contains("main.kara:5:5: branches 1 and 2 of the `par` conflict"),
        "expected the par conflict in:\n{err}"
    );
}
