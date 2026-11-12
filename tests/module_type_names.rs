//! B-2026-11-04-24 — a package's check flattens its modules into one source,
//! renaming a declaration two modules share (`a.P` becomes `P__a`). Its
//! diagnostics named those internal names (`expected 'ref P__a', found
//! 'P__b'`) and quoted the rewritten text. They now name the module path,
//! quote the file's own line with the carets moved to match, and leave out
//! the `ref` a bare parameter's slot carries. Columns, in the header and
//! the JSON, are the file's too.

use std::process::Command;

#[test]
fn a_mismatch_between_same_named_types_names_their_modules() {
    let dir = std::env::temp_dir().join(format!("karac-type-names-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"tn\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/a.kara"),
        "pub struct P {\n    pub x: i64,\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/b.kara"),
        "pub struct P {\n    pub x: i64,\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import a.P as AP;\nimport b.P as BP;\nfn fa(p: AP) -> i64 {\n    p.x\n}\n\
         fn main() {\n    let q = BP { x: 1 };\n    println(fa(q));\n}\n",
    )
    .unwrap();
    let check = |json: bool| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
        cmd.arg("check").current_dir(&dir);
        if json {
            cmd.arg("--output=json");
        }
        let out = cmd.output().unwrap();
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };
    let (ok, text) = check(false);
    let (_, json) = check(true);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok, "a.P and b.P are distinct types: {text}");
    assert!(text.contains("expected 'a.P', found 'b.P'"), "{text}");
    assert!(!text.contains("P__"), "{text}");
    // The file's own line, with the caret on `q`.
    assert!(
        text.contains("8 |     println(fa(q));\n  |                ^"),
        "{text}"
    );
    assert!(json.contains("expected 'a.P', found 'b.P'"), "{json}");
    assert!(!json.contains("P__"), "{json}");
}

#[test]
fn a_snippet_quotes_the_file_and_moves_its_carets() {
    // The renamed name is on the quoted line, before the caret.
    let dir = std::env::temp_dir().join(format!("karac-type-names-snip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"tn\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/a.kara"),
        "pub struct P {\n    pub x: i64,\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/b.kara"),
        "pub struct P {\n    pub x: i64,\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import a.P as AP;\nimport b.P as BP;\nfn main() {\n    let q: AP = BP { x: 1 };\n    println(q.x);\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    // The host's separator: `src\main.kara` on Windows.
    let text = String::from_utf8_lossy(&out.stderr).replace('\\', "/");
    assert!(!text.contains("P__"), "{text}");
    // The header's column and the carets are the file's, under `BP { x: 1 }`.
    assert!(
        text.contains("src/main.kara:4:17: expected 'a.P', found 'b.P'"),
        "{text}"
    );
    assert!(
        text.contains("4 |     let q: AP = BP { x: 1 };\n  |                 ^^^^^^^^^^^\n"),
        "{text}"
    );
}
