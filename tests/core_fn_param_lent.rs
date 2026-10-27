//! A bare function-typed parameter borrows (core-semantics §4.1, §9.3), so
//! passing a function value held in a collection element to one moves
//! nothing (B-2026-10-27-12). An `escaping`, `own` or `OnceFn` parameter
//! still owns, so the element move is refused.

use std::process::Command;

/// `karac check` on `src`: its error lines, empty when it passes.
fn check(tag: &str, src: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("karac-fn-param-lent-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    let errors: Vec<&str> = err.lines().filter(|l| l.starts_with("error")).collect();
    assert_eq!(out.status.success(), errors.is_empty(), "{err}");
    errors.join("\n")
}

const ELEMENT_MOVE: &str =
    "cannot move a non-`Copy` value out of a collection element: the collection still owns it";

fn program(param: &str) -> String {
    format!(
        "fn doubler(n: i64) -> i64 {{ n * 2 }}\n\
fn apply(f: {param}, x: i64) -> i64 {{ f(x) }}\n\
fn main() {{\n    let mut v: Vec[Fn(i64) -> i64] = Vec.new();\n    v.push(doubler);\n    println(f\"{{apply(v[0], 4)}}\");\n}}\n"
    )
}

#[test]
fn core_fn_element_passed_to_a_bare_fn_parameter_is_lent() {
    assert_eq!(check("fn", &program("Fn(i64) -> i64")), "");
}

#[test]
fn core_fn_element_passed_to_a_static_or_instance_method_is_lent() {
    let src = "fn doubler(n: i64) -> i64 { n * 2 }\n\
struct Runner { k: i64 }\n\
impl Runner {\n    fn make(f: Fn(i64) -> i64) -> Runner { Runner { k: f(1) } }\n    fn run(self, f: Fn(i64) -> i64) -> i64 { f(self.k) }\n}\n\
fn main() {\n    let mut v: Vec[Fn(i64) -> i64] = Vec.new();\n    v.push(doubler);\n    let r = Runner.make(v[0]);\n    println(f\"{r.run(v[0])}\");\n}\n";
    assert_eq!(check("method", src), "");
}

#[test]
fn core_fn_element_passed_to_an_owning_fn_parameter_is_a_move() {
    for (tag, param) in [
        ("escaping", "escaping Fn(i64) -> i64"),
        ("own", "own Fn(i64) -> i64"),
    ] {
        let err = check(tag, &program(param));
        assert!(err.contains(ELEMENT_MOVE), "{tag}: {err}");
    }
    // The `Fn` element is read as the `OnceFn` the parameter expects, which
    // the typechecker refuses as a move out of the index.
    let err = check("once", &program("OnceFn(i64) -> i64"));
    assert!(err.contains("cannot move `v[0]` out of an index"), "{err}");
}
