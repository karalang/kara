//! A `fold` with a bare literal seed takes the number type the call is
//! checked against, whatever the receiver chain: the expectation was
//! cleared by the check of an adapter's own argument (the `1` of
//! `skip(1)`, a `map`'s closure), so `nums.into_iter().skip(1).fold(0, |a,
//! b| a | b)` in an `i32` tail kept an `i64` seed and was refused as a
//! narrowing, where the same fold with no adapter checked
//! (B-2026-11-18-90).

mod common;

use std::process::Command;

fn karac(name: &str, src: &str, cmd: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-fold-seed-adapters-{name}-{cmd}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// `karac check` accepts `src`, and the interpreter and, built with the
/// backend, the compiled program and its ASan build print `want`.
fn fold_runs_everywhere(name: &str, src: &str, want: &str) {
    let check = karac(name, src, "check");
    assert!(
        check.status.success(),
        "{name}: check: {}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let cmds: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run", "__mir-llvm-asan"]
    } else {
        &["__mir-run"]
    };
    for cmd in cmds {
        let out = karac(name, src, cmd);
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        // Only a failed link may skip: a refusal, a runtime error or a
        // sanitizer report is the program's own.
        if *cmd != "__mir-run" {
            let linked = common::link_or_skip(if out.status.success() {
                Ok(())
            } else {
                Err(err.clone())
            });
            if linked.is_none() {
                continue;
            }
        }
        assert!(out.status.success(), "{name}: {cmd}: {err}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}: {cmd}");
    }
}

/// `karac check`'s output for `src`, which it refuses.
fn fold_refused(name: &str, src: &str) -> String {
    let out = karac(name, src, "check");
    assert!(!out.status.success(), "{name}: accepted");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Kata 2317's mutation and its neighbours, each in an `i32` tail.
#[test]
fn a_bare_fold_seed_takes_the_tail_type_through_an_adapter_chain() {
    fold_runs_everywhere(
        "tail",
        "fn skip(nums: own Vec[i32]) -> i32 {\n    nums.into_iter().skip(1).fold(0, |a, b| a | b)\n}\nfn take(nums: own Vec[i32]) -> i32 {\n    nums.into_iter().take(2).fold(0, |a, b| a + b)\n}\nfn filter(nums: own Vec[i32]) -> i32 {\n    nums.iter().filter(|x| x > 1).fold(0, |a, b| a + b)\n}\nfn map(nums: own Vec[i32]) -> i32 {\n    nums.into_iter().map(|x| x + 1).fold(0, |a, b| a | b)\n}\nfn max(nums: own Vec[i32]) -> i32 {\n    nums.iter().skip(1).fold(0, |a, b| a.max(b))\n}\nfn plain(nums: own Vec[i32]) -> i32 {\n    nums.into_iter().fold(0, |a, b| a | b)\n}\nfn main() {\n    let v = [1i32, 2i32, 4i32].to_vec();\n    println(f\"{skip(v.clone())} {take(v.clone())} {filter(v.clone())} {map(v.clone())} {max(v.clone())} {plain(v)}\");\n}\n",
        "6 3 6 7 4 7\n",
    );
}

/// An annotated `let` is the same expectation, and an `i64` slot still
/// gives an `i64` fold over `i32` items.
#[test]
fn a_bare_fold_seed_takes_a_let_annotation_through_an_adapter_chain() {
    fold_runs_everywhere(
        "let",
        "fn main() {\n    let v = [1i32, 2i32, 4i32].to_vec();\n    let r: i32 = v.iter().skip(1).fold(0, |a, b| a | b);\n    let w: i64 = v.iter().skip(1).fold(0, |a, b| a + b as i64);\n    let u: u8 = v.iter().take(2).fold(0, |a, b| a + b as u8);\n    println(f\"{r} {w} {u}\");\n}\n",
        "6 6 3\n",
    );
}

/// With no expectation at the fold, the seed keeps the default, and the
/// refusal still names the literal's suffix.
#[test]
fn a_fold_seed_with_no_expectation_keeps_the_default() {
    let shown = fold_refused(
        "open",
        "fn f(nums: own Vec[i32]) -> i32 {\n    let r = nums.into_iter().skip(1).fold(0, |a, b| a | b);\n    r\n}\nfn main() {\n    println(f([1i32, 2i32].to_vec()));\n}\n",
    );
    assert!(
        shown.contains("error[E0200]") && shown.contains("`0i32`"),
        "{shown}"
    );
}
