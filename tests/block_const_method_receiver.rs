//! A block's `const` (design.md § Constants) with a method called on it,
//! `DIRS.iter()` or `DIRS.len()`, was "undefined name": the call reads as
//! a path whose first segment is the constant, and the pass that hoists a
//! block constant renamed only a bare name. A module-scope constant
//! worked; the block's now does too (kata 1631's Rust template).

use std::process::Command;

#[test]
fn a_block_const_takes_a_method_call() {
    let dir = std::env::temp_dir().join(format!("karac-blockconst-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    let src = "fn main() {\n    const DIRS: Array[(i64, i64), 2] = [(1, 0), (0, 1)];\n    println(DIRS.len());\n    for (a, b) in DIRS.iter() {\n        println(a + 10 * b);\n    }\n}\n";
    std::fs::write(&f, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["run", "--interp", f.to_str().unwrap()])
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2\n1\n10\n");
}
