//! B-2026-11-04-17 — design.md § `Iterator` and `Iterable`: "An `Iterator`
//! is consumed. `for x in it` moves `it`", and "To loop over an iterator
//! without consuming it, write `for x in it.by_ref()`". A `for` over a
//! borrowed iterator parameter bound the loop variable to the borrow
//! itself ("expected 'i64', found 'ref I'" at the first use of `x`), and a
//! concrete one reached MIR as "a `for` loop over a collection is not
//! lowered yet". It is
//! refused at `it`, and `karac fix` reaches a program that runs.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn program(param: &str, arg: &str) -> String {
    format!(
        "fn total{param} -> i64 {{\n    let mut t = 0;\n    for x in it {{\n        t += x;\n    }}\n    t\n}}\n\
         fn main() {{\n    println(total({arg}[1, 2, 3].into_iter()));\n}}\n"
    )
}

#[test]
fn a_for_over_a_borrowed_iterator_is_refused_and_fixed() {
    let cases = [
        (
            "[I: Iterator[Item = i64]](it: I)",
            "",
            "`it` is 'ref I', a borrowed iterator",
            "(it: own I)",
        ),
        (
            "(it: impl Iterator[Item = i64])",
            "",
            "`it` is 'ref impl Iterator[..]', a borrowed iterator",
            "(it: own impl Iterator[Item = i64])",
        ),
        (
            "(it: VecIntoIter[i64])",
            "",
            "`it` is 'ref VecIntoIter[i64]', a borrowed iterator",
            "(it: own VecIntoIter[i64])",
        ),
        (
            "[I: Iterator[Item = i64]](it: mut ref I)",
            "mut ",
            "loop over `it.by_ref()`",
            "for x in it.by_ref() {",
        ),
    ];
    for (i, (param, arg, said, fixed)) in cases.into_iter().enumerate() {
        let path = std::env::temp_dir().join(format!(
            "karac-borrowed-iter-{}-{i}.kara",
            std::process::id()
        ));
        let file = path.to_str().unwrap();
        std::fs::write(&path, program(param, arg)).unwrap();
        let check = karac(&["check", file]);
        let err = String::from_utf8_lossy(&check.stderr).to_string();
        assert!(!check.status.success(), "{param}: {err}");
        assert!(err.contains(":3:14:"), "refused at `it`: {err}");
        assert!(err.contains(said), "{param}: {err}");
        assert!(!err.contains("found 'ref"), "{param}: {err}");
        karac(&["fix", file]);
        let out = std::fs::read_to_string(&path).unwrap();
        let recheck = karac(&["check", file]);
        let run = karac(&["__mir-run", file]);
        let _ = std::fs::remove_file(&path);
        assert!(out.contains(fixed), "{param}: {out}");
        assert!(
            recheck.status.success(),
            "{param}: {}",
            String::from_utf8_lossy(&recheck.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&run.stdout), "6\n", "{out}");
    }
}
