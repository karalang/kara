//! Kata 2289's iterator arm folds with an annotated tuple-pattern
//! parameter, `|(mut stack, mut most): (Vec[(i64, i64)], i64), x|`. The
//! annotation borrows, as a single name's does, but only a single name was
//! told so: the tuple got a raw "expected 'Vec[?T1]', found 'ref Vec'" and
//! a plain E0517, with no `own` and no fix. Its bindings now take the
//! parameter's `own` hint, spelled for the whole pattern, and `karac fix`
//! writes `own` before the tuple type (B-2026-11-18-87).

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn kata_2289_tuple_pattern_closure_param_gets_the_own_fix() {
    let src = "fn main() {
    let nums = [5, 3, 4, 4, 7, 3, 6, 11, 8, 5, 11].to_vec();
    let (_, most) = nums.iter().rev().fold((Vec.new(), 0), |(stack, most): (Vec[i64], i64), x| {
        let mut stack = stack;
        stack.push(x);
        (stack, most.max(stack.len()))
    });
    println(most);
}
";
    let file = write("k2289-tuple-own", src);
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string()
        + &String::from_utf8_lossy(&check.stdout);
    assert!(
        stderr.contains("`|(stack, most): own (Vec[i64], i64)|`"),
        "{stderr}"
    );
    assert!(!stderr.contains("found '(ref Vec[i64], i64)'"), "{stderr}");
    let mut fixed = String::new();
    for _ in 0..2 {
        let fix = karac(&["fix", &file]);
        assert!(
            fix.status.success(),
            "{}",
            String::from_utf8_lossy(&fix.stderr)
        );
        fixed = std::fs::read_to_string(&file).unwrap();
    }
    let _ = std::fs::remove_file(&file);
    assert!(
        fixed.contains("|(stack, most): own (Vec[i64], i64), x|"),
        "{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "11\n");
}
