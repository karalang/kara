//! B-2026-11-10-34 — Rust's `let s: String = parts.map(..).collect()`, which
//! Rust allows through `FromIterator<String> for String`, is refused in Kāra,
//! where a `String` gathers `char`s (library/iterators.md); the refusal now
//! says how to write it: collect into a `Vec[String]` and `join("")`.

use std::process::Command;

#[test]
fn collecting_text_pieces_into_a_string_names_join() {
    let dir = std::env::temp_dir().join(format!("karac-collect-pieces-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(
        &path,
        "fn main() {\n    let v = [\"a\", \"b\"].to_vec();\n    let s: String = v.iter().map(|x| f\"[{x}]\").collect();\n    println(s);\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("implements `FromIterator[char]`"), "{text}");
    assert!(
        text.contains("collect the pieces into a `Vec[String]` and join them"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
