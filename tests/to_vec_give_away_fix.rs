//! `[a, b].to_vec()` where an owned `Vec[P]` is wanted, with `a` and `b`
//! bare (borrowed) parameters, reports each element as a parameter given
//! away, with the machine fix writing `own` on it, as `vec![a, b]` and
//! `v.push(a)` do, rather than one `Vec[ref P]` mismatch of the whole
//! literal (B-2026-10-29-14). A binding out of a bare parameter names that
//! parameter's `own` (B-2026-10-29-4). `Copy` elements stay accepted.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn karac_fix_owns_the_parameters_an_array_to_vec_gives_away() {
    let src = "#[derive(Clone)]
struct P {
    name: String,
}
fn total(ps: Vec[P]) -> i64 {
    ps.iter().map(|p| p.name.len()).sum()
}
fn both(a: P, b: P) -> i64 {
    total([a, b].to_vec())
}
fn stored(a: P, b: P) -> i64 {
    let v: Vec[P] = [a, P { name: \"xyz\".to_string() }, b].to_vec();
    total(v)
}
fn count(names: Vec[String]) -> i64 {
    names.len()
}
fn pick(by: Option[String]) -> i64 {
    match by {
        Some(key) => count([key].to_vec()),
        None => 0,
    }
}
fn sum(xs: Vec[i64]) -> i64 {
    xs.iter().sum()
}
fn copies(n: i64, m: i64) -> i64 {
    sum([n, m].to_vec())
}
fn main() {
    let p = P { name: \"ab\".to_string() };
    let q = P { name: \"c\".to_string() };
    println(f\"{both(p.clone(), q.clone())} {stored(p, q)} {pick(Some(\"k\".to_string()))} {copies(2, 3)}\");
}
";
    let path = std::env::temp_dir().join(format!(
        "karac-to-vec-give-away-{}.kara",
        std::process::id()
    ));
    let file = path.to_str().unwrap();
    std::fs::write(&path, src).unwrap();

    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr);
    assert!(!err.contains("found 'Vec[ref"), "{err}");
    for want in [
        ":9:12: cannot give away parameter `a`",
        ":9:15: cannot give away parameter `b`",
        ":12:22: cannot give away parameter `a`",
        ":12:56: cannot give away parameter `b`",
        "cannot give away `key`, bound out of parameter `by`",
    ] {
        assert!(err.contains(want), "missing {want:?} in:\n{err}");
    }
    assert_eq!(err.matches("cannot give away").count(), 5, "{err}");

    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let out = std::fs::read_to_string(&path).unwrap();
    for want in [
        "fn both(a: own P, b: own P)",
        "fn stored(a: own P, b: own P)",
        "fn pick(by: own Option[String])",
        "fn copies(n: i64, m: i64)",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        (
            run.status.code(),
            String::from_utf8_lossy(&run.stdout).as_ref()
        ),
        (Some(0), "3 6 1 5\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
