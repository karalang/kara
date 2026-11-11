//! B-2026-11-11-50 — a method on a const (`NAME.as_bytes()`, `CODES.map(..)`)
//! parses as a path call, and the method call the typechecker made of it
//! gave the receiver and the `)` the whole call's span. `karac fix` then
//! found no place for `.bytes()` (`NAME.as_bytes()` stayed refused) and put
//! `.iter()` after the call (`CODES.map(..).iter().sum()`). The receiver is
//! now the first segment and the `)` the call's last byte, in the
//! typechecker and in lowering's rewrite alike, so a const's integer
//! intrinsics still find their width.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn karac_fix_places_its_edit_on_a_const_receiver() {
    let src = "const NAME: Str = \"kara\";
const CODES: Array[i64, 3] = [1, 2, 3];
const MASK: u8 = 200;
fn main() {
    let a = NAME.as_bytes().len();
    let s: i64 = CODES.map(|x| x * 2).sum();
    let b = NAME
        .as_bytes()
        .len();
    println(f\"{a} {s} {b} {MASK.count_ones()} {MASK.trailing_zeros()}\");
}
";
    let path = std::env::temp_dir().join(format!(
        "karac-path-call-receiver-{}.kara",
        std::process::id()
    ));
    let file = path.to_str().unwrap();
    std::fs::write(&path, src).unwrap();
    let check = karac(&["check", file]);
    let text = String::from_utf8_lossy(&check.stderr).into_owned()
        + &String::from_utf8_lossy(&check.stdout);
    assert!(
        text.contains("a string's UTF-8 bytes are `.bytes()`"),
        "{text}"
    );
    karac(&["fix", file]);
    let out = std::fs::read_to_string(&path).unwrap();
    for want in [
        "let a = NAME.bytes().len();",
        "CODES.iter().map(|x| x * 2).sum()",
        "        .bytes()\n",
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
        (Some(0), "4 12 4 3 3\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
