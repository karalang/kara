//! A trait default body copied into two impls keeps the trait's spans in
//! both copies, so a call in it can borrow in one copy and consume in the
//! other at one span. `karac check` resolved the call and judged the
//! consume site per copy only through span-keyed tables, so the borrowing
//! copy hid the moving copy's use after the move (B-2026-10-28-11): with
//! `Keep.take(s: own String)` and `Look.take(s: ref String)`, `s.len()`
//! after `t.take(s)` is a use after a move in `A`'s copy, whichever impl
//! comes last.

use std::process::Command;

const HEAD: &str = "struct Keep {}
impl Keep { fn take(ref self, s: own String) -> i64 { return s.len(); } }
struct Look {}
impl Look { fn take(ref self, s: ref String) -> i64 { return s.len() * 2; } }

trait Use[T] {
    fn tool(ref self) -> T;
    fn run(ref self) -> i64 {
        let t = self.tool();
        let s = \"abc\";
        let n = t.take(s);
        return n + s.len();
    }
}
";

const KEEP: &str = "struct A {}
impl Use[Keep] for A { fn tool(ref self) -> Keep { return Keep {}; } }
";

const LOOK: &str = "struct B {}
impl Use[Look] for B { fn tool(ref self) -> Look { return Look {}; } }
";

const MAIN: &str = "fn main() {
    println(B {}.run());
    println(A {}.run());
}
";

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-copymoves-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn assert_moved(name: &str, src: &str) {
    let (ok, err) = check(name, src);
    assert!(!ok, "check must refuse:\n{src}");
    let want = "main.kara:12:20: value 's' moved here, used again here (moved at line 11:24)";
    assert!(err.contains(want), "want {want:?} in\n{err}");
}

#[test]
fn a_move_in_one_copy_of_a_default_body_is_refused_with_the_borrowing_impl_last() {
    assert_moved("look-last", &format!("{HEAD}{KEEP}{LOOK}{MAIN}"));
}

#[test]
fn a_move_in_one_copy_of_a_default_body_is_refused_with_the_borrowing_impl_first() {
    assert_moved("look-first", &format!("{HEAD}{LOOK}{KEEP}{MAIN}"));
}

#[test]
fn a_default_body_that_borrows_in_every_copy_checks_and_runs() {
    let src = format!("{HEAD}{KEEP}{LOOK}{MAIN}").replace(
        "fn take(ref self, s: own String) -> i64 { return s.len(); }",
        "fn take(ref self, s: ref String) -> i64 { return s.len(); }",
    );
    let (ok, err) = check("borrows", &src);
    assert!(ok, "{err}");
    let out = karac::mir::lower::run_source(&src).expect("MIR interpreter");
    assert_eq!(out.output, "9\n6\n");
}
