//! A closure parameter nothing has typed yet is typed by a later use in
//! the same body (design.md § Closures, synthesis mode), and an operator on
//! two such parameters waits for that use: `let f = |x, n| x + n; f(1, 2)`
//! was refused at the operator, "type '?T0' does not implement trait Add",
//! before the call that types `x` and `n` was seen (B-2026-11-10-8). A
//! parameter no use types is §8's "cannot infer closure parameter type".

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-closure-param-operator-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

fn check_errors(tag: &str, src: &str) -> String {
    let path = temp(tag, src);
    let out = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(!out.status.success(), "{tag} checked clean");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

const PROGRAM: &str = r#"fn main() {
    let f = |x, n| x + n;
    println(f(1, 2));
    println(f(10, 20));
    let lt = |x, n| x < n;
    println(lt(1, 2));
    let bits = |x, n| x & n;
    println(bits(6, 3));
    let h = |x, n| x * n + x;
    let a: i32 = 3;
    println(h(a, 4));
    let same = |x, n| x == n;
    println(same(true, false));
    let pick = |a, b| if a > b { a } else { b };
    let total = |v: Vec[i64]| {
        let add = |p, q| p + q;
        let mut s = 0;
        for x in v {
            s = add(s, x);
        }
        s
    };
    println(pick(3, 9));
    println(total([1, 2, 3].to_vec()));
    let g = |x, y| (x - y) * (x + y);
    println(g(5.0, 3.0));
}
"#;

const EXPECTED: &str = "3\n30\ntrue\n2\n15\nfalse\n9\n6\n16.0\n";

/// The default executor (the JIT on an llvm build, the interpreter
/// otherwise) and the interpreter.
const LANES: [&[&str]; 2] = [&["run"], &["run", "--interp"]];

#[test]
fn an_operator_on_closure_parameters_waits_for_the_call_that_types_them() {
    let path = temp("run", PROGRAM);
    for lane in LANES {
        let out = karac(lane, &path);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            EXPECTED,
            "{lane:?}; stderr:\n{stderr}"
        );
        assert!(out.status.success(), "{lane:?}; stderr:\n{stderr}");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_operator_on_closure_parameters_nothing_types_is_the_closure_message() {
    let err = check_errors("uncalled", "fn main() {\n    let f = |x, n| x + n;\n}\n");
    assert!(err.contains("cannot infer closure parameter type"), "{err}");
    assert!(!err.contains("does not implement trait"), "{err}");
}

#[test]
fn an_operator_on_closure_parameters_a_call_types_as_a_string_is_refused() {
    let err = check_errors(
        "string",
        "fn main() {\n    let f = |a, b| a + b;\n    println(f(\"a\".to_string(), \"b\".to_string()));\n}\n",
    );
    assert!(
        err.contains("'+' on 'String': a closure parameter that a later use types"),
        "{err}"
    );
}

#[test]
fn a_to_vec_of_operator_closures_leads_with_the_move_only_refusal() {
    let err = check_errors(
        "to-vec",
        "fn main() {\n    let ops: Vec[Fn(i64, i64) -> i64] = [|x, n| x + n, |x, n| x - n].to_vec();\n    println(ops.len());\n}\n",
    );
    let first = err.lines().next().unwrap_or("");
    assert!(first.contains("is move-only"), "{err}");
    assert!(!err.contains("does not implement trait"), "{err}");
}
