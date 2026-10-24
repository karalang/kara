//! A child's output pipe reads (`ChildStdout` / `ChildStderr` `read`,
//! `read_to_end`, `read_to_string`) suspend their task while they wait
//! (`docs/library/io.md` § Processes), so they are `blocks suspends`, and a
//! public caller that declares nothing is asked for both.

use std::process::Command;

#[test]
fn undeclared_pipe_read_asks_for_blocks_and_suspends() {
    let dir = std::env::temp_dir().join(format!("karac-pipe-reads-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(
        &path,
        "pub fn slurp(out: ref ChildStdout) -> i64 {
             let mut s = String.new();
             match out.read_to_string(s) {
                 Ok(n) => n,
                 Err(_) => -1,
             }
         }
         fn main() {}",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must refuse: {err}");
    assert!(
        err.contains("Add: with blocks suspends"),
        "expected `Add: with blocks suspends` in: {err}"
    );
}
