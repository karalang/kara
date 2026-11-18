//! B-2026-11-18-7 (kata 1604): `s.parse[i32]()`, the type to parse to written in
//! brackets as Kāra writes a type list, read as an index of `parse` by
//! `i32`, and the error said `i32` is a type, not a function, and to cast
//! with `as i32`, which does not parse a string. It is now named as a
//! call-site type argument, with the fix `i32.parse(s)`, as Rust's
//! `s.parse::<i32>()` already was. An index by a variable is not read so.

use std::process::{Command, Stdio};

fn karac(dir: &std::path::Path, args: &[&str]) -> (bool, String, String) {
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

#[test]
fn a_parse_type_in_brackets_is_fixed_to_the_associated_call() {
    let dir = std::env::temp_dir().join(format!("karac-parse-brackets-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = "fn minutes(t: Str) -> i32 {\n    let (h, m) = t.split_once(\":\").unwrap();\n    h.parse[i32]().unwrap() * 60 + m.parse[i32]().unwrap()\n}\n\nfn main() {\n    let parse = [|s: Str| s.len()];\n    println(parse[0](\"ab\"));\n    println(minutes(\"10:40\"));\n    println(\"7\".parse[u8]().unwrap());\n}\n";
    std::fs::write(dir.join("p.kara"), src).unwrap();
    let (_, _, first) = karac(&dir, &["check", "p.kara"]);
    karac(&dir, &["fix", "p.kara"]);
    let text = std::fs::read_to_string(dir.join("p.kara")).unwrap();
    let (clean, out, err) = karac(&dir, &["check", "p.kara"]);
    let (_, run, _) = karac(&dir, &["__mir-run", "p.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(first.contains("i32.parse(s)"), "{first}");
    assert!(!first.contains("as i32"), "{first}");
    assert!(clean, "{text}\n{out}{err}");
    assert!(
        text.contains("i32.parse(h).unwrap() * 60 + i32.parse(m).unwrap()"),
        "{text}"
    );
    assert!(text.contains("u8.parse(\"7\").unwrap()"), "{text}");
    assert!(text.contains("parse[0](\"ab\")"), "{text}");
    assert_eq!(run, "2\n640\n7\n", "{text}");
}
