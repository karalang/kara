//! A `par` whose branches call through a stored callback (B-2026-10-27-2).
//! A call through a function value whose type declares no effects has every
//! effect (core-semantics §12), so two such branches conflict, and the
//! refusal says which call it could not see through instead of a bare
//! `unknown` and `unknown`. One whose type declares its effects has exactly
//! those, so two calls that only read run together.

use std::process::Command;

/// `karac check` on `src`: its error lines, empty when it passes.
fn check(tag: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "karac-par-stored-callback-{tag}-{}",
        std::process::id()
    ));
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

const SRC: &str = r#"
struct Job { run: Fn(i64) -> i64 }

fn sq(x: i64) -> i64 { x * x }

fn main() {
    let j = Job { run: sq };
    let (a, b) = par { (j.run)(3), (j.run)(4) };
    println(f"{a + b}");
}
"#;

#[test]
fn mir_par_stored_callback_without_a_clause_names_the_call() {
    let err = check("bare", SRC);
    assert!(
        err.contains(
            "branches 1 and 2 of the `par` conflict: `unknown (a call through a value)` and `unknown (a call through a value)`"
        ),
        "{err}"
    );
}

#[test]
fn mir_par_stored_callback_with_a_clause_has_its_effects() {
    // The slot declares only `reads(Clock)`, and two reads do not conflict.
    let src = SRC.replace("Fn(i64) -> i64 }", "Fn(i64) -> i64 with reads(Clock) }");
    assert_eq!(check("clause", &src), "");
    let r = karac::mir::lower::run_source(&src).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(r.output, "25\n", "stderr: {}", r.stderr);
    // Two writes of the declared resource keep their order.
    let src = SRC.replace("Fn(i64) -> i64 }", "Fn(i64) -> i64 with writes(Stdout) }");
    let err = check("writes", &src);
    assert!(
        err.contains("conflict: `writes(Stdout)` and `writes(Stdout)`"),
        "{err}"
    );
}

#[test]
fn mir_par_stored_callback_captured_from_a_let_has_its_effects() {
    let src = r#"
fn sq(x: i64) -> i64 { x * x }

fn pick(n: i64) -> Fn(i64) -> i64 with reads(Clock) {
    sq
}

fn main() {
    let f: Fn(i64) -> i64 with reads(Clock) = pick(0);
    let (a, b) = par { f(3), f(4) };
    println(f"{a + b}");
}
"#;
    // Inside a `par` branch `f` is a capture, and the call through it has
    // the binding's clause (B-2026-10-27-8).
    assert_eq!(check("let", src), "");
    let r = karac::mir::lower::run_source(src).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(r.output, "25\n", "stderr: {}", r.stderr);
    let writes = src.replace("reads(Clock)", "writes(Stdout)");
    let err = check("let-writes", &writes);
    assert!(
        err.contains("conflict: `writes(Stdout)` and `writes(Stdout)`"),
        "{err}"
    );
}
