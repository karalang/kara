//! core-types.md (B-2026-11-07-16): `Option.take`, `replace`,
//! `get_or_insert` and `get_or_insert_with` take `mut ref self`, so they are
//! refused through a bare (borrowing) parameter, a field of one, an element
//! of an immutable `Vec`, and on a local declared without `mut`, the way
//! `Vec.push` is. Accepted, every backend wrote the caller's value through
//! the shared borrow.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-option-mut-ref-self-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

/// `(line, column, message)` of every error `karac check --output=json`
/// reports.
fn errors(tag: &str, src: &str) -> Vec<(u64, u64, String)> {
    let out = karac(tag, &["check", "--output=json"], src);
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{tag}: {e}"));
    json["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|d| d["severity"] == "error")
        .map(|d| {
            (
                d["line"].as_u64().unwrap(),
                d["column"].as_u64().unwrap(),
                d["message"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

#[test]
fn option_writers_through_a_bare_parameter_are_refused() {
    let src = "struct S { o: Option[String] }
fn f(o: Option[String]) -> Option[String] { o.take() }
fn g(s: S) -> Option[String] { s.o.replace(\"new\") }
fn h(o: Option[String]) -> i64 { o.get_or_insert(\"a\").len() }
fn k(o: Option[String]) -> i64 { o.get_or_insert_with(|| \"b\").len() }
fn main() {
    let a: Option[String] = Some(\"x\");
    println(f(a));
    println(h(a));
    println(k(a));
    let s = S { o: Some(\"old\") };
    println(g(s));
}
";
    let errs = errors("param", src);
    let at: Vec<(u64, u64)> = errs.iter().map(|e| (e.0, e.1)).collect();
    assert_eq!(at, vec![(2, 45), (3, 32), (4, 34), (5, 34)], "{errs:?}");
    assert!(errs[0].2.contains("`o` is a bare parameter"), "{errs:?}");
    assert!(errs[1].2.contains("`s` is a bare parameter"), "{errs:?}");
}

#[test]
fn option_writers_on_a_non_mut_local_are_refused() {
    let src = "fn main() {
    let a: Option[i64] = Some(1);
    let o = a.replace(2);
    let b: Option[String] = Some(\"x\");
    let p = b.take();
    let v: Vec[Option[i64]] = [Some(1)];
    let w = v[0].take();
    println(f\"{o} {p} {w}\");
}
";
    let errs = errors("local", src);
    let at: Vec<(u64, u64)> = errs.iter().map(|e| (e.0, e.1)).collect();
    assert_eq!(at, vec![(3, 13), (5, 13), (7, 13)], "{errs:?}");
    assert!(
        errs.iter().all(|e| e.2.contains("declared without `mut`")),
        "{errs:?}"
    );
}

/// The correct shapes: a `mut ref` parameter, a `mut` local and a `mut`
/// `Vec`'s element, an owned parameter, and an iterator's `take(n)` (not
/// `Option`'s). They check clean and print the same on both backends.
#[test]
fn option_writers_through_mut_places_still_run() {
    let src = "fn f(o: mut ref Option[String]) -> Option[String] { o.take() }
fn g(o: own Option[i64]) -> Option[i64] { let mut o = o; o.replace(9) }
fn main() {
    let mut a: Option[String] = Some(\"x\");
    let t = f(mut a);
    println(f\"{t} {a}\");
    let mut b: Option[i64] = None;
    let n = b.get_or_insert(4) + 0;
    println(f\"{n} {b} {g(Some(1))}\");
    let mut v: Vec[Option[i64]] = [Some(1), None];
    let w = v[0].take();
    println(f\"{w} {v}\");
    let firsts: Vec[i64] = [5, 6, 7].into_iter().take(2).collect();
    println(firsts);
}
";
    let errs = errors("ok", src);
    assert!(
        errs.is_empty(),
        "the correct shapes must check clean: {errs:?}"
    );
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for b in backends {
        let out = karac("ok-run", &[b], src);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "Some(x) None\n4 Some(4) Some(1)\nSome(1) [None, None]\n[5, 6]\n",
            "{b}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
