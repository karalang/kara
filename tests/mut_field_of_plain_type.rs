//! B-2026-11-04-79, design.md §5 Structs (25d27bd53a): `mut` on a field of
//! a plain struct, or of a plain enum's struct variant, is a compile error
//! from the next minor release. In 0.8.x it is a `deprecated` warning whose
//! machine-applicable fix deletes the keyword. A `shared` type's `mut` field
//! is the form the keyword is for and stays silent, and
//! `#[allow(deprecated)]` on the type suppresses the warning.

use std::process::{Command, Stdio};

const SRC: &str = "struct P { mut port: i64, name: String }
enum E { A { mut x: i64 }, B }
shared struct S { mut n: i64 }
#[allow(deprecated)]
struct Q { mut q: i64 }
fn main() {
    let mut p = P { port: 1, name: \"a\".to_string() };
    p.port = 2;
    let s = S { n: 1 };
    s.n = 3;
    let q = Q { q: 4 };
    let e = E.A { x: 5 };
    match e { E.A { x } => println(x), E.B => println(0) }
    println(f\"{p.port} {s.n} {q.q} {p.name}\");
}
";

fn karac(dir: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn mut_on_a_plain_field_warns_and_fix_deletes_it() {
    let dir = std::env::temp_dir().join(format!("karac-mut-plain-field-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), SRC).unwrap();

    let (ok, out) = karac(&dir, &["check", "m.kara"]);
    assert!(ok, "{out}");
    for want in [
        "warning[deprecated]: m.kara:1:12: `mut` on field `port` of struct `P` is deprecated",
        "warning[deprecated]: m.kara:2:14: `mut` on field `x` of enum `E` is deprecated",
        "It becomes an error in the next minor release; `karac fix` deletes it",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    // The `shared` field and the allowed type stay silent.
    assert_eq!(out.matches("warning[deprecated]").count(), 2, "{out}");

    let (_, json) = karac(&dir, &["check", "--output=json", "m.kara"]);
    assert!(
        json.contains("\"replacement\":{\"offset\":11,\"length\":4,\"text\":\"\"}"),
        "{json}"
    );
    assert!(
        json.contains("\"new\":\"struct P { port: i64, name: String }\""),
        "{json}"
    );

    let (ok, out) = karac(&dir, &["fix", "m.kara"]);
    assert!(ok, "{out}");
    let after = std::fs::read_to_string(dir.join("m.kara")).unwrap();
    assert!(after.starts_with("struct P { port: i64, name: String }\nenum E { A { x: i64 }, B }\nshared struct S { mut n: i64 }\n"), "{after}");
    assert!(after.contains("struct Q { mut q: i64 }"), "{after}");

    let (ok, out) = karac(&dir, &["check", "m.kara"]);
    assert!(ok, "{out}");
    assert!(!out.contains("warning[deprecated]"), "{out}");
    let (ok, out) = karac(&dir, &["run", "--interp", "m.kara"]);
    assert!(ok, "{out}");
    assert_eq!(out, "5\n2 3 4 a\n");
    let _ = std::fs::remove_dir_all(&dir);
}
