//! `!x` on an integer: `karac check`'s JSON replacement is the `~` that
//! `karac fix` writes, not `not`, and the message names both spellings
//! (B-2026-10-29-29). A `!` on a bool keeps `not`.

use std::process::Command;

#[test]
fn check_json_replaces_an_integer_bang_with_the_complement() {
    let dir = std::env::temp_dir().join(format!("karac-bang-json-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src =
        "fn main() {\n    let x: i64 = 5;\n    let b = true;\n    println(f\"{!x} {!b}\");\n}\n";
    std::fs::write(dir.join("n.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["check", "--output=json", "n.kara"])
        .output()
        .unwrap();
    let json = String::from_utf8_lossy(&out.stdout);
    let int_bang = src.find("!x").unwrap();
    let bool_bang = src.find("!b").unwrap();
    for (offset, text) in [(int_bang, "~"), (bool_bang, "not ")] {
        let want =
            format!("\"replacement\":{{\"offset\":{offset},\"length\":1,\"text\":\"{text}\"}}");
        assert!(json.contains(&want), "missing {want} in:\n{json}");
    }
    assert!(
        json.contains("use `not` for a bool, `~` for an integer's bitwise complement"),
        "{json}"
    );
    let fixed = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["fix", "n.kara"])
        .output()
        .unwrap();
    assert!(fixed.status.success(), "{fixed:?}");
    let after = std::fs::read_to_string(dir.join("n.kara")).unwrap();
    assert!(after.contains("{~x} {not b}"), "{after}");
    let _ = std::fs::remove_dir_all(&dir);
}
