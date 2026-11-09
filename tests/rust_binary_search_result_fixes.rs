//! B-2026-11-09-14: Rust's `binary_search` taken apart as a `Result`.
//! Kāra's returns `Option[i64]` (docs/library/collections.md), so the
//! mechanical shapes are rewritten through `partition_point` by `karac
//! fix`, and the others say why they are not.

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

/// `check`'s stderr before any fix; then the text after `karac fix` until
/// nothing is left, its `check` stderr and its interpreter output.
fn fix_and_run(tag: &str, src: &str) -> (String, String, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-rust-binary-search-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), src).unwrap();
    let before = String::from_utf8_lossy(&karac(&dir, &["check"]).stderr).into_owned();
    for _ in 0..3 {
        karac(&dir, &["fix"]);
    }
    let text = std::fs::read_to_string(dir.join("m.kara")).unwrap();
    let check = String::from_utf8_lossy(&karac(&dir, &["check"]).stderr).into_owned();
    let out = String::from_utf8_lossy(&karac(&dir, &["__mir-run"]).stdout).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    (before, text, check, out)
}

const SHAPE: &str = "Kāra's `binary_search` returns `Option[i64]`, the index of an element \
                     equal to the value or `None`, not Rust's `Result` of an index or the \
                     insertion point; the insertion point is `partition_point(|y| y < v)`";

#[test]
fn unwrap_or_else_and_ok_err_matches_are_rewritten_through_partition_point() {
    let src = "fn main() {
    let v: Vec[i64] = [1, 3, 5];
    let want = 4;
    let at = v.binary_search(want).unwrap_or_else(|x| x);
    let b = v.binary_search(3).unwrap_or_else(|i| i + 100);
    let mut prices: Vec[i64] = [2, 6];
    let p = 4;
    match prices.binary_search(p) {
        Ok(i) => println(f\"have {i}\"),
        Err(i) => prices.insert(i, p),
    }
    let e = match v.binary_search(5) { Ok(_) => 10, Err(i) => i };
    if let Err(pos) = v.binary_search(0) { println(f\"ins {pos}\"); }
    if let Ok(i) = v.binary_search(3) { println(f\"at {i}\"); } else { println(\"no\"); }
    println(f\"{at} {b} {e} {prices}\");
}
";
    let (before, text, check, out) = fix_and_run("rewrite", src);
    assert_eq!(before.matches("error[").count(), 6, "{before}");
    assert!(
        before.contains(&format!(
            "4:14: {SHAPE}: write `v.partition_point(|y| y < want)`"
        )),
        "{before}"
    );
    assert!(
        before.contains(&format!(
            "8:5: {SHAPE}; `karac fix` rewrites this one through `partition_point`"
        )),
        "{before}"
    );
    assert!(
        text.contains("let at = v.partition_point(|y| y < want);")
            && text.contains(
                "    {
        let i = prices.partition_point(|y| y < p);
        if i < prices.len() and prices[i] == p {
            println(f\"have {i}\")
        } else {
            prices.insert(i, p)
        }
    }"
            ),
        "{text}"
    );
    assert!(check.contains("All checks passed"), "{text}\n{check}");
    assert_eq!(out, "ins 0\nat 1\n2 1 10 [2, 4, 6]\n");
}

#[test]
fn a_shape_the_rewrite_cannot_name_again_says_why() {
    let src = "fn key(x: i64) -> i64 { x * 2 }
fn main() {
    let v: Vec[i64] = [1, 3, 5];
    let a = v.binary_search(key(1)).unwrap_or_else(|i| i);
    let c = v.binary_search(4).unwrap_err();
    let r = match v.clone().binary_search(3) { Ok(i) => i, Err(_) => 0 };
    println(f\"{a} {c} {r}\");
}
";
    let (before, text, _, _) = fix_and_run("why", src);
    assert_eq!(before.matches("error[").count(), 3, "{before}");
    for (at, why) in [
        (
            "4:13",
            "the predicate names the value once per probe, so bind it to a `let` first",
        ),
        (
            "5:13",
            "`.unwrap_err()` takes Rust's `Result` apart, and the `Option` has no counterpart",
        ),
        (
            "6:13",
            "the rewrite names the sequence more than once, so bind it to a `let` first",
        ),
    ] {
        assert!(
            before.contains(&format!(
                "{at}: {SHAPE}; this one is not rewritten, as {why}"
            )),
            "{at}\n{before}"
        );
    }
    assert_eq!(text, src);
}

#[test]
fn kata_2035s_insertion_point_search_is_rewritten() {
    let src = "fn main() {
    let ys: Vec[i64] = [1, 4, 9, 16];
    let mut best = 1000;
    for want in [0, 5, 16, 20] {
        let at = ys.binary_search(want).unwrap_or_else(|x| x);
        if at < ys.len() {
            best = best.min((want - ys[at]).abs());
        }
        println(at);
    }
    println(best);
}
";
    let (_, text, check, out) = fix_and_run("kata-2035", src);
    assert!(
        text.contains("let at = ys.partition_point(|y| y < want);"),
        "{text}"
    );
    assert!(check.contains("All checks passed"), "{check}");
    assert_eq!(out, "0\n2\n3\n4\n0\n");
}
