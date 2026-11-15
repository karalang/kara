//! Slice patterns over a `Vec` are checked length by length
//! (design.md § Pattern exhaustiveness, "Arm i is reachable iff
//! U(P[0..i], P[i])"; B-2026-11-15-96). A fixed-length arm after a rest
//! arm that covers it is unreachable (`[_, _]` after `[x, ..]`, `[0]`
//! after `[.., _]`), a wildcard after `[]` and `[x, ..]` is unreachable,
//! and `[.., 0]` after `[0, ..]` is reachable (`[1, 0]`): it was warned
//! about, so `karac fix` deleted a live arm (B-2026-11-15-97).

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SRC: &str = r#"fn f(v: Vec[i64]) -> i64 {
    match v {
        [x, ..] => x,
        [_, _] => 2,
        _ => 0,
    }
}
fn g(v: Vec[i64]) -> i64 {
    match v {
        [.., _] => 1,
        [0] => 2,
        _ => 0,
    }
}
fn h(v: Vec[i64]) -> i64 {
    match v {
        [0, ..] => 1,
        [.., 0] => 2,
        _ => 0,
    }
}
fn k(v: Vec[i64]) -> i64 {
    match v {
        [a, ..] => a,
        [.., b] => b,
        [] => 0,
    }
}
fn m(v: Vec[i64]) -> i64 {
    match v {
        [0, ..] => 1,
        [.., 0] => 2,
        [1, 0] => 3,
        [_, _, ..] => 4,
        [0, _, 0] => 5,
        _ => 0,
    }
}
fn w(v: Vec[i64]) -> i64 {
    match v {
        [] => 0,
        [b, ..] => b,
        _ => 9,
    }
}
fn o(v: Option[Vec[bool]]) -> i64 {
    match v {
        Some([]) => 0,
        Some([true, ..]) => 1,
        Some([false, ..]) => 2,
        None => 3,
    }
}
fn main() {
    println(f"{f([1, 2].to_vec())} {g([0].to_vec())} {h([1, 0].to_vec())} {k([5].to_vec())}");
    println(f"{m([2, 0].to_vec())} {m([2, 3].to_vec())} {w([4].to_vec())} {o(Some([false].to_vec()))}");
}
"#;

const NOT_COVERED: &str = r#"fn a(v: Vec[i64]) -> i64 {
    match v {
        [x, ..] => x,
    }
}
fn b(v: Vec[bool]) -> i64 {
    match v {
        [] => 0,
        [true, ..] => 1,
        [.., true] => 2,
    }
}
fn d(o: Option[Vec[i64]]) -> i64 {
    match o {
        Some([x]) => x,
        None => 0,
    }
}
fn main() {}
"#;

fn karac(args: &[&str], src: &str) -> (bool, String, String) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-slice-reach-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The `line:col` of each diagnostic of `kind` (`warning[unreachable_arm]`).
fn lines(out: &str, kind: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.starts_with(kind))
        .filter_map(|l| {
            let at = l.split("main.kara:").nth(1)?;
            let mut p = at.split(':');
            Some(format!("{}:{}", p.next()?, p.next()?))
        })
        .collect()
}

#[test]
fn slice_arms_covered_by_a_rest_arm_are_unreachable() {
    let (ok, out, err) = karac(&["check"], SRC);
    assert!(ok, "{out}{err}");
    let both = format!("{out}{err}");
    assert_eq!(
        lines(&both, "warning[unreachable_arm]"),
        ["4:9", "11:9", "25:9", "33:9", "35:9", "43:9"],
        "{both}"
    );
}

#[test]
fn slice_arm_after_a_rest_arm_it_overlaps_still_runs() {
    let (ok, out, err) = karac(&["__mir-run"], SRC);
    assert!(ok, "{err}");
    assert_eq!(out, "1 1 2 5\n2 4 4 2\n");
}

#[test]
fn slice_match_witnesses_name_the_missing_lengths() {
    let (ok, out, err) = karac(&["check"], NOT_COVERED);
    assert!(!ok);
    let both = format!("{out}{err}");
    let got: Vec<&str> = both
        .lines()
        .filter(|l| l.contains("non-exhaustive match"))
        .map(|l| l.split("non-exhaustive match: ").nth(1).unwrap_or(l))
        .collect();
    assert_eq!(
        got,
        [
            "pattern `[]` not covered",
            "pattern `[false]` not covered",
            "patterns `Some([])`, `Some([_, _, ..])` not covered",
        ],
        "{both}"
    );
}
