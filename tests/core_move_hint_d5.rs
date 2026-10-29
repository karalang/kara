//! Under D5 a bare parameter already borrows, so the E0500 hint for a value
//! moved into an `own` parameter says to drop the `own`, not to "declare the
//! callee parameter `ref`" (which is what a bare parameter already is).

use std::process::Command;

#[test]
fn a_use_after_a_move_into_an_own_parameter_suggests_dropping_own() {
    let src = "fn archive(log: own Vec[String]) -> i64 { log.len() as i64 }
fn main() {
    let log = [\"a\".to_string(), \"b\".to_string()].to_vec();
    let n = archive(log);
    println(n);
    println(f\"{log[1]}\");
}
";
    let dir = std::env::temp_dir().join(format!("karac-movehint-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must refuse:\n{err}");
    assert!(
        err.contains("drop `own` from the callee parameter if it only reads"),
        "{err}"
    );
    assert!(!err.contains("parameter `ref`"), "{err}");
}
