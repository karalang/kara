//! B-2026-11-13-15 — Rust's `dst.copy_from_slice(src)` was "no method
//! 'copy_from_slice' on type 'Slice'" with no fix, and the error type it
//! left set off a borrow error at every later use of the buffer. Kāra
//! spells the copy as a range assignment for a `Copy` element and
//! `clone_from_slice` for the rest (design.md § Range indexing); the error
//! now says so, and `karac fix` writes it.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn karac_fix_writes_copy_from_slice_as_a_range_assignment() {
    let src = "const MAGIC: Array[u8, 4] = [99, 130, 83, 99];
fn fill(dst: mut Slice[u8], src: Slice[u8]) {
    dst.copy_from_slice(src);
}
fn main() {
    let mut pkt = Vec.filled(8, 0u8);
    let xid: Vec[u8] = [1u8, 2].to_vec();
    pkt[0..2].copy_from_slice(xid);
    pkt[4..8].copy_from_slice(MAGIC);
    let mut names = [\"a\".to_string(), \"b\".to_string(), \"c\".to_string()].to_vec();
    let other = [\"x\".to_string(), \"y\".to_string()].to_vec();
    names[1..3].copy_from_slice(other);
    let mut tail = Vec.filled(2, 0u8);
    fill(mut tail, pkt[4..6]);
    pkt.push(9);
    println(f\"{pkt} {names} {tail}\");
}
";
    let path =
        std::env::temp_dir().join(format!("karac-copy-from-slice-{}.kara", std::process::id()));
    let file = path.to_str().unwrap();
    std::fs::write(&path, src).unwrap();
    let check = karac(&["check", file]);
    let text = String::from_utf8_lossy(&check.stderr).into_owned()
        + &String::from_utf8_lossy(&check.stdout);
    assert_eq!(
        text.matches("no method `copy_from_slice`").count(),
        4,
        "{text}"
    );
    // The buffer's later uses are not borrow errors.
    assert!(!text.contains("cannot be borrowed"), "{text}");
    assert!(!text.contains("cannot move"), "{text}");
    karac(&["fix", file]);
    let out = std::fs::read_to_string(&path).unwrap();
    for want in [
        "    dst.clone_from_slice(src);\n",
        "    pkt[0..2] = xid;\n",
        "    pkt[4..8] = MAGIC;\n",
        "    names[1..3].clone_from_slice(other);\n",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        (
            run.status.code(),
            String::from_utf8_lossy(&run.stdout).as_ref()
        ),
        (
            Some(0),
            "[1, 2, 0, 0, 99, 130, 83, 99, 9] [a, x, y] [99, 130]\n"
        ),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
