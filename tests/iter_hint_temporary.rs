//! B-2026-11-11-46 — an iterator method on a `Vec` that a call returns
//! (`pkt.split(|b| b == 0).collect()`, `s.to_vec().map(..)`) was fixed
//! with `.iter()`, which lends from the temporary the statement then drops
//! (E0516), so the fixed program was refused again. A call's result is
//! walked with `.into_iter()`, which hands its items over; a place and a
//! borrowed `Slice` still get `.iter()`.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn karac_fix_walks_a_returned_collection_with_into_iter() {
    let src = "fn main() {
    let pkt: Vec[u8] = [1u8, 0, 2, 3, 0].to_vec();
    let parts: Vec[Slice[u8]] = pkt[..].split(|b| b == 0).collect();
    let s: i64 = 258u16.to_be_bytes().to_vec().map(|x| x as i64).sum();
    let v = [3, 1, 2].to_vec();
    let w: Vec[i64] = v.map(|x| x * 2).collect();
    println(f\"{parts.len()} {parts[1].len()} {s} {w[0]}\");
}
";
    let path = std::env::temp_dir().join(format!(
        "karac-iter-hint-temporary-{}.kara",
        std::process::id()
    ));
    let file = path.to_str().unwrap();
    std::fs::write(&path, src).unwrap();
    let check = karac(&["check", file]);
    let text = String::from_utf8_lossy(&check.stderr).into_owned()
        + &String::from_utf8_lossy(&check.stdout);
    assert!(
        text.contains("write `pkt[..].split(|b| b == 0).into_iter().collect(...)`"),
        "{text}"
    );
    for _ in 0..2 {
        karac(&["fix", file]);
    }
    let out = std::fs::read_to_string(&path).unwrap();
    for want in [
        "split(|b| b == 0).into_iter().collect()",
        "to_vec().into_iter().map(",
        "v.iter().map(",
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
        (Some(0), "3 2 3 6\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
