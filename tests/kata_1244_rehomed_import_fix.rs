//! Kata 1244's heap arm carried Rust's two `use` lines over as one import,
//! `import std.collections.{PriorityQueue, Reverse};`. There is no
//! `std.collections`: `PriorityQueue` is the prelude's and `Reverse` is
//! `std.cmp`'s. The import was refused as an unknown module with no word
//! of either and no fix (B-2026-11-20-80). The error now says where each
//! name is and `karac fix` rewrites the import to the module that has the
//! rest; an item that no module has keeps the plain error.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn scratch(name: &str, text: &str) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("karac-1244-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("p.kara");
    std::fs::write(&file, text).unwrap();
    let file = file.to_str().unwrap().to_string();
    (dir, file)
}

#[test]
fn kata_1244_prelude_and_std_cmp_import_is_rehomed() {
    let (dir, file) = scratch(
        "heap",
        "import std.collections.{PriorityQueue, Reverse};\n\nfn main() {\n    \
         let mut q: PriorityQueue[Reverse[i64]] = PriorityQueue.new();\n    \
         q.push(Reverse { value: 3 });\n    q.push(Reverse { value: 7 });\n    \
         match q.pop() {\n        Some(Reverse { value: v }) => println(v),\n        \
         None => {}\n    }\n}\n",
    );
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        stderr.contains(
            "unknown module `std.collections`: `PriorityQueue` is in the prelude, visible \
             without an import, and `Reverse` is `std.cmp`'s, so the import becomes \
             `import std.cmp.Reverse;`"
        ),
        "{stderr}"
    );
    let fix = karac(&["fix", &file]);
    assert!(fix.status.success(), "{fix:?}");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("import std.cmp.Reverse;\n"), "{text}");
    assert!(!text.contains("std.collections"), "{text}");
    let run = karac(&["__mir-run", &file]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "7\n", "{run:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_rehomed_import_with_an_unknown_item_keeps_its_plain_error() {
    let (dir, file) = scratch(
        "plain",
        "import std.collections.{Reverse, NoSuchThing};\n\nfn main() {\n    println(1);\n}\n",
    );
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        stderr.contains("unknown module `std.collections`"),
        "{stderr}"
    );
    assert!(!stderr.contains("std.cmp"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}
