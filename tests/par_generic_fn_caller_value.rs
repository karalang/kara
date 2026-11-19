//! B-2026-11-19-94: two `par` branches, or two iterations of a `par for`,
//! calling a generic function-typed parameter (`fn both[F: Fn() -> i64](f:
//! F)`) have the effects of the function value each caller passes (core
//! semantics §12: a call through a non-escaping function-typed parameter
//! has the effects of the argument, charged at the call). A pure closure
//! or function passes, through a generic that forwards it too; a closure
//! that prints is refused, naming the caller.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, sub: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-par-generic-fn-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(sub)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

const HEAD: &str = "fn both[F: Fn() -> i64](f: F) -> i64 {
    let (a, b) = par { f(), f() };
    a + b
}
fn each[F: Fn(i64) -> i64](xs: Vec[i64], f: F) -> Vec[i64] {
    par for x in xs.iter() { f(x) }
}
fn fwd[F: Fn() -> i64](f: F) -> i64 { both(f) }
fn five() -> i64 { 5 }
";

#[test]
fn par_branches_calling_a_generic_fn_param_take_each_callers_value() {
    let src = format!(
        "{HEAD}fn main() {{
    let k = 2;
    println(both(|| k));
    println(f\"{{each([1, 2], |x| x + k)}}\");
    println(fwd(|| k + 1));
    println(both(five));
}}
"
    );
    for sub in ["check", "__mir-run"] {
        let out = karac("pure", sub, &src);
        assert!(
            out.status.success(),
            "{sub}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        if sub == "__mir-run" {
            assert_eq!(String::from_utf8_lossy(&out.stdout), "4\n[3, 4]\n6\n10\n");
        }
    }
}

#[test]
fn par_branches_calling_a_generic_fn_param_a_caller_passes_a_printer_is_refused() {
    let src = format!(
        "{HEAD}fn main() {{
    println(both(five));
    println(fwd(|| {{ println(\"hi\"); 1 }}));
}}
"
    );
    let out = karac("printer", "check", &src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a printer was accepted");
    assert!(
        stderr.contains(
            "branches 1 and 2 of the `par` conflict: `writes(Stdout)` and `writes(Stdout)`, \
             with the value `main` passes at line 12"
        ),
        "{stderr}"
    );
}
