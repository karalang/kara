//! A closure handed to an adaptor chain whose value holds no closure
//! (`let n = h.iter().filter(|u| mask[..]).count();`) is called only while
//! its statement runs, so moving a binding it reads after the statement is
//! fine. The ownership pass paired the body's read with the later move as a
//! reuse ("moved inside a loop", with no loop in the program), as it did for
//! a `let`-bound closure before B-2026-10-29-37 (B-2026-10-29-43). An
//! iterator kept past the move, a move before the chain, and the pair inside
//! a loop are still refused.

use std::process::Command;

fn check(name: &str, body: &str) -> (bool, String, String) {
    let src = format!(
        "fn main() {{
    let mask: Vec[bool] = vec![true, false];
    let h: Vec[i64] = vec![1, 2, 3, 4];
    {body}
}}
"
    );
    let dir = std::env::temp_dir().join(format!("karac-adread-{}-{name}", std::process::id()));
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
fn a_binding_moved_after_an_adaptor_chain_that_reads_it_is_accepted() {
    for (name, body, want) in [
        (
            "count",
            "let n = h.iter().filter(|u| mask[*u % 2]).count(); let m2 = mask; println(f\"{n} {m2.len()}\");",
            "2 2\n",
        ),
        (
            "sum",
            "let s: i64 = h.iter().map(|u| if mask[*u % 2] { 1 } else { 0 }).sum(); let m2 = mask; println(f\"{s} {m2.len()}\");",
            "2 2\n",
        ),
        (
            "collect",
            "let v: Vec[i64] = h.iter().filter(|u| mask[*u % 2]).map(|u| *u).collect(); let m2 = mask; println(f\"{v.len()} {m2.len()}\");",
            "2 2\n",
        ),
        (
            "statement",
            "h.iter().for_each(|u| println(mask[*u % 2])); let m2 = mask; println(m2.len());",
            "false\ntrue\nfalse\ntrue\n2\n",
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
fn an_adaptor_closure_that_may_run_after_the_move_is_still_refused() {
    for (name, body) in [
        (
            "kept_iterator",
            "let it = h.iter().filter(|u| mask[*u % 2]); let m2 = mask; println(f\"{it.count()} {m2.len()}\");",
        ),
        (
            "moved_before",
            "let m2 = mask; let n = h.iter().filter(|u| mask[*u % 2]).count(); println(n);",
        ),
        (
            "in_a_loop",
            "let mut i = 0; while i < 2 { let n = h.iter().filter(|u| mask[*u % 2]).count(); let m2 = mask; println(n); i += 1; }",
        ),
    ] {
        let (ok, err, _) = check(name, body);
        assert!(!ok, "{name}: check must refuse");
        assert!(err.contains("mask"), "{name}: {err}");
    }
}
