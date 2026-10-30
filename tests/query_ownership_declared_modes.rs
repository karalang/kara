//! `karac query ownership` reports a parameter's DECLARED mode, the calling
//! convention under D5 (core-semantics §4.1): `own T` is `own` even when the
//! body only reads it, and `mut ref T` is `mut_ref` even when the body only
//! calls a method on it. The pass once printed the mode it inferred from the
//! body, so an `own String` parameter read as a borrow (B-2026-10-30-35).

use std::process::Command;

#[test]
fn query_ownership_prints_each_parameters_declared_mode() {
    let dir = std::env::temp_dir().join(format!("karac-query-declared-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("app.kara"),
        "fn grab(s: own String) -> i64 {
    s.len()
}

fn eat(s: own String) -> String {
    s
}

fn look(s: String, n: i64, v: mut ref Vec[i64]) -> i64 {
    v.push(n);
    s.len()
}

fn main() {
    let mut v: Vec[i64] = [];
    println(f\"{grab(\"a\".to_string())} {eat(\"b\".to_string())} {look(\"c\".to_string(), 1, mut v)}\");
}
",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["query", "ownership", "app.kara"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let got = String::from_utf8_lossy(&out.stdout);
    for want in [
        "\"function\":\"grab\",\"parameters\":[{\"name\":\"s\",\"mode\":\"own\",\"representation\":\"owned (stack)\"}]",
        "\"function\":\"eat\",\"parameters\":[{\"name\":\"s\",\"mode\":\"own\",\"representation\":\"owned (stack)\"}]",
        "{\"name\":\"s\",\"mode\":\"ref\",\"representation\":\"ref (borrow)\"},{\"name\":\"n\",\"mode\":\"ref\"",
        "{\"name\":\"v\",\"mode\":\"mut_ref\",\"representation\":\"ref (borrow)\"}",
    ] {
        assert!(got.contains(want), "missing {want} in {got}");
    }
}
