//! E0127, a binding named outside Value-class (`let Total = 3`), carries the
//! rename its help names, applied to the binding and every use of it, and
//! withholds it when a use cannot be placed or the new name is taken
//! (B-2026-11-21-74).

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

/// The text `karac fix` leaves, whether it checks clean, and its output
/// under the MIR interpreter.
fn mend(name: &str, src: &str) -> (String, bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-binding-rename-{name}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(dir.join("cell.kara")).unwrap();
    let (clean, _, _) = karac(&dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (text, clean, out)
}

#[test]
fn a_type_class_binding_is_renamed_with_every_use() {
    let (text, clean, out) = mend(
        "uses",
        "fn main() {\n    let Total = 3;\n    let mut N = Total + 1;\n    N += 1;\n    for Item in [1, 2] {\n        println(Item + N);\n    }\n    println(f\"{Total} {N}\");\n}\n",
    );
    assert_eq!(
        text,
        "fn main() {\n    let total = 3;\n    let mut n = total + 1;\n    n += 1;\n    for item in [1, 2] {\n        println(item + n);\n    }\n    println(f\"{total} {n}\");\n}\n"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "6\n7\n3 5\n");
}

/// `max` is already a local, so `MAX` becoming `max` would merge the two:
/// the diagnostic keeps its help and carries no edit.
#[test]
fn a_rename_onto_a_name_already_bound_is_withheld() {
    let src = "fn main() {\n    let max = 2;\n    let MAX = 5;\n    println(MAX + max);\n}\n";
    let (text, clean, _) = mend("local", src);
    assert_eq!(text, src);
    assert!(!clean);
}

/// `max` is also a builtin, which a local `max` would shadow at the call.
#[test]
fn a_rename_onto_a_builtins_name_is_withheld() {
    let src = "fn main() {\n    let MAX = 5;\n    println(max(MAX, 2));\n}\n";
    let (text, clean, _) = mend("builtin", src);
    assert_eq!(text, src);
    assert!(!clean);
}

/// A shadowing binding of the same name is its own symbol, renamed on its
/// own, and the shadowed one's uses stay with it.
#[test]
fn a_shadowing_binding_and_the_one_it_shadows_are_renamed_apart() {
    let (text, clean, out) = mend(
        "shadow",
        "fn main() {\n    let Count = 1;\n    println(Count);\n    let Count = Count + 10;\n    println(Count);\n}\n",
    );
    assert_eq!(
        text,
        "fn main() {\n    let count = 1;\n    println(count);\n    let count = count + 10;\n    println(count);\n}\n"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "1\n11\n");
}
