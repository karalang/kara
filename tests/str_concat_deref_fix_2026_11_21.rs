//! B-2026-11-21-16: `s + *t`, with `t` a `ref Str` (an item lent by
//! `.iter()` over a `Vec[Str]`), was refused as "'+' on 'String' requires a
//! 'String' right operand, found 'Str'" with no fix, where `s + t` and
//! `s + ref t` carry the machine-applicable `.to_string()`. It now takes the
//! same edit: `s + t.to_string()`.

use std::process::Command;

#[test]
fn string_plus_a_dereferenced_str_is_fixed_to_to_string() {
    let dir = std::env::temp_dir().join(format!("karac-concat-deref-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(
        &file,
        "fn main() {\n    let cs: Vec[Str] = [\"a\", \"b\"].to_vec();\n    let mut c = \"\";\n    for x in cs.iter() { c = c + \"<\" + *x + \">\"; }\n    println(c);\n}\n",
    )
    .unwrap();
    let karac = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    };
    let first = karac(&["check", "main.kara"]);
    assert!(
        first.contains("so `s + *t` is `s + t.to_string()`"),
        "{first}"
    );
    karac(&["fix", "main.kara"]);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("c = c + \"<\" + x.to_string() + \">\";"),
        "{text}"
    );
    assert!(
        karac(&["check", "main.kara"]).contains("All checks passed"),
        "{text}"
    );
    assert_eq!(karac(&["__mir-run", "main.kara"]), "<a><b>\n");
    let _ = std::fs::remove_dir_all(&dir);
}
