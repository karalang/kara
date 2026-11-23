//! B-2026-11-23-9: a function's effects were listed in the order
//! inference reached its callees and the effect checker's diagnostics in the
//! order it walked its hash tables, so an unrelated change to the compiler
//! reordered both for a program it did not touch. The effects are listed by
//! their text now, as the `with` clause's fix writes them, and the
//! diagnostics by where they point.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Three public functions out of name order, each performing effects out of
/// text order.
const SRC: &str = r#"pub fn zeta(v: Vec[i64]) -> i64 {
    println(v[0]);
    eprintln("z");
    v[0] + 1
}

pub fn alpha(n: i64) {
    println(n);
}

pub fn mid(n: i64) -> Vec[i64] {
    eprintln("m");
    [n, n]
}

fn main() {
    println(zeta([1]));
    alpha(2);
    println(mid(3).len());
}
"#;

fn check_json(tag: &str, src: &str) -> serde_json::Value {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("karac-effect-order-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "cell.kara", "--output=json"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json")
}

#[test]
fn effect_errors_are_listed_in_source_order_with_their_effects_in_text_order() {
    let v = check_json("errors", SRC);
    let found: Vec<(u64, String)> = v["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|d| d["severity"] == "error")
        .map(|d| {
            (
                d["line"].as_u64().unwrap_or_default(),
                d["message"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let lines: Vec<u64> = found.iter().map(|(line, _)| *line).collect();
    assert_eq!(lines, [1, 7, 11], "{found:?}");
    assert!(
        found[0]
            .1
            .contains("'zeta' performs effects [writes(Stderr), writes(Stdout)]"),
        "{found:?}"
    );
}

#[test]
fn a_programs_effects_are_listed_in_text_order() {
    // The same program with nothing to declare, so its effects are reported.
    let v = check_json("effects", &SRC.replace("pub fn", "fn"));
    let effects: Vec<&str> = v["program_effects"]
        .as_array()
        .expect("program_effects")
        .iter()
        .map(|e| e.as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        effects,
        [
            "allocates(Heap)",
            "panics",
            "writes(Stderr)",
            "writes(Stdout)"
        ]
    );
}
