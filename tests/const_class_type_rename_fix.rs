//! LeetCode's `class H2O` written `struct H2O` is Const class in Kāra
//! (design.md §3 CN-1, CN-4: acronyms are words, `IoError`), so it is
//! refused, and the rename was left to the author at every use
//! (B-2026-11-11-35, kata 1117). A private `struct` or `enum` now gets the
//! rename, `H2o`, as one fix over every use of the name in the file.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-type-rename-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

const H2O: &str = r#"struct H2O {
    count: i64,
}

impl H2O {
    fn new() -> H2O {
        H2O { count: 0 }
    }

    fn bump(mut ref self) {
        self.count += 1;
    }
}

enum IO {
    Read,
    Write,
}

fn main() {
    let mut h = H2O.new();
    h.bump();
    let op = IO.Read;
    match op {
        IO.Read => println(h.count),
        IO.Write => println(0),
    }
}
"#;

#[test]
fn a_const_class_struct_and_enum_are_renamed_at_every_use() {
    let path = temp("h2o", H2O);
    let first = karac(&["check"], &path);
    let stderr = String::from_utf8_lossy(&first.stderr).into_owned();
    assert!(
        stderr.contains("`H2O` is Const-class but struct names must be Type-class (PascalCase); consider renaming to `H2o`"),
        "{stderr}"
    );
    let fixed = karac(&["fix"], &path);
    assert!(
        fixed.status.success(),
        "{}",
        String::from_utf8_lossy(&fixed.stderr)
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("H2O") && !text.contains("IO"), "{text}");
    assert!(
        text.contains("impl H2o {") && text.contains("Io.Write =>"),
        "{text}"
    );
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, &path);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n", "{lane:?}");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_rename_an_f_string_would_miss_is_withheld() {
    let src = "struct H2O {\n    n: i64,\n}\n\nfn main() {\n    let h = H2O { n: 2 };\n    \
               println(f\"{H2O { n: 3 }.n} {h.n}\");\n}\n";
    let path = temp("fstring", src);
    karac(&["fix"], &path);
    let text = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(text, src);
}
