//! Literal typing rows from the kata threads (design.md §8 Literals: a
//! literal defaults only at the end of the body, after every use has had its
//! chance to fix it):
//! - B-2026-11-18-13: a `for` range's bound and a printed value fix nothing,
//!   so `let n = 100; for a in 0..n { println(a); } take_i32(n)` makes `n`
//!   an `i32`, where the range settled it to `i64` first.
//! - B-2026-11-18-12: an inline list's literals are open for the read's
//!   use, as a bound list's are: `let c: i32 = [0, 45, 90][k]` makes the list
//!   a `Vec[i32]`, where it was refused as a narrowing from `i64`.
//! - B-2026-11-18-14: an unannotated `collect()` handed to a callee's bare
//!   `T` that nothing else carries (`show[T: Display](x: T)`) is refused at
//!   check time asking for an annotation, where it reached the MIR builder
//!   as "does not lower"; a parameter that fixes it still does.

use karac::check_source;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn errors(src: &str) -> Vec<String> {
    check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect()
}

fn modes() -> Vec<&'static str> {
    let mut m = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        m.push("__mir-llvm-run");
    }
    m
}

/// `src`'s stdout under each backend, `None` for a compiled lane with no
/// runtime archive.
fn outputs(src: &str) -> Vec<(&'static str, Option<String>)> {
    static N: AtomicUsize = AtomicUsize::new(0);
    modes()
        .into_iter()
        .map(|mode| {
            let n = N.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!("karac-lit18-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("main.kara"), src).unwrap();
            let out = Command::new(env!("CARGO_BIN_EXE_karac"))
                .args([mode, "main.kara"])
                .current_dir(&dir)
                .output()
                .unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            let err = String::from_utf8_lossy(&out.stderr).into_owned();
            if mode != "__mir-run" && err.contains("runtime archive") {
                return (mode, None);
            }
            assert!(out.status.success(), "{mode}: {err}\n{src}");
            (
                mode,
                Some(String::from_utf8_lossy(&out.stdout).into_owned()),
            )
        })
        .collect()
}

fn assert_prints(src: &str, want: &str) {
    assert!(errors(src).is_empty(), "{src}\n{:#?}", errors(src));
    for (mode, out) in outputs(src) {
        if let Some(out) = out {
            assert_eq!(out, want, "{mode}\n{src}");
        }
    }
}

const TAKE: &str = "fn take_i32(x: i32) -> i32 {\n    x * 2\n}\n";

#[test]
fn a_range_bound_leaves_a_literal_open() {
    for body in [
        "for a in 0..n {\n        println(a);\n    }",
        "let r = 0..n;",
        "let r = ..n;",
        "let r = n..5;",
        "let k = 3;\n    let r = k..n;\n    println(take_i32(k));",
        "for a in 0..n {\n        for b in a + 1..n {\n            let p: Vec[i32] = [a, b];\n        }\n    }",
    ] {
        let src = format!(
            "{TAKE}fn main() {{\n    let n = 4;\n    {body}\n    println(take_i32(n));\n}}\n"
        );
        assert!(errors(&src).is_empty(), "{src}\n{:#?}", errors(&src));
    }
    assert_prints(
        &format!(
            "{TAKE}fn main() {{\n    let n = 4;\n    for a in 0..n {{\n        println(a);\n    \
             }}\n    println(take_i32(n));\n}}\n"
        ),
        "0\n1\n2\n3\n8\n",
    );
    // Nothing fixing it, the range is the default's.
    assert_prints(
        "fn main() {\n    let n = 3000000000;\n    let mut s = 0;\n    for i in n - 2..n {\n        \
         s += i;\n    }\n    println(s);\n}\n",
        "5999999997\n",
    );
    // The type a later use fixes still range-checks the literal.
    let es = errors(
        "fn take_u8(x: u8) -> u8 {\n    x\n}\nfn main() {\n    let n = 300;\n    for a in 0..n \
         {\n        println(a);\n    }\n    println(take_u8(n));\n}\n",
    );
    assert_eq!(es.len(), 1, "{es:#?}");
    assert!(es[0].contains("300 out of range for 'u8'"), "{es:#?}");
}

#[test]
fn a_printed_value_leaves_a_literal_open() {
    assert_prints(
        &format!(
            "{TAKE}fn main() {{\n    let n = 21;\n    println(n);\n    println(take_i32(n));\n    \
             let f = 2.5;\n    println(f);\n    let big = 3000000000;\n    println(big);\n}}\n"
        ),
        "21\n42\n2.5\n3000000000\n",
    );
}

#[test]
fn an_indexed_inline_list_takes_its_read_s_type() {
    assert_prints(
        "fn main() {\n    let k = 2;\n    let c: i32 = [0, 45, 90, 180, 270][k];\n    \
         let b: u8 = [0, 45, 200][k];\n    let f: f32 = [1.5, 2.5][k - 1];\n    \
         let d = [0, 45, 90][k] + 1;\n    let e: i32 = d;\n    \
         let w = [3000000000, 1][k - 2];\n    println(f\"{c} {b} {f} {e} {w}\");\n}\n",
        "90 200 2.5 91 3000000000\n",
    );
    // The read's type range-checks each literal.
    let es = errors(
        "fn main() {\n    let k = 1;\n    let b: u8 = [0, 45, 300][k];\n    println(b);\n}\n",
    );
    assert_eq!(es.len(), 1, "{es:#?}");
    assert!(es[0].contains("300 out of range for 'u8'"), "{es:#?}");
}

const CALLEES: &str = "fn show[T: Display](x: T) {\n    println(f\"{x}\");\n}\n\
fn own_show[T: Display](x: own T) {\n    println(f\"{x}\");\n}\n\
fn count[T](x: Vec[T]) {\n    println(x.len());\n}\n\
fn take(v: Vec[char]) {\n    println(v.len());\n}\n\
fn id[T](x: own T) -> T {\n    x\n}\n";

#[test]
fn a_collect_handed_to_a_bare_generic_is_refused_at_check() {
    for call in ["show", "own_show"] {
        let src = format!(
            "{CALLEES}fn main() {{\n    let b = \"abc\";\n    {call}(b.chars().collect());\n}}\n"
        );
        let es = errors(&src);
        assert_eq!(es.len(), 1, "{src}\n{es:#?}");
        assert!(
            es[0].contains("cannot infer type parameter 'C'")
                && es[0].contains("bind the result with a type annotation"),
            "{es:#?}"
        );
    }
    assert_prints(
        &format!(
            "{CALLEES}fn main() {{\n    let b = \"abc\";\n    take(b.chars().collect());\n    \
             count(b.chars().collect());\n    \
             let x: Option[Vec[char]] = Some(b.chars().collect());\n    \
             let y: Result[Vec[char], String] = Ok(b.chars().collect());\n    \
             let z: Vec[char] = id(b.chars().collect());\n    \
             let v: String = b.chars().collect();\n    show(v);\n    \
             show(b.chars().count());\n    \
             println(f\"{{x.unwrap().len()}} {{y.unwrap().len()}} {{z.len()}}\");\n}}\n"
        ),
        "3\n3\nabc\n3\n3 3 3\n",
    );
}
