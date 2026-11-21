//! B-2026-11-21-24: printing a `Secret[T]` with `println(tok)` or `f"{tok}"`
//! was refused with the generic "does not implement Display", where `dbg`,
//! `==` and a hand-written impl say that `Secret` withholds the trait on
//! purpose and point at docs/library/secret.md. Every Display refusal of a
//! `Secret` now carries the same reason, and an ordinary type's does not.

use std::process::Command;

fn check(tag: &str, body: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("karac-secret-display-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), body).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "main.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success(), "{tag} must be refused");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn secret_display_refusals_name_secret_md() {
    let head = "import std.secret.{Secret};\nfn main() {\n    let tok: Secret[String] = Secret.new(\"hunter2\".to_string());\n";
    for (tag, line) in [
        ("println", "    println(tok);\n}\n"),
        ("fstring", "    println(f\"token = {tok}\");\n}\n"),
    ] {
        let out = check(tag, &format!("{head}{line}"));
        assert!(
            out.contains("does not implement Display") || out.contains("must implement Display"),
            "{tag}: {out}"
        );
        assert!(
            out.contains("withholds `Display` on purpose")
                && out.contains("docs/library/secret.md")
                && out.contains(".expose()"),
            "{tag}: {out}"
        );
        assert!(
            !out.contains("hunter2\""),
            "{tag}: the refusal must not quote the value"
        );
    }
    let plain = check(
        "plain",
        "struct P { a: i64 }\nfn main() { let p = P { a: 1 }; println(f\"{p}\"); }\n",
    );
    assert!(plain.contains("does not implement Display"), "{plain}");
    assert!(!plain.contains("secret.md"), "{plain}");
}
