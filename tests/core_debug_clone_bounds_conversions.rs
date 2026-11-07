//! design.md §9, the remainder of B-2026-11-06-27 (B-2026-11-07-11):
//! - `dbg` needs `Debug` and never falls back to `Display`;
//! - a derived `Debug` / `Clone` on a generic type holds only where the type
//!   arguments have the trait (`impl[T: Debug] Debug for W[T]`);
//! - `Option.from(x)` / `Result.from(x)` wrap in `Some` / `Ok`;
//! - `x.into()` / `x.try_into()` with nothing fixing the target asks for an
//!   annotation rather than reporting no method.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-debug-clone-conv-{tag}-{}.kara",
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

fn one_error(tag: &str, src: &str, at: (u64, u64), needle: &str) {
    let errs = errors(tag, src);
    assert_eq!(errs.len(), 1, "{tag}: {errs:?}");
    assert_eq!((errs[0].0, errs[0].1), at, "{tag}: {errs:?}");
    assert!(errs[0].2.contains(needle), "{tag}: {errs:?}");
}

#[test]
fn dbg_of_a_display_only_type_is_refused() {
    one_error(
        "dbg-display",
        "#[derive(Display)]
struct Pt { x: i64 }
fn main() { dbg(Pt { x: 1 }); }
",
        (3, 17),
        "dbg() argument must implement `Debug`, and 'Pt' does not",
    );
}

#[test]
fn derived_debug_on_a_generic_type_needs_debug_arguments() {
    one_error(
        "debug-bound",
        "struct Nd {}
#[derive(Debug)]
struct W[T] { v: T }
fn main() { dbg(W { v: Nd {} }); }
",
        (4, 17),
        "'W[Nd]' does not",
    );
}

#[test]
fn derived_clone_on_a_generic_type_needs_clone_arguments() {
    one_error(
        "clone-bound",
        "struct NoC { v: Vec[i64] }
#[derive(Clone)]
struct Pair[T] { a: T }
fn main() {
    let p = Pair { a: NoC { v: [1] } };
    let q = p.clone();
    println(q.a.v.len());
}
",
        (6, 13),
        "'NoC' has no `.clone()`",
    );
}

#[test]
fn into_with_no_target_asks_for_an_annotation() {
    one_error(
        "into-ambiguous",
        "struct A { v: i64 }
struct B { v: i64 }
impl From[i64] for A { fn from(v: i64) -> A { A { v } } }
impl From[i64] for B { fn from(v: i64) -> B { B { v } } }
fn main() { let x = 3.into(); println(1); }
",
        (5, 21),
        "`.into()` on 'i64' cannot pick a conversion: nothing here fixes the target type",
    );
    one_error(
        "try-into-ambiguous",
        "fn main() { let n: i32 = 7.try_into().unwrap(); println(n); }
",
        (1, 26),
        "`.try_into()` on 'i64' cannot pick a conversion",
    );
}

/// The neighbouring correct shapes: `dbg` of a `Debug` type and of a
/// generic one at a `Debug` argument, a derived `Clone` at a cloning
/// argument (a deep copy), `Option.from` / `Result.from`, and annotated
/// `into` / `try_into`. They check clean and print the same on both
/// backends.
#[test]
fn neighbouring_debug_clone_and_conversion_shapes_still_run() {
    let src = "#[derive(Debug)]
struct Pt { x: i64 }
#[derive(Debug, Clone)]
struct W[T] { v: T }
struct A { v: i64 }
impl From[i64] for A { fn from(v: i64) -> A { A { v } } }
fn main() {
    let p = dbg(Pt { x: 1 });
    println(p.x);
    println(W { v: Pt { x: 2 } }.fmt_debug());
    let w = W { v: [1, 2] };
    let mut c = w.clone();
    c.v.push(3);
    println(f\"{w.v.len()} {c.v.len()}\");
    let o: Option[i64] = Option.from(3);
    let r: Result[i64, String] = Result.from(4);
    println(o.unwrap() + r.unwrap());
    let a: A = 5.into();
    println(a.v);
    let n: Result[i32, String] = 9.try_into();
    println(n.unwrap());
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
            "1\nW { v: Pt { x: 2 } }\n2 3\n7\n5\n9\n",
            "{b}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
