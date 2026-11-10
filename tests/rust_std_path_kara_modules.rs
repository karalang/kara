//! B-2026-11-10-16 — a Rust path through a module Kāra's own expressions
//! name (`std.fs.read_to_string(p)`, `std::env::args()`,
//! `std::process::exit(0)`) is fixed to that module's call
//! (`fs.read_to_string(p)`), not to the bare function, which does not
//! resolve. A type after the module (`std::fs::File`) still goes on alone.

use std::process::Command;

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The messages `karac check` gives, then the program `karac fix` leaves,
/// which must check.
fn check_then_fix(tag: &str, src: &str, messages: &[&str], fixed: &str) {
    let dir = std::env::temp_dir().join(format!("karac-std-module-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let p = path.to_str().unwrap();
    let (ok, out) = karac(&["check", p]);
    assert!(!ok, "{tag}: accepted:\n{out}");
    for m in messages {
        assert!(out.contains(m), "{tag}: want `{m}`:\n{out}");
    }
    for _ in 0..2 {
        karac(&["fix", p]);
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), fixed, "{tag}");
    let (ok, out) = karac(&["check", p]);
    assert!(ok, "{tag}: the fixed program checks:\n{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_dotted_std_path_keeps_karas_module() {
    check_then_fix(
        "dots",
        "fn main() {\n    let mut v = [1, 2].to_vec();\n    let w = std.mem.take(mut v);\n    let s = std.fs.read_to_string(\"x.txt\");\n    println(f\"{w.len()} {v.len()} {s.is_err()}\");\n}\n",
        &["write `mem.take`", "write `fs.read_to_string`"],
        "fn main() {\n    let mut v = [1, 2].to_vec();\n    let w = mem.take(mut v);\n    let s = fs.read_to_string(\"x.txt\");\n    println(f\"{w.len()} {v.len()} {s.is_err()}\");\n}\n",
    );
}

#[test]
fn a_colon_std_path_keeps_karas_module() {
    check_then_fix(
        "colons",
        "fn main() {\n    let s = std::fs::read_to_string(\"x.txt\");\n    let a = std::env::args();\n    println(f\"{s.is_err()} {a.len()}\");\n    std::process::exit(0);\n}\n",
        &[
            "write `fs.read_to_string`",
            "write `env.args`",
            "write `process.exit`",
        ],
        "fn main() {\n    let s = fs.read_to_string(\"x.txt\");\n    let a = env.args();\n    println(f\"{s.is_err()} {a.len()}\");\n    process.exit(0);\n}\n",
    );
}

#[test]
fn a_std_path_to_a_type_after_a_kara_module_drops_the_module() {
    check_then_fix(
        "type",
        "fn main() {\n    let f = std::fs::File::open(\"x.txt\");\n    println(f.is_err());\n}\n",
        &["write `File.open`"],
        "fn main() {\n    let f = File.open(\"x.txt\");\n    println(f.is_err());\n}\n",
    );
}
