//! Kata 1079, LeetCode's Rust template for Letter Tile Possibilities:
//! `count[(c as u8 - b'A') as usize] += 1` over `tiles.chars()`. Kāra
//! refuses `char as u8` (the scalar range does not fit in 8 bits) and its
//! help names `c as u32 as u8`, Rust's meaning of the cast, but `karac fix`
//! had no edit, so the template stopped there. It now inserts the ` as u32`
//! when the operand is not itself a cast (B-2026-11-09-18).

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The first check's diagnostics, the text one `karac fix` leaves, whether
/// that text checks clean, and what it prints under the MIR interpreter.
fn one_round(name: &str, src: &str) -> (String, String, bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-1079-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let first = karac(&dir, &["check", "cell.kara"]).2;
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(dir.join("cell.kara")).unwrap();
    let (clean, _, _) = karac(&dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (first, text, clean, out)
}

#[test]
fn kata_1079_char_as_u8_is_fixed_to_go_through_u32() {
    let src = "fn main() {\n    let tiles = \"AAB\".to_string();\n    let mut count = [0; 26];\n    for c in tiles.chars() {\n        count[(c as u8 - b'A') as usize] += 1;\n    }\n    let cs: Vec[char] = tiles.chars().collect();\n    let first = cs[0] as u8;\n    let last = tiles.chars().last().unwrap() as u8;\n    let r = ref cs[2];\n    let byte = *r as u8;\n    println(f\"{count[0]} {count[1]} {first} {last} {byte}\");\n}";
    let (first, text, clean, out) = one_round("fixed", src);
    assert!(
        first.contains("cannot cast `char` to `u8` directly"),
        "{first}"
    );
    assert!(clean, "still refused after one fix:\n{text}");
    assert!(
        text.contains("count[(c as u32 as u8 - b'A') as usize]"),
        "{text}"
    );
    assert!(text.contains("cs[0] as u32 as u8"), "{text}");
    assert!(
        text.contains("tiles.chars().last().unwrap() as u32 as u8"),
        "{text}"
    );
    assert!(text.contains("*r as u32 as u8"), "{text}");
    assert_eq!(out.trim_end(), "2 1 65 66 66", "{text}");
}

#[test]
fn kata_1079_a_cast_of_a_cast_to_u8_gets_the_help_alone() {
    let src =
        "fn main() {\n    let n: u8 = 66;\n    let b = n as char as u8;\n    println(f\"{b}\");\n}";
    let (first, text, clean, _) = one_round("hint", src);
    assert!(
        first.contains("help: `c as u32 as u8` for explicit truncation"),
        "{first}"
    );
    assert!(!clean, "a cast of a cast has no edit:\n{text}");
    assert!(text.contains("n as char as u8"), "{text}");
}
