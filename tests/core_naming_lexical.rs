//! design.md §3 (B-2026-11-06-18): the naming and lexical rules `karac check`
//! used to miss.
//! - CN-2: every binding name is Value-class (snake_case), on `let`, `for`
//!   and pattern bindings, not only on parameters;
//! - CN-5: a leading `_` marks an unused binding, never a type or a const;
//! - `requires`, `ensures` and `invariant` are not v1 keywords;
//! - an f-string inside an f-string's `{..}` is deprecated (0.8.x; an
//!   error at the next minor release);
//! - a `///` run that documents nothing is discarded with a warning, never a
//!   parse error and never silently.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-naming-lexical-{tag}-{}.kara",
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

/// `(severity, line, column, message)` of every `karac check --output=json`
/// diagnostic that is an error or a warning.
fn diags(tag: &str, src: &str) -> Vec<(String, u64, u64, String)> {
    let out = karac(tag, &["check", "--output=json"], src);
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{tag}: {e}"));
    json["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|d| d["severity"] == "error" || d["severity"] == "warning")
        .map(|d| {
            (
                d["severity"].as_str().unwrap_or("").to_string(),
                d["line"].as_u64().unwrap(),
                d["column"].as_u64().unwrap(),
                d["message"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

fn errors(tag: &str, src: &str) -> Vec<(u64, u64, String)> {
    diags(tag, src)
        .into_iter()
        .filter(|d| d.0 == "error")
        .map(|d| (d.1, d.2, d.3))
        .collect()
}

fn run_stdout(tag: &str, src: &str) -> Vec<(String, String)> {
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    backends
        .iter()
        .map(|b| {
            let out = karac(tag, &[b], src);
            (
                b.to_string(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
            )
        })
        .collect()
}

#[test]
fn binding_names_outside_parameters_must_be_value_class() {
    let src = "const MAX: i64 = 3;
enum E { A, B }
fn main() {
    let Count = 1;
    let COUNT = 2;
    let (Ab, b) = (1, 2);
    for Ix in 0..3 { println(Ix); }
    let o = Some(4);
    match o { Some(Val) => println(Val), None => {} }
    match 3 { MAX => println(1), _ => {} }
    let e = E.A;
    match e { A => println(0), B => println(1) }
    println(Count + COUNT + Ab + b);
}
";
    let errs = errors("cn2", src);
    let at: Vec<(u64, u64)> = errs.iter().map(|e| (e.0, e.1)).collect();
    assert_eq!(at, vec![(4, 9), (5, 9), (6, 10), (7, 9)], "{errs:?}");
    assert!(
        errs.iter().all(|e| e
            .2
            .contains("binding names must be Value-class (snake_case)")),
        "{errs:?}"
    );
    assert!(errs[1].2.contains("`COUNT` is Const-class"), "{errs:?}");
    assert!(errs[0].2.contains("renaming to `count`"), "{errs:?}");
    // A Type-class name in a refutable pattern may be a bare variant of the
    // scrutinee's enum, so the type checker decides it: `Some(Val)` over an
    // `i64` payload binds, and is refused there; `NotFound` over a library
    // `IoError` is its variant.
    let errs = errors(
        "cn2-pattern",
        "fn main() {
    let o = Some(4);
    match o { Some(Val) => println(Val), None => {} }
    let e = IoError.PermissionDenied;
    match e { NotFound => println(0), PermissionDenied => println(1), _ => println(2) }
}
",
    );
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!((errs[0].0, errs[0].1), (3, 20), "{errs:?}");
    assert!(errs[0].2.contains("`Val` is Type-class"), "{errs:?}");
}

#[test]
fn leading_underscore_types_and_consts_are_refused() {
    let src = "struct _Hidden {}
const _MAX: i64 = 1;
fn main() { let _unused = 2; println(1); }
";
    let errs = errors("cn5", src);
    let at: Vec<(u64, u64)> = errs.iter().map(|e| (e.0, e.1)).collect();
    assert_eq!(at, vec![(1, 1), (2, 1)], "{errs:?}");
    assert!(errs[0]
        .2
        .contains("`_Hidden` begins with `_`, but struct names may not"));
    assert!(errs[1].2.contains("rename it to `MAX`"), "{errs:?}");
}

/// design.md §3 says a nested f-string is an error; 0.8.x reports it as
/// `deprecated` (programs written against 0.8.0 nest them), with a fix that
/// writes the inner text in the hole when the hole is the inner f-string
/// alone. The program still runs.
#[test]
fn a_nested_f_string_is_deprecated() {
    let src = "fn main() {
    let x = 4;
    println(f\"a {f\"<{x}>\"} b\");
    println(f\"c {f\"{x}\".len()} d\");
}
";
    let out = karac("nested-f", &["check", "--output=json"], src);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let warned: Vec<(u64, u64, String)> = json["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|d| {
            d["message"]
                .as_str()
                .unwrap_or("")
                .starts_with("use of deprecated nested f-string")
        })
        .map(|d| {
            (
                d["line"].as_u64().unwrap(),
                d["column"].as_u64().unwrap(),
                d["replacement"]["text"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    assert_eq!(
        warned,
        vec![(3, 18, "<{x}>".to_string()), (4, 18, String::new())],
        "{json}"
    );
    assert!(errors("nested-f", src).is_empty());
    for (b, out) in run_stdout("nested-f-run", src) {
        assert_eq!(out, "a <4> b\nc 1 d\n", "{b}");
    }
}

#[test]
fn unused_doc_comments_warn_and_the_program_runs() {
    let src = "/// Documents nothing: a blank line follows.

/// A point.
struct Pt { x: i64 }

/// An impl takes no doc comment.
impl Pt {
    fn get(self) -> i64 { self.x }
}

/// Doubles.
fn double(n: i64) -> i64 {
    /// inside a body
    let m = n * 2;
    m
}

fn main() {
    println(double(Pt { x: 3 }.get()));
}
/// trailing
";
    let ds = diags("doc", src);
    let warned: Vec<(u64, u64)> = ds
        .iter()
        .filter(|d| d.0 == "warning" && d.3.contains("this doc comment documents nothing"))
        .map(|d| (d.1, d.2))
        .collect();
    assert_eq!(warned, vec![(1, 1), (6, 1), (13, 5), (21, 1)], "{ds:?}");
    assert!(ds.iter().all(|d| d.0 != "error"), "{ds:?}");
    assert!(ds
        .iter()
        .any(|d| d.1 == 6 && d.3.contains("an `impl` block takes no doc comment")));
    assert!(ds
        .iter()
        .any(|d| d.1 == 13 && d.3.contains("a statement takes no doc comment")));
    for (b, out) in run_stdout("doc-run", src) {
        assert_eq!(out, "6\n", "{b}");
    }
    let script = "/// a script statement
println(1);
";
    let ds = diags("doc-script", script);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert!(
        ds[0].3.contains("a statement takes no doc comment"),
        "{ds:?}"
    );
}

/// The correct neighbours: parameters keep their own check, the contract
/// words are ordinary names (a field, a `let`, a parameter) while contracts
/// still parse, a const pattern and bare variant patterns are not
/// bindings, `_`-prefixed bindings are fine, and doc comments on items and
/// members attach without a warning.
#[test]
fn neighbouring_naming_and_doc_shapes_still_run() {
    let errs = errors(
        "param",
        "fn f(Count: i64) -> i64 { Count }
fn main() { println(f(1)); }
",
    );
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!((errs[0].0, errs[0].1), (1, 6), "{errs:?}");

    let src = "/// A spec.
struct Spec {
    /// The bound.
    invariant: i64,
    requires: i64,
}
/// Checks.
fn check(requires: i64, ensures: i64) -> i64 requires requires > 0 { requires + ensures }
const LIMIT: i64 = 3;
fn main() {
    let invariant = 3;
    let _skip = 0;
    let s = Spec { invariant, requires: 1 };
    match 3 { LIMIT => println(s.invariant + check(1, 2)), _ => {} }
    let x = 2;
    println(f\"{x} {s.requires}\");
}
";
    let ds = diags("ok", src);
    assert!(ds.is_empty(), "the correct shapes must check clean: {ds:?}");
    for (b, out) in run_stdout("ok-run", src) {
        assert_eq!(out, "6\n2 1\n", "{b}");
    }
}
