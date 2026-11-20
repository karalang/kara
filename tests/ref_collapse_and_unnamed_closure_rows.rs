//! Two refusals whose text named the wrong reason:
//! - B-2026-11-20-6: a generic `mut ref T` at a reference `T` is that
//!   reference (a borrow of a borrow is the borrow; docs/deferred.md
//!   § References to References), so `setv(mut p, q)` with `p`, `q` both
//!   `ref String` is refused. The refusal says so, once, in place of
//!   "parameter expects `ref String` (not a mutable borrow)" and §5.9's
//!   "`p` is a shared `ref`".
//! - B-2026-11-20-7: a move of a place that a closure no `let` names still
//!   borrows (`let f = keep(|| s.len()); drop_it(s);`) is that, not "moved
//!   inside a loop" in a program with no loop.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-ref-collapse-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn check(tag: &str, src: &str) -> (bool, String) {
    let out = karac(tag, &["check"], src);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

const SETV: &str = "fn setv[T](slot: mut ref T, x: own T) { *slot = x; }\n";

#[test]
fn generic_mut_ref_at_a_reference_names_the_collapse_once() {
    let src = format!(
        "{SETV}fn main() {{ let a = \"a\".to_string(); let b = \"b\".to_string(); \
         let mut p = ref a; let q = ref b; setv(mut p, q); println(p); }}\n"
    );
    let (ok, diag) = check("collapse", &src);
    assert!(!ok, "{diag}");
    assert!(
        diag.contains("error[E0219]")
            && diag.contains(
                "`T` is `ref String` here, so the parameter `mut ref T` is a `ref String`: \
             a borrow of a borrow is the borrow"
            )
            && diag.contains("reassign `p` directly"),
        "{diag}"
    );
    assert!(!diag.contains("not a mutable borrow"), "{diag}");
    assert!(!diag.contains("is a shared `ref`"), "{diag}");
    assert_eq!(diag.matches("error[").count(), 1, "{diag}");
}

#[test]
fn generic_mut_ref_at_an_owned_type_still_writes() {
    let src = format!("{SETV}fn main() {{ let mut p = 1; setv(mut p, 5); println(p); }}\n");
    let (ok, diag) = check("owned", &src);
    assert!(ok, "{diag}");
    let run = karac("owned-run", &["__mir-run"], &src);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "5\n");
}

const KEEP: &str = "fn keep[F: Fn() -> i64](f: own F) -> F { f }\nfn drop_it(s: own String) { }\n";

#[test]
fn move_under_an_unnamed_closure_names_the_closure_not_a_loop() {
    for (tag, tail) in [("used", "println(f());"), ("unused", "")] {
        let src = format!(
            "{KEEP}fn main() {{ let s = \"abc\".to_string(); let f = keep(|| s.len() as i64); \
             drop_it(s); {tail} }}\n"
        );
        let (ok, diag) = check(tag, &src);
        assert!(!ok, "{tag}: {diag}");
        assert!(
            diag.contains("cannot move 's' (moved at line 3:")
                && diag.contains("while a closure still borrows it"),
            "{tag}: {diag}"
        );
        assert!(!diag.contains("moved inside a loop"), "{tag}: {diag}");
    }
}

#[test]
fn move_under_an_unnamed_closure_in_a_loop_keeps_the_loop_text() {
    let src = format!(
        "{KEEP}fn main() {{ let s = \"abc\".to_string(); let mut i = 0; \
         while i < 2 {{ let f = keep(|| s.len() as i64); drop_it(s); i += 1; }} }}\n"
    );
    let (ok, diag) = check("loop", &src);
    assert!(!ok, "{diag}");
    assert!(diag.contains("moved inside a loop"), "{diag}");
}
