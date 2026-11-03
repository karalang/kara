//! A generic function whose type parameter is instantiated at an erased
//! `Fn` calls the value it is given, so a call of it has that value's
//! effects (core-semantics §12: a generic call's effects are the
//! instance's). The effect pass resolved a call through a value of an
//! erased `Fn` type to every effect, so two `par` branches calling
//! `ap[F: Fn(i64) -> i64]` with pure closures conflicted as "unknown"
//! (B-2026-11-02-36). It now follows the erased value back to the closure
//! or function the caller erased into it: pure arguments pass, and a
//! printing one still conflicts by name.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-generic-fn-value-par-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

const PRELUDE: &str = "fn ap[F: Fn(i64) -> i64](x: i64, f: F) -> i64 { f(x) }
fn loud(x: i64) -> i64 { println(\"l\"); x }
";

#[test]
fn a_generic_call_given_a_printing_fn_value_conflicts_by_name() {
    for (tag, par) in [
        (
            "closure",
            "par { ap(1, |x| { println(\"a\"); x }), ap(2, |x| { println(\"b\"); x }) }",
        ),
        ("fn-item", "par { ap(1, loud), { println(\"x\"); 2 } }"),
    ] {
        let src = format!(
            "{PRELUDE}fn main() {{\n    let (a, b) = {par};\n    println(f\"{{a}} {{b}}\");\n}}\n"
        );
        let path = temp(tag, &src);
        let check = karac(&["check", path.to_str().unwrap()]);
        let _ = std::fs::remove_file(&path);
        let err = String::from_utf8_lossy(&check.stderr);
        assert!(
            !check.status.success()
                && err.contains(
                    "branches 1 and 2 of the `par` conflict: `writes(Stdout)` and `writes(Stdout)`"
                ),
            "{tag}: {err}"
        );
    }
}

#[test]
fn a_generic_call_given_pure_fn_values_is_no_conflict() {
    let src = format!(
        "{PRELUDE}fn dbl(x: i64) -> i64 {{ x * 2 }}
fn main() {{
    let (a, b) = par {{ ap(3, |x| x * 2), ap(4, |x| x * 2) }};
    let (c, d) = par {{ ap(3, dbl), ap(4, dbl) }};
    let k = 5;
    let (e, f) = par {{ ap(1, |x| x + k), ap(2, |x| x * k) }};
    println(f\"{{a + b}} {{c + d}} {{e}} {{f}} {{loud(0)}}\");
}}
"
    );
    let path = temp("pure", &src);
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        (
            run.status.code(),
            String::from_utf8_lossy(&run.stdout).as_ref()
        ),
        (Some(0), "l\n14 14 6 10 0\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
