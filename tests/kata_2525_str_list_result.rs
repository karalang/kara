//! Kata 2525's table arm: a `-> Str` function returning an element of
//! `let names = ["Neither", ..]`. The literals took `String`, and the
//! `.as_str()` fix offered for it borrowed the function's own list, which
//! `karac fix` applied and check then refused (E0509). The fix is the
//! binding's `: Vec[Str]`; a list written in place gets the hint alone
//! (B-2026-11-23-68).

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-k2525-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

fn karac(args: &[&str], f: &PathBuf) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(f)
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const TABLE: &str = "fn name(i: i64) -> Str {
    let names = [\"Neither\", \"Heavy\", \"Bulky\", \"Both\"];
    names[i]
}

fn main() {
    println(name(2));
}
";

const IN_PLACE: &str = "fn name(heavy: bool) -> Str {
    [(\"Heavy\", true), (\"Neither\", false)].iter().filter(|(_, h)| h == heavy).map(|(n, _)| n).next().unwrap()
}

fn main() {
    println(name(true));
}
";

#[test]
fn a_result_read_out_of_a_literal_list_offers_the_bindings_str() {
    let f = program("table", TABLE);
    let (ok, out) = karac(&["check"], &f);
    assert!(!ok, "check accepted the `String` element: {out}");
    assert!(out.contains("declare it `names: Vec[Str]`"), "{out}");
    assert!(
        !out.contains("as_str"),
        "the `.as_str()` fix is still offered: {out}"
    );
    assert!(
        !out.contains("E0515"),
        "the follow-on move error is still reported: {out}"
    );
    let (ok, out) = karac(&["fix"], &f);
    assert!(ok, "fix: {out}");
    let (ok, out) = karac(&["check"], &f);
    assert!(ok, "check after fix: {out}");
    let (ok, out) = karac(&["run", "--interp"], &f);
    assert!(ok, "run: {out}");
    assert_eq!(out, "Bulky\n");
}

#[test]
fn a_result_read_out_of_a_list_in_place_offers_no_as_str() {
    let f = program("in_place", IN_PLACE);
    let (ok, out) = karac(&["check"], &f);
    assert!(!ok, "check accepted the `ref String` element: {out}");
    assert!(
        out.contains("Bind the list with its element type first"),
        "{out}"
    );
    let (_, out) = karac(&["fix", "--dry-run"], &f);
    assert!(out.contains("no fixable"), "a fix is still offered: {out}");
}
