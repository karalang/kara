//! B-2026-11-19-109: a `par for` calling a method of a `dyn` parameter
//! whose type declares no effects is checked with the value each caller
//! passes (design.md § `dyn Trait`: a parameter that does not escape has
//! the effects of the value passed, charged at the call), as a generic
//! parameter's call is checked in each instance. A pure method passes,
//! through a parameter that passes the value on too; one that writes a
//! resource is refused, naming the caller; and a function reached as a
//! value, whose callers the check cannot list, is still refused.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, sub: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-par-dyn-caller-{tag}-{}.kara",
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

const HEAD: &str = "effect resource Log;
trait T { fn g(self, x: i64) -> i64; }
struct S {}
impl T for S { fn g(self, x: i64) -> i64 { x * 2 } }
struct W {}
impl T for W { fn g(self, x: i64) -> i64 with writes(Log) { x } }
fn v(xs: Vec[i64], t: ref dyn T + CrossTask) -> Vec[i64] {
    let r = par for x in xs.iter() { t.g(x) };
    r
}
fn pass(xs: Vec[i64], t: ref dyn T + CrossTask) -> Vec[i64] { v(xs, t) }
";

#[test]
fn par_for_through_a_dyn_param_takes_the_effects_of_each_callers_value() {
    let src = format!(
        "{HEAD}fn main() {{
    let a: dyn T + CrossTask = S {{}};
    println(f\"{{v([1, 2], S {{}})}} {{pass([3], S {{}})}} {{v([4], a)}}\");
}}
"
    );
    let out = karac("pure", "check", &src);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac("pure", "__mir-run", &src);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[2, 4] [6] [8]\n");
}

#[test]
fn par_for_through_a_dyn_param_a_caller_passes_a_writer_is_refused() {
    let src = format!(
        "{HEAD}fn main() {{
    println(f\"{{v([1, 2], S {{}})}}\");
    println(f\"{{pass([1, 2], W {{}})}}\");
}}
"
    );
    let out = karac("writer", "check", &src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a writer was accepted");
    assert!(
        stderr.contains(
            "two iterations of the `par for` conflict: `writes(Log)` and `writes(Log)`, \
             with the value `main` passes at line 14"
        ),
        "{stderr}"
    );
}

#[test]
fn par_for_through_a_dyn_param_of_a_function_value_is_still_refused() {
    let src = format!(
        "{HEAD}fn main() {{
    let f = v;
    println(f\"{{f([1, 2], S {{}})}}\");
}}
"
    );
    let out = karac("value", "check", &src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a function value was accepted");
    assert!(
        stderr.contains("`calls(p2.g)` and `calls(p2.g)`"),
        "{stderr}"
    );
}
