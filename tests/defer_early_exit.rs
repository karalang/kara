//! design.md §10 Rules: a `?`, a `return`, or a `break` or `continue`
//! whose loop encloses a `defer` / `errdefer` block is refused in it, with
//! one diagnostic. Each crashed `karac check` with a stack overflow, the
//! cleanup lowering re-running itself at its own exit; `errdefer { return
//! Ok(1); }` passed check and failed in MIR. A loop or labeled block
//! written inside the block may still exit itself, and a closure there
//! returns from itself. An `errdefer` in a function with no failure exit
//! is `warning[unreachable_errdefer]`.

use std::process::{Command, Stdio};

const REFUSED: &str =
    "a defer/errdefer block cannot leave its scope early (`?`, `return`, `break`, `continue`)";

fn karac(sub: &str, tag: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-defer-exit-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([sub, "cell.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn assert_refused(tag: &str, src: &str) {
    let (ok, out, err) = karac("check", tag, src);
    let all = format!("{out}{err}");
    assert!(!ok, "{tag}: check passed:\n{all}");
    assert!(!all.contains("overflow"), "{tag}: {all}");
    assert_eq!(all.matches(REFUSED).count(), 1, "{tag}: {all}");
}

#[test]
fn defer_break_out_of_its_loop_refused() {
    assert_refused("y1", "fn main() { loop { defer { break; } } }\n");
}

#[test]
fn defer_continue_of_its_loop_refused() {
    assert_refused(
        "y3",
        "fn main() { for i in 0..2 { defer { continue; } println(i); } }\n",
    );
}

#[test]
fn defer_return_refused() {
    assert_refused(
        "d22",
        "fn f() -> i64 { defer { return 3; } 1 }\nfn main() { println(f()); }\n",
    );
}

#[test]
fn errdefer_return_refused() {
    assert_refused(
        "y2",
        "fn f() -> Result[i64, String] { errdefer { return Ok(1); } Err(\"x\".to_string()) }\n\
         fn main() { let _ = f(); }\n",
    );
}

#[test]
fn defer_question_refused() {
    assert_refused(
        "d06",
        "fn f() -> Result[i64, String] { Err(\"x\".to_string()) }\n\
         fn run() -> Result[i64, String] { defer { let v = f()?; } Ok(1) }\n\
         fn main() { let _x = run(); }\n",
    );
    assert_refused(
        "y4",
        "fn g() -> Option[i64] { None }\n\
         fn f() -> Option[i64] { defer { let _v = g()?; } Some(1) }\n\
         fn main() { let _ = f(); }\n",
    );
}

#[test]
fn defer_labeled_break_past_the_block_refused() {
    assert_refused(
        "label",
        "fn main() {\n    'out: for i in 0..3 {\n        defer {\n            for j in 0..2 {\n                if j == 1 { break 'out; }\n            }\n        }\n        println(i);\n    }\n}\n",
    );
}

#[test]
fn defer_exits_of_its_own_loops_and_closures_run() {
    let src = "fn main() {\n\
        for i in 0..2 {\n\
            defer {\n\
                let mut n = 0;\n\
                for j in 0..10 {\n\
                    if j == 1 { continue; }\n\
                    if j > 3 { break; }\n\
                    n += j;\n\
                }\n\
                'inner: loop { break 'inner; }\n\
                let k = 'b: { if i == 0 { break 'b 7; } 9 };\n\
                let f = |x: i64| { if x > 0 { return x * 2; } 0 };\n\
                println(f\"d {i} {n} {k} {f(i)}\");\n\
            }\n\
            println(f\"i {i}\");\n\
        }\n\
    }\n";
    let (ok, out, err) = karac("__mir-run", "inner", src);
    assert!(ok, "{out}{err}");
    assert_eq!(out, "i 0\nd 0 5 7 0\ni 1\nd 1 5 9 2\n", "{err}");
}

#[test]
fn errdefer_in_a_function_that_cannot_fail_warns() {
    let src = "fn run() -> i64 { errdefer println(\"rb\"); 1 }\nfn main() { let _ = run(); println(\"x\"); }\n";
    let (ok, out, err) = karac("check", "d19", src);
    let all = format!("{out}{err}");
    assert!(ok, "{all}");
    assert!(
        all.contains("warning[unreachable_errdefer]")
            && all.contains("this function cannot return an error, so the errdefer never runs")
            && all.contains("help: use `defer` if the cleanup should always run"),
        "{all}"
    );
    let ok_src = "fn run() -> Option[i64] { errdefer println(\"rb\"); None }\nfn main() { let _ = run(); }\n";
    let (ok, out, err) = karac("check", "d19ok", ok_src);
    assert!(
        ok && !format!("{out}{err}").contains("unreachable_errdefer"),
        "{out}{err}"
    );
}
