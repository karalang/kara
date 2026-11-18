//! A prelude name imported through its module (kata 2245's `use
//! std::iter::once;`, carried to `import std.iter.once;`, and kata 2246's
//! `import std.collections.VecDeque;`) was refused with no fix and no
//! word that the name needs no import (B-2026-11-17-149). The error now
//! says so and `karac fix` deletes the import; a name that is not in the
//! prelude keeps its plain error.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn fixed(name: &str, head: &str, body: &str, note: &str, out: &str) {
    let dir = std::env::temp_dir().join(format!("karac-2245-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("p.kara");
    let file = file.to_str().unwrap();
    std::fs::write(file, format!("{head}\n\nfn main() {{\n{body}}}\n")).unwrap();
    let check = karac(&["check", file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(stderr.contains(note), "{stderr}");
    let fix = karac(&["fix", file]);
    assert!(fix.status.success(), "{fix:?}");
    let text = std::fs::read_to_string(file).unwrap();
    assert!(!text.contains("import"), "{text}");
    let run = karac(&["__mir-run", file]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), out, "{run:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn kata_2245_prelude_function_import_is_deleted() {
    fixed(
        "once",
        "import std.iter.once;",
        "    let v: Vec[i64] = once(5).collect();\n    println(f\"{v}\");\n",
        "`std.iter` has no item `once`: `once` is in the prelude, visible without an import",
        "[5]\n",
    );
}

#[test]
fn kata_2246_prelude_type_import_from_std_collections_is_deleted() {
    fixed(
        "deque",
        "import std.collections.VecDeque;",
        "    let mut q: VecDeque[i64] = VecDeque.new();\n    q.push_back(3);\n    println(f\"{q.len()}\");\n",
        "unknown module `std.collections`: `VecDeque` is in the prelude",
        "1\n",
    );
    fixed(
        "group",
        "import std.collections.{Map, Set};",
        "    let m: Map[i64, i64] = Map.new();\n    let s: Set[i64] = Set.new();\n    println(f\"{m.len()} {s.len()}\");\n",
        "`Map`, `Set` are in the prelude",
        "0 0\n",
    );
}

#[test]
fn a_name_outside_the_prelude_keeps_its_plain_error() {
    let dir = std::env::temp_dir().join(format!("karac-2245-plain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("p.kara");
    std::fs::write(
        &file,
        "import std.iter.nothere;\n\nfn main() {\n    println(\"x\");\n}\n",
    )
    .unwrap();
    let check = karac(&["check", file.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        stderr.contains("`std.iter` has no item `nothere`"),
        "{stderr}"
    );
    assert!(!stderr.contains("prelude"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}
