//! Rust's `use std::iter::Peekable; use std::str::Chars;` and a parameter
//! `&mut Peekable<Chars>` (kata 1106's recursive descent) stopped the Mend
//! loop at `std.iter has no item Peekable` and `unknown module std.str`
//! (B-2026-11-10-35). Both imports name prelude types, so `karac fix`
//! deletes them; `Chars` is Kāra's `StrChars`; and `Peekable[StrChars]`
//! gets its item, `Peekable[StrChars, char]`, from a fix of its own.

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
        "karac-rust-peekable-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

const RUST: &str = "use std::iter::Peekable;
use std::str::Chars;

fn read(chars: &mut Peekable<Chars>) -> i64 {
    let mut n = 0;
    while let Some(c) = chars.next() {
        if c == 'a' {
            n += 1;
        }
    }
    n
}

fn main() {
    let mut c = \"abca\".chars().peekable();
    println(read(mut c));
}
";

#[test]
fn rust_peekable_of_chars_mends_to_a_running_program() {
    let path = temp("mend", RUST);
    let first = karac(&["check"], &path);
    let stderr = String::from_utf8_lossy(&first.stderr).into_owned();
    assert!(
        stderr.contains("`std::iter::Peekable` is Rust's standard library -- Kāra has `Peekable` in its prelude"),
        "{stderr}"
    );
    for _ in 0..6 {
        let before = std::fs::read_to_string(&path).unwrap();
        karac(&["fix"], &path);
        if std::fs::read_to_string(&path).unwrap() == before {
            break;
        }
    }
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("import") && !text.contains("use "), "{text}");
    assert!(
        text.contains("fn read(chars: mut ref Peekable[StrChars, char]) -> i64 {"),
        "{text}"
    );
    let check = karac(&["check"], &path);
    assert!(
        check.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, &path);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "2\n", "{lane:?}");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_one_argument_peekable_is_given_its_item() {
    let path = temp(
        "arity",
        "fn read(chars: mut ref Peekable[StrChars]) -> i64 {\n    0\n}\n\nfn main() {\n    \
         let mut c = \"ab\".chars().peekable();\n    println(read(mut c));\n}\n",
    );
    let out = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains(
            "`Peekable` takes two type arguments, the iterator and its item: write \
             `Peekable[StrChars, char]`"
        ),
        "{stderr}"
    );
    assert!(stderr.contains("1 error(s) found"), "{stderr}");
}

#[test]
fn a_rust_iter_import_of_a_real_item_is_not_deleted() {
    let path = temp(
        "repeat",
        "use std::iter::repeat;\n\nfn main() {\n    println(1);\n}\n",
    );
    let out = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!stderr.contains("delete the line"), "{stderr}");
    assert!(stderr.contains("`std.iter.repeat`"), "{stderr}");
}
