//! The `OnceCell` -> `OnceLock` fix renames the program's import with the
//! type when nothing else names `OnceCell`, so it leaves no import of an
//! unused name; a program that still uses `OnceCell` keeps its import.

use std::process::Command;

fn fix(tag: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-once-import-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    for _ in 0..2 {
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["fix", "main.kara"])
            .current_dir(&dir)
            .output()
            .unwrap();
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    text
}

#[test]
fn the_once_cell_rename_takes_an_import_nothing_else_needs() {
    assert_eq!(
        fix(
            "alone",
            "import std.sync.{Mutex, OnceCell};\nsync struct S {\n    c: OnceCell[i64],\n    m: Mutex[i64],\n}\n\nfn main() {\n    println(1);\n}\n",
        ),
        "import std.sync.{Mutex, OnceLock};\nsync struct S {\n    c: OnceLock[i64],\n    m: Mutex[i64],\n}\n\nfn main() {\n    println(1);\n}\n",
    );
}

#[test]
fn the_once_cell_rename_keeps_an_import_another_use_needs() {
    let src = "import std.sync.{OnceCell};\nsync struct S {\n    c: OnceCell[i64],\n}\n\nfn main() {\n    let local: OnceCell[i64] = OnceCell.new();\n    println(1);\n}\n";
    let fixed = fix("shared", src);
    assert!(
        fixed.contains("OnceCell}") || fixed.contains("OnceCell;"),
        "the import went: {fixed}"
    );
    assert!(fixed.contains("c: OnceLock[i64]"), "{fixed}");
}
