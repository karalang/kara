//! A private enum's variants written as Rust allows them, `AA` and `BB`
//! (kata 2745), are Const class in Kāra (design.md §3 CN-1, CN-4), and the
//! refusal named `Aa` with no fix, leaving the rename at every use to the
//! author (B-2026-11-20-142). A private enum's variant now gets the rename as
//! one fix over every use of the name in the file, as a private type's
//! name does; a `const` of the same name withholds it.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-variant-rename-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

const PIECES: &str = r#"enum Piece {
    AA,
    BB,
    Start,
}

fn after(last: Piece) -> i64 {
    match last {
        Piece.AA => 1,
        Piece.BB => 2,
        Piece.Start => 3,
    }
}

fn main() {
    let order = [Piece.Start, Piece.AA, Piece.BB];
    let mut total = 0;
    for p in order.iter() {
        total = total * 10 + after(p);
    }
    println(total);
}
"#;

#[test]
fn a_private_enums_const_class_variants_are_renamed_at_every_use() {
    let path = temp("pieces", PIECES);
    let first = karac(&["check"], &path);
    let stderr = String::from_utf8_lossy(&first.stderr).into_owned();
    assert!(
        stderr.contains(
            "`AA` is Const-class but enum variant names must be Type-class (PascalCase); consider renaming to `Aa`"
        ),
        "{stderr}"
    );
    let fixed = karac(&["fix"], &path);
    assert!(
        fixed.status.success(),
        "{}",
        String::from_utf8_lossy(&fixed.stderr)
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("AA") && !text.contains("BB"), "{text}");
    assert!(
        text.contains("    Aa,\n    Bb,\n") && text.contains("Piece.Bb => 2,"),
        "{text}"
    );
    let out = karac(&["run", "--interp"], &path);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "312\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_variant_rename_a_const_of_the_same_name_would_take_is_withheld() {
    let src = "const AA: i64 = 7;\n\nenum Piece {\n    AA,\n    Start,\n}\n\nfn main() {\n    \
               let p = Piece.AA;\n    match p {\n        Piece.AA => println(AA),\n        \
               Piece.Start => println(0),\n    }\n}\n";
    let path = temp("const", src);
    karac(&["fix"], &path);
    let text = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(text, src);
}

#[test]
fn a_public_enums_variants_are_not_renamed() {
    let src = "pub enum Piece {\n    AA,\n    Start,\n}\n\nfn main() {\n    let p = Piece.AA;\n    \
               match p {\n        Piece.AA => println(1),\n        Piece.Start => println(0),\n    }\n}\n";
    let path = temp("pub", src);
    karac(&["fix"], &path);
    let text = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(text, src);
}
