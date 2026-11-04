//! B-2026-11-04-80, the two remainders of the sharing/diagnostics roll-up.
//!
//! (b) A `#[deprecated]` enum variant warns where it is used: at a unit,
//! tuple and struct constructor and at the matching patterns (a pattern may
//! name it bare). The deprecation is keyed by its ENUM, so a same-named variant of
//! another enum stays silent.
//!
//! (a) design.md §11 (weak Ergonomics, 8c5e67ab81): `.unwrap()` directly on a
//! weak read (`k.parent.unwrap()`, `k.parent.upgrade().unwrap()`) is a
//! `karac check` warning whose machine-applicable fix is `.expect("…")`. On
//! any other Option it is never a check warning.

use std::process::{Command, Stdio};

fn write(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-deprecated-weak-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), src).unwrap();
    dir
}

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

const DEPRECATED: &str = "enum E {
    #[deprecated(note: \"gone soon\")]
    A,
    B,
    #[deprecated]
    C(i64),
    #[deprecated(since: \"0.8.0\")]
    D { x: i64 },
}
enum F { A, B, C(i64) }
fn main() {
    let e = E.A;
    let c = E.C(3);
    let d = E.D { x: 1 };
    let f = F.A;
    let g = F.C(2);
    match e {
        E.A => println(\"a\"),
        E.C(n) => println(n),
        E.D { x } => println(x),
        _ => println(\"other\"),
    }
    match f { F.A => println(\"fa\"), F.B => println(\"fb\"), F.C(n) => println(n) }
    match c { E.C(n) => println(n), _ => println(\"no\") }
    match d { E.D { x } => println(x), _ => println(\"no\") }
    match g { F.C(n) => println(n), _ => println(\"no\") }
}
";

#[test]
fn a_deprecated_variant_warns_at_its_constructors_and_patterns() {
    let dir = write("variant", DEPRECATED);
    let (ok, out) = karac(&dir, &["check", "m.kara"]);
    assert!(ok, "{out}");
    for want in [
        "m.kara:12:13: use of deprecated item `E.A`: gone soon",
        "m.kara:13:13: use of deprecated item `E.C`",
        "m.kara:14:13: use of deprecated item `E.D` (since 0.8.0)",
        "m.kara:18:9: use of deprecated item `E.A`",
        "m.kara:19:9: use of deprecated item `E.C`",
        "m.kara:20:9: use of deprecated item `E.D`",
        "m.kara:24:15: use of deprecated item `E.C`",
        "m.kara:25:15: use of deprecated item `E.D`",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    // `F.A` and `F.C` share the names of deprecated `E` variants and stay
    // silent; `E.B` is not deprecated.
    assert!(!out.contains("`F."), "{out}");
    assert!(!out.contains("`E.B`"), "{out}");
    assert_eq!(out.matches("deprecated item").count(), 8, "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bare_deprecated_variant_pattern_warns() {
    let dir = write(
        "bare",
        "enum E {
    #[deprecated(note: \"gone soon\")]
    A,
    B,
    #[deprecated]
    C(i64),
}
fn pick(e: E) -> i64 {
    match e {
        A => 1,
        C(n) => n,
        _ => 0,
    }
}
fn main() {
    let x = E.A;
    let y = E.C(4);
    let z = E.B;
    println(pick(x) + pick(y) + pick(z));
}
",
    );
    let (ok, out) = karac(&dir, &["check", "m.kara"]);
    assert!(ok, "{out}");
    for want in [
        "m.kara:10:9: use of deprecated item `E.A`",
        "m.kara:11:9: use of deprecated item `E.C`",
        "m.kara:16:13: use of deprecated item `E.A`",
        "m.kara:17:13: use of deprecated item `E.C`",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    assert!(!out.contains("`E.B`"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

const WEAK: &str = "shared struct P { name: String, mut kids: Vec[C] }
shared struct C { mut parent: weak P, v: i64 }
fn main() {
    let p = P { name: \"root\".to_string(), kids: [] };
    let c = C { parent: p, v: 3 };
    p.kids.push(c);
    let k = p.kids[0];
    println(k.parent.unwrap().name);
    println(k.parent.upgrade().unwrap().name);
    let o: Option[i64] = Some(1);
    println(o.unwrap());
}
";

#[test]
fn unwrap_on_a_weak_read_warns_and_fixes_to_expect() {
    let dir = write("weak", WEAK);
    let (ok, out) = karac(&dir, &["check", "m.kara"]);
    assert!(ok, "{out}");
    for want in [
        "warning[weak_read_unwrap]: m.kara:8:22",
        "warning[weak_read_unwrap]: m.kara:9:32",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    // A plain Option's unwrap is never a check warning (§11).
    assert_eq!(out.matches("weak_read_unwrap").count(), 2, "{out}");

    let (ok, out) = karac(&dir, &["fix", "m.kara"]);
    assert!(ok, "{out}");
    let after = std::fs::read_to_string(dir.join("m.kara")).unwrap();
    assert!(
        after.contains("println(k.parent.expect(\"the referent is alive\").name);"),
        "{after}"
    );
    assert!(
        after.contains("println(k.parent.upgrade().expect(\"the referent is alive\").name);"),
        "{after}"
    );
    assert!(after.contains("println(o.unwrap());"), "{after}");

    let (ok, out) = karac(&dir, &["check", "m.kara"]);
    assert!(ok, "{out}");
    assert!(!out.contains("weak_read_unwrap"), "{out}");
    let (ok, out) = karac(&dir, &["run", "--interp", "m.kara"]);
    assert!(ok, "{out}");
    assert_eq!(out, "root\nroot\n1\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn weak_read_unwrap_reports_its_code_in_json() {
    let dir = write("json", WEAK);
    let (_, out) = karac(&dir, &["check", "--output=json", "m.kara"]);
    assert!(out.contains("W0294"), "{out}");
    assert!(
        out.contains("expect(\\\"the referent is alive\\\")"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
