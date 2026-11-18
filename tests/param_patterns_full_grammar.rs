//! B-2026-11-18-27: a parameter takes any irrefutable pattern, in functions
//! and closures alike (design.md § Destructuring parameters): `ref name` and
//! its struct-field shorthand, a fixed-array pattern, `name @ pattern`, and a
//! distinct type's constructor. Whether a pattern can fail is the declared
//! type's question, so `E.A(x): E` and `Some(x): Option[i64]` are refused
//! with one message, where `E.A(x)` was a pair of parse errors and `Id(n)`
//! over a distinct type was refused as refutable.

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
            let dir = std::env::temp_dir().join(format!("karac-ppat-{}-{n}", std::process::id()));
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

#[test]
fn a_ref_binding_destructures_an_owned_parameter() {
    assert_prints(
        "struct Req {\n    h: String,\n    body: Vec[i64],\n}\n\
         fn f(Req { h: ref hh, body }: own Req) -> i64 {\n    hh.len() + body.len()\n}\n\
         fn g(Req { ref h, body }: own Req) -> i64 {\n    h.len() * 10 + body.len()\n}\n\
         fn nested((ref s, k): own (String, i64)) -> i64 {\n    s.len() + k\n}\n\
         struct P {\n    name: String,\n    n: i64,\n}\n\
         fn bump(P { mut ref n, .. }: mut ref P) {\n    n += 1;\n}\n\
         fn main() {\n    println(f(Req { h: \"ab\".to_string(), body: [1] }));\n    \
         println(g(Req { h: \"abc\".to_string(), body: [1, 2] }));\n    \
         println(nested((\"xyz\".to_string(), 4)));\n    \
         let mut p = P { name: \"a\".to_string(), n: 1 };\n    bump(mut p);\n    \
         println(p.n);\n    \
         let ws: Vec[P] = [P { name: \"xyz\".to_string(), n: 2 }];\n    \
         let ls: Vec[i64] = ws.iter().map(|P { ref name, n }| name.len() + n).collect();\n    \
         println(ls[0]);\n}\n",
        "3\n32\n7\n2\n5\n",
    );
}

#[test]
fn an_array_pattern_and_an_at_binding_stand_as_parameters() {
    assert_prints(
        "fn arr([a, b, c]: Array[i64, 3]) -> i64 {\n    a * 100 + b * 10 + c\n}\n\
         fn rest([first, ..tail]: Array[i64, 4]) -> i64 {\n    first + tail.len()\n}\n\
         fn at(name @ (a, b): (i64, i64)) -> i64 {\n    name.0 + a * b\n}\n\
         fn main() {\n    println(arr([1, 2, 3]));\n    println(rest([10, 1, 2, 3]));\n    \
         println(at((3, 4)));\n    let sum = |[p, q]: Array[i64, 2]| p + q;\n    \
         println(sum([5, 6]));\n    let pair = |whole @ (x, y): (i64, i64)| whole.1 + x + y;\n    \
         println(pair((1, 2)));\n}\n",
        "123\n13\n15\n11\n5\n",
    );
}

#[test]
fn a_distinct_type_s_constructor_is_irrefutable_in_a_parameter() {
    assert_prints(
        "distinct type Id = i64;\nfn unwrap_id(Id(n): Id) -> i64 {\n    n + 1\n}\n\
         fn main() {\n    println(unwrap_id(Id(41)));\n    let unid = |Id(n): Id| n * 2;\n    \
         println(unid(Id(21)));\n}\n",
        "42\n42\n",
    );
}

#[test]
fn a_parameter_pattern_that_can_fail_is_refused_once() {
    for (src, head) in [
        (
            "enum E {\n    A(i64),\n    B,\n}\nfn f(E.A(x): E) -> i64 {\n    x\n}\n\
             fn main() {\n    println(f(E.A(1)));\n}\n",
            "`E.A(..)`",
        ),
        (
            "enum E {\n    A(i64),\n    B,\n}\nfn main() {\n    let c = |E.A(x): E| x;\n    \
             println(c(E.B));\n}\n",
            "`E.A(..)`",
        ),
        (
            "fn g(Some(x): Option[i64]) -> i64 {\n    x\n}\nfn main() {\n    println(g(Some(1)));\n}\n",
            "`Some(..)`",
        ),
        (
            "fn h([a, b]: Vec[i64]) -> i64 {\n    a + b\n}\nfn main() {\n    println(h([1, 2]));\n}\n",
            "`[..]`",
        ),
    ] {
        let es = errors(src);
        assert_eq!(es.len(), 1, "{src}\n{es:#?}");
        assert!(
            es[0].contains("a parameter's pattern must be irrefutable") && es[0].contains(head),
            "{src}\n{es:#?}"
        );
    }
}
