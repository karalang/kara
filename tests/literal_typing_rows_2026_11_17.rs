//! Literal typing rows from the kata threads (design.md §8 Literals):
//! - B-2026-11-17-130: an argument fixes a literal's type, and a `mut`
//!   argument is an argument: `let mut best = 0; bump(mut best);` with
//!   `bump(x: mut ref i32)` makes `best` an `i32`, as `ref i32` does.
//! - B-2026-11-17-129: a `fold` seed a method on the accumulator leaves at
//!   `i64` is refused naming the seed and the literals a `map` yields, and
//!   `karac fix` gives each the target's suffix, where the advice was a
//!   truncating `as`.

use karac::{check_source, run_playground};
use std::sync::atomic::{AtomicUsize, Ordering};

fn errors(src: &str) -> Vec<String> {
    check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect()
}

/// `src` after `karac fix`.
fn karac_fix(src: &str) -> String {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-littype-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.kara"), src).unwrap();
    let st = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["fix", "a.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let out = std::fs::read_to_string(dir.join("a.kara")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        st.status.success(),
        "{}",
        String::from_utf8_lossy(&st.stderr)
    );
    out
}

#[test]
fn a_mut_argument_fixes_a_literal_s_type() {
    for (sig, call) in [
        (
            "fn bump(x: mut ref i32) {\n    *x += 1;\n}\n",
            "bump(mut best)",
        ),
        ("fn bump(x: ref i32) -> i32 {\n    *x\n}\n", "bump(best)"),
    ] {
        let src = format!(
            "{sig}fn f() -> i32 {{\n    let mut best = 0;\n    {call};\n    best\n}}\n\
             fn main() {{\n    println(f\"{{f()}}\");\n}}\n"
        );
        assert!(errors(&src).is_empty(), "{src}\n{:#?}", errors(&src));
    }
    let out = run_playground(
        "fn bump(x: mut ref i32) {\n    *x += 1;\n}\nfn main() {\n    let mut best = 0;\n    \
         bump(mut best);\n    bump(mut best);\n    println(f\"{best}\");\n}\n",
    );
    assert!(out.ok, "{:#?}", out.diagnostics);
    assert_eq!(out.stdout.concat(), "2\n");
    // The parameter's range still holds the literal.
    let es = errors(
        "fn bump(x: mut ref u8) {\n    *x += 1;\n}\nfn main() {\n    let mut c = 300;\n    \
         bump(mut c);\n    println(f\"{c}\");\n}\n",
    );
    assert_eq!(es.len(), 1, "{es:#?}");
    assert!(
        es[0].contains("integer literal 300 out of range for 'u8'"),
        "{es:#?}"
    );
}

#[test]
fn a_fold_seed_left_at_i64_is_named_and_fixed() {
    for (body, names_map) in [
        ("v.iter().map(|x| 1).fold(0, |a, b| a.max(b))", true),
        ("v.iter().map(|x| 1).fold(0, |a, b| (a + b).max(0))", true),
        (
            "v.iter().map(|x| 1).fold(0, |a, b| { let s = a + b; s.max(0) })",
            true,
        ),
        (
            "v.iter().map(|x| if *x > 1 { -1 } else if *x == 1 { 0 } else { 1 })\
             .fold(0, |depth, step| (depth + step).max(0))",
            true,
        ),
        ("v.iter().map(|x| *x).fold(0, |a, _| a.max(0))", false),
    ] {
        let src = format!(
            "fn f(v: Vec[i64]) -> i32 {{\n    {body}\n}}\n\
             fn main() {{\n    println(f\"{{f([1, 2, 0])}}\");\n}}\n"
        );
        let es = errors(&src);
        assert_eq!(es.len(), 1, "{body}: {es:#?}");
        let want = if names_map {
            "the `fold` seed `0` and the literals the `map` yields kept the default 'i64'"
        } else {
            "the `fold` seed `0` kept the default 'i64'"
        };
        assert!(es[0].contains(want), "{body}: {}", es[0]);
        let out = karac_fix(&src);
        assert!(out.contains(".fold(0i32,"), "{body}: {out}");
        assert!(
            errors(&out).is_empty(),
            "{body}: {out}\n{:#?}",
            errors(&out)
        );
    }
}

#[test]
fn an_i64_fold_over_i64_items_does_not_blame_its_seed() {
    let es = errors(
        "fn f(v: Vec[i64]) -> i32 {\n    v.iter().map(|x| *x).fold(0, |a, b| a.max(b))\n}\n\
         fn main() {\n    println(f\"{f([1])}\");\n}\n",
    );
    assert_eq!(es.len(), 1, "{es:#?}");
    assert!(!es[0].contains("`fold` seed"), "{}", es[0]);
}
