//! Comparing a library lookup with an owned `Option` (`m.get(k) == Some(5)`)
//! is refused, as the library side is an `Option[ref T]` (B-2026-10-12-22).
//! For a `Copy` item the error carries the `.cloned()` that copies the item
//! out, so `karac fix` makes it (B-2026-11-01-10): on either side of `==`
//! and `!=`, after a `Map.get` and a `Vec.get`, and on an ordering. A
//! non-`Copy` item keeps the message with no edit.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-option-ref-compare-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn karac_fix_clones_the_viewed_side_of_an_option_compare() {
    let src = "fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1, 5);
    let v: Vec[i64] = vec![5, 6];
    let o: Option[i64] = Some(5);
    println(f\"{m.get(1) == Some(5)} {Some(6) != m.get(1)} {v.get(1) == o.map(|x| x + 1)} {m.get(2) == None}\");
    println(f\"{m.get(1) < o}\");
}
";
    let path = temp("fix", src);
    let file = path.to_str().unwrap();

    let check = karac(&["check", "--output=json", file]);
    let json = String::from_utf8_lossy(&check.stdout);
    assert_eq!(json.matches("\"text\":\".cloned()\"").count(), 4, "{json}");
    assert!(json.contains("`.cloned()` copies the item out"), "{json}");

    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let out = std::fs::read_to_string(&path).unwrap();
    for want in [
        "m.get(1).cloned() == Some(5)",
        "Some(6) != m.get(1).cloned()",
        "v.get(1).cloned() == o.map(",
        "m.get(2) == None",
        "m.get(1).cloned() < o",
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
        (Some(0), "true true true true\nfalse\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn a_non_copy_item_compare_keeps_the_message_without_an_edit() {
    let src = "fn main() {
    let ms: Map[i64, String] = Map.new();
    let s = \"x\".to_string();
    println(f\"{ms.get(1) == Some(s)}\");
}
";
    let path = temp("non-copy", src);
    let file = path.to_str().unwrap();
    let check = karac(&["check", "--output=json", file]);
    let _ = std::fs::remove_file(&path);
    let json = String::from_utf8_lossy(&check.stdout);
    assert!(
        json.contains("cannot compare 'Option[ref String]' and 'Option[String]'"),
        "{json}"
    );
    assert!(!json.contains(".cloned()"), "{json}");
}
