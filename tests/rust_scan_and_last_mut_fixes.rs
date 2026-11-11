//! Two Rust idioms `karac fix` carries over by rewriting the construct
//! around them.
//!
//! B-2026-11-08-45: Rust's `scan` closure writes a lent state
//! (`|m, x| { *m = (*m).max(x); Some(*m) }`); Kāra's takes it by value and
//! returns `Some((next_state, item))`. One diagnostic says so and carries
//! the rewrite, where six "drop the `*`" fixes left a closure of the wrong
//! type.
//!
//! B-2026-11-08-46: `match v.last_mut() { Some(x) if g => body, _ => other }`
//! and `if let Some(x) = v.last_mut() { .. }` rewrite through an index when
//! only `x` reaches the element.

use std::process::{Command, Output, Stdio};

fn karac(dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .arg("m.kara")
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// `src` after `karac fix` until nothing is left to fix, its `check`
/// stderr, and its output on the interpreter.
fn fixed(tag: &str, src: &str) -> (String, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-rust-scan-last-mut-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), src).unwrap();
    for _ in 0..4 {
        karac(&dir, &["fix"]);
    }
    let text = std::fs::read_to_string(dir.join("m.kara")).unwrap();
    let check = karac(&dir, &["check"]);
    let run = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    (
        text,
        String::from_utf8_lossy(&check.stderr).into_owned(),
        String::from_utf8_lossy(&run.stdout).into_owned(),
    )
}

fn check_errors(tag: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "karac-rust-scan-last-mut-check-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), src).unwrap();
    let out = karac(&dir, &["check"]);
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_rust_scan_closure_names_karas_shape_once_and_is_rewritten() {
    let src = "fn main() {
    let v: Vec[i64] = [3, 1, 4, 1, 5];
    let m: Vec[i64] = v.iter().scan(0, |m, &x| { *m = (*m).max(x); Some(*m) }).collect();
    let s: Vec[i64] = v.iter().scan(0, |acc, x| { *acc += x; Some(*acc * 2) }).collect();
    println(f\"{m} {s}\");
}
";
    let err = check_errors("scan", &src.replace("&x", "x"));
    assert_eq!(err.matches("error[").count(), 2, "{err}");
    assert!(
        err.contains(
            "3:40: this closure writes the state `m` through `*m`, as Rust's `scan` does; \
             Kāra's `scan` closure takes the state by value and returns \
             `Some((next_state, item))`"
        ) && err.contains("write `|m, x| { let m = m.max(x); Some((m, m)) }`"),
        "{err}"
    );
    assert!(!err.contains("drop the `*`"), "{err}");
    let (text, check, out) = fixed("scan", src);
    assert!(
        text.contains("|m, x| { let m = m.max(x); Some((m, m)) }")
            && text.contains("|acc, x| { let acc = acc + x; Some((acc, acc * 2)) }"),
        "{text}"
    );
    assert!(
        check.contains("All checks passed") || check.is_empty(),
        "{check}"
    );
    assert_eq!(out, "[3, 3, 4, 4, 5] [6, 8, 16, 18, 28]\n");
}

// The write under a branch was explained with no fix until kata 1111's
// scan arm; it now copies the state and writes the copy.
#[test]
fn a_rust_scan_writing_its_state_under_a_branch_is_rewritten() {
    let src = "fn main() {
    let v: Vec[i64] = [3, 1, 4];
    let a: Vec[i64] = v.iter().scan(0, |m, x| { if x > 2 { *m = x; } Some(*m) }).collect();
    println(f\"{a}\");
}
";
    let (text, check, out) = fixed("branch", src);
    assert!(text.contains("let mut m = m;"), "{text}");
    assert!(
        check.contains("All checks passed") || check.is_empty(),
        "{check}\n{text}"
    );
    assert_eq!(out, "[3, 3, 4]\n");
    // A `scan` written Kāra's way is untouched.
    let ok = "fn main() {
    let v: Vec[i64] = [3, 1, 4];
    let c: Vec[i64] = v.iter().scan(0, |m, x| { let n = m + x; Some((n, n)) }).collect();
    println(f\"{c}\");
}
";
    let (text, check, out) = fixed("kara-scan", ok);
    assert_eq!(text, ok);
    assert!(
        check.contains("All checks passed") || check.is_empty(),
        "{check}"
    );
    assert_eq!(out, "[3, 4, 8]\n");
}

#[test]
fn a_last_mut_match_and_if_let_are_rewritten_through_an_index() {
    // Kata 2015's arm, and the `if let` form.
    let src = "fn main() {
    let mut ans: Vec[Array[i64, 3]] = Vec.new();
    let segs: Vec[Array[i64, 3]] = [[1, 2, 5], [2, 4, 5], [4, 6, 3]];
    for s in segs.iter() {
        let prev = s[0];
        let pos = s[1];
        let avg = s[2];
        match ans.last_mut() {
            Some(last) if last[1] == prev and last[2] == avg => last[1] = pos,
            _ => ans.push([prev, pos, avg]),
        }
    }
    let mut runs: Vec[i64] = [1];
    if let Some(r) = runs.last_mut() {
        *r += 4;
    }
    println(f\"{ans} {runs}\");
}
";
    let err = check_errors("last-mut", src);
    assert!(
        err.contains("8:15: no method 'last_mut' on type 'Vec'")
            && err.contains("this one rewrites mechanically, as `karac fix` writes it"),
        "{err}"
    );
    let (text, check, out) = fixed("last-mut", src);
    assert!(
        text.contains(
            "        if ans.len() > 0 and ans[ans.len() - 1][1] == prev and ans[ans.len() - 1][2] == avg {
            ans[ans.len() - 1][1] = pos;
        } else { ans.push([prev, pos, avg]); }"
        ) && text.contains(
            "    if runs.len() > 0 {
        runs[runs.len() - 1] += 4;
    }"
        ),
        "{text}"
    );
    assert!(
        check.contains("All checks passed") || check.is_empty(),
        "{check}"
    );
    assert_eq!(out, "[[1, 4, 5], [4, 6, 3]] [5]\n");
}

#[test]
fn a_last_mut_arm_that_names_the_vector_keeps_the_message() {
    let src = "fn main() {
    let mut v: Vec[i64] = [1];
    match v.last_mut() {
        Some(x) if v.len() > 3 => x = 2,
        _ => v.push(0),
    }
}
";
    let err = check_errors("names-vec", src);
    assert!(
        err.contains("after checking the vector is not empty") && !err.contains("rewrites"),
        "{err}"
    );
    let (text, _, _) = fixed("names-vec", src);
    assert!(text.contains("v.last_mut()"), "{text}");
}
