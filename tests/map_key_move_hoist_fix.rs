//! `m[key] = v` on a map moves `key` before `v` runs (core semantics §8.2),
//! so a value that reads the key is a use after move (E0500), reported at
//! the read and naming the key's move; `karac fix` repairs it by computing
//! the value first (B-2026-11-01-46).

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
        "karac-map-key-hoist-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn map_key_move_hoist_fix_computes_the_value_first() {
    let src = "fn main() {
    let mut m: Map[String, i64] = Map.new();
    let key = \"abc\".to_string();
    m[key] = key.len();
    println(m[\"abc\"]);
}
";
    let path = temp("fix", src);
    let file = path.to_str().unwrap();

    let check = karac(&["check", "--output=json", file]);
    let json = String::from_utf8_lossy(&check.stdout);
    assert!(json.contains("E0500"), "{json}");
    assert!(json.contains("as the key of a map assignment"), "{json}");
    assert!(json.contains("\"line\":4"), "{json}");

    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let out = std::fs::read_to_string(&path).unwrap();
    assert!(
        out.contains("    let value = key.len();\n    m[key] = value;\n"),
        "{out}"
    );
    let run = karac(&["check", file]);
    assert!(
        run.status.success(),
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn map_key_move_hoist_fix_leaves_a_value_that_writes_the_key() {
    let src = "fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut key = \"a\".to_string();
    m[key] = { key = \"b\".to_string(); 5 };
    println(f\"{m.contains_key(\"a\")} {key}\");
}
";
    let path = temp("write", src);
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    assert!(
        check.status.success(),
        "{}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let _ = std::fs::remove_file(&path);
}
