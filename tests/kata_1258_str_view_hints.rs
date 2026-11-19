//! Three string slips the katas made, each of which got a bare refusal
//! (B-2026-11-20-82):
//!
//! - kata 1258's breadth-first arm assigns a `for other in others.iter()`
//!   binding, a `ref String`, into a `Vec[Str]` slot. An owned `String`
//!   there already got "take the view with `.as_str()`" and its fix; the
//!   borrowed one got only "expected 'Str', found 'ref String'". It now
//!   gets the same hint and fix.
//! - kata 1255's harness wrote Rust-flavoured `s.parse_int()`; the refusal
//!   now names `i64.parse(s)`.
//! - kata 1256's first draft cut a string with Rust's `s.get(1..)`; the
//!   refusal now names `s[a..b]` and `s.substring(a, b)`.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("karac-k1258-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

const VIEWS: &str = "fn main() {
    let names: Vec[String] = [\"ab\".to_string(), \"cd\".to_string()];
    let mut views: Vec[Str] = \"x y\".split(\" \").collect();
    for other in names.iter() {
        views[0] = other;
    }
    println(views.join(\" \"));
}
";

#[test]
fn kata_1258_borrowed_string_into_a_str_slot_gets_as_str_fix() {
    let path = write("views", VIEWS);
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        stderr.contains("expected 'Str', found 'ref String': a `String` is passed as a `Str`"),
        "{stderr}"
    );
    let fix = karac(&["fix", file]);
    assert!(fix.status.success(), "{fix:?}");
    let text = std::fs::read_to_string(file).unwrap();
    assert!(text.contains("views[0] = other.as_str();"), "{text}");
    let run = karac(&["__mir-run", file]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "cd y\n", "{run:?}");
    let _ = std::fs::remove_file(&path);
}

const PARSE: &str = "fn main() {
    let s = \"123\".to_string();
    let v: Str = \"45\";
    let a = s.parse_int();
    let b = v.to_i64();
    println(f\"{a} {b}\");
}
";

#[test]
fn kata_1258_rust_style_string_to_number_names_parse_on_the_type() {
    let path = write("parse", PARSE);
    let check = karac(&["check", path.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    for (method, ty) in [("parse_int", "String"), ("to_i64", "Str")] {
        assert!(
            stderr.contains(&format!(
                "no method '{method}' on type '{ty}': a string is parsed by the type it \
                 becomes: `i64.parse(s)`"
            )),
            "{stderr}"
        );
    }
    let _ = std::fs::remove_file(&path);
}

const CUT: &str = "fn main() {
    let s = \"hello\".to_string();
    let v: Str = \"world\";
    let a = s.get(1..);
    let b = v.get(0..2);
    println(f\"{a} {b}\");
}
";

#[test]
fn kata_1258_rust_style_checked_cut_names_slice_and_substring() {
    let path = write("cut", CUT);
    let check = karac(&["check", path.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    for ty in ["String", "Str"] {
        assert!(
            stderr.contains(&format!(
                "no method 'get' on type '{ty}': a string is cut by `s[a..b]`"
            )),
            "{stderr}"
        );
    }
    assert!(stderr.contains("`s.substring(a, b)`"), "{stderr}");
    let _ = std::fs::remove_file(&path);
}
