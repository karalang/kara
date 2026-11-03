//! A closure handed to an adaptor chain rooted at a range
//! (`(1..n + 1).filter(|i| not side.contains(i)).collect()`) runs only while
//! its statement does, exactly like one rooted at `v.iter()`
//! (B-2026-10-29-43). The use classifier did not count a range among the
//! closure-free roots, so moving the binding the closure reads after the
//! statement was refused as "moved inside a loop" with no loop in the
//! program (B-2026-11-03-20). The pair inside a real loop is still refused.

use std::process::Command;

fn check(name: &str, body: &str) -> (bool, String, String) {
    let src = format!(
        "fn main() {{
    let n: i64 = 4;
    let side: Vec[i64] = vec![1, 3];
    {body}
}}
"
    );
    let dir = std::env::temp_dir().join(format!("karac-rgread-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, &src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        src,
    )
}

#[test]
fn a_binding_moved_after_a_range_chain_that_reads_it_is_accepted() {
    for (name, body, want) in [
        (
            "collect",
            "let rest: Vec[i64] = (1..n + 1).filter(|i| not side.contains(i)).collect(); let g = Some((side, rest)); println(g.map_or(0, |p| p.0.len() + p.1.len()));",
            "4\n",
        ),
        (
            "count",
            "let k = (0..n).filter(|i| side.contains(i)).count(); let taken = side; println(f\"{k} {taken.len()}\");",
            "2 2\n",
        ),
        (
            "inclusive",
            "let s: i64 = (1..=n).map(|i| if side.contains(i) { i } else { 0 }).sum(); let taken = side; println(f\"{s} {taken.len()}\");",
            "4 2\n",
        ),
    ] {
        let (ok, err, src) = check(name, body);
        assert!(ok, "{name}: {err}");
        assert_eq!(
            karac::mir::lower::run_source(&src)
                .expect("MIR interpreter")
                .output,
            want,
            "{name}"
        );
    }
}

#[test]
fn a_range_chain_and_a_move_inside_one_loop_are_still_refused() {
    let (ok, err, _) = check(
        "loop",
        "for _r in 0..2 { let k = (0..n).filter(|i| side.contains(i)).count(); let taken = side; println(f\"{k} {taken.len()}\"); }",
    );
    assert!(!ok, "a move of `side` inside the loop must be refused");
    assert!(err.contains("side"), "{err}");
}
