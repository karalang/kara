//! design.md §7 function kinds that `karac check` did not hold:
//! B-2026-11-04-67 (an `OnceFn` called twice, an erased `Fn` used after a
//! move), B-2026-11-04-62 (`let mut f = f;` on a non-escaping parameter) and
//! B-2026-11-04-65 (printing a capture reads it).

use std::process::{Command, Stdio};

fn check(tag: &str, src: &str) -> (bool, String) {
    let path = std::env::temp_dir().join(format!("karac-ck-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

const MK: &str = "fn mk() -> (Fn() -> i64) {\n    || 1\n}\n";

#[test]
fn a_closure_that_returns_its_capture_moves_it_in() {
    let (ok, diag) = check(
        "outer_after",
        "fn main() {\n    let s: String = \"a\";\n    let f = || s;\n    println(s);\n    \
         println(f());\n}\n",
    );
    assert!(!ok, "{diag}");
    assert!(diag.contains("moved into a closure"), "{diag}");
}

#[test]
fn an_erased_fn_value_moves() {
    for (tag, body) in [
        (
            "into_vec",
            "fn main() {\n    let h = mk();\n    let v = [h];\n    println(h());\n}\n",
        ),
        (
            "esc_param",
            "fn run(f: escaping Fn() -> i64) -> i64 {\n    let g = f;\n    f() + g()\n}\n\
             fn main() {\n    println(run(|| 1));\n}\n",
        ),
    ] {
        let (ok, diag) = check(tag, &format!("{MK}{body}"));
        assert!(!ok, "{tag}: {diag}");
        assert!(
            diag.contains("function value is move-only"),
            "{tag}: {diag}"
        );
    }
}

#[test]
fn copyable_function_values_and_borrows_stay_accepted() {
    for (tag, src) in [
        (
            "item",
            "fn dbl(x: i64) -> i64 { x * 2 }\nfn main() {\n    let f = dbl;\n    let g = f;\n    \
             println(f(1));\n    println(g(1));\n}\n"
                .to_string(),
        ),
        (
            "annotated_literal",
            "fn main() {\n    let f: Fn() -> i64 = || 1;\n    let g = f;\n    println(f());\n    \
             println(g());\n}\n"
                .to_string(),
        ),
        (
            "borrowed_twice",
            format!(
                "{MK}fn use2(f: Fn() -> i64) -> i64 {{ f() }}\nfn main() {{\n    let h = mk();\n    \
                 println(use2(h));\n    println(use2(h));\n    let r = ref h;\n    println(r());\n    \
                 println(h());\n}}\n"
            ),
        ),
        (
            "fn_param_prints_capture",
            "fn call2(f: Fn()) { f(); f(); }\nfn main() {\n    let s: String = \"abc\";\n    \
             call2(|| println(s));\n    println(s);\n}\n"
                .to_string(),
        ),
    ] {
        let (ok, diag) = check(tag, &src);
        assert!(ok, "{tag}: {diag}");
    }
}

#[test]
fn a_rebound_non_escaping_parameter_still_may_not_escape() {
    let (ok, diag) = check(
        "rebind_escape",
        "fn keep(f: Fn()) -> Fn() {\n    let g = f;\n    g\n}\nfn main() {}\n",
    );
    assert!(!ok, "{diag}");
    assert!(
        diag.contains("`g` holds parameter `f`, which is a non-escaping function value")
            && diag.contains("Declare it `f: escaping Fn(...)`"),
        "{diag}"
    );
}

#[test]
fn a_default_names_a_function_but_not_a_closure_literal() {
    let (ok, diag) = check(
        "default_fn",
        "fn id(x: i64) -> i64 { x }\nfn f(a: i64; g: Fn(i64) -> i64 = id) -> i64 { g(a) }\n\
         fn main() {\n    println(f(4));\n}\n",
    );
    assert!(ok, "{diag}");
    let (ok, diag) = check(
        "default_closure",
        "fn f(a: i64; g: Fn(i64) -> i64 = |x| x) -> i64 { g(a) }\nfn main() {\n    \
         println(f(4));\n}\n",
    );
    assert!(!ok, "{diag}");
}

/// B-2026-11-04-69: five design.md §7 errors that `karac check` accepted, and
/// two diagnostics that named the wrong rule.
#[test]
fn section7_refusals_name_their_rule() {
    for (tag, src, want) in [
        (
            "mutfn_plain_let",
            "fn main() {\n    let mut n = 0;\n    let f = || { n += 1; };\n    f();\n    \
             println(n);\n}\n",
            "calling it needs `let mut f`",
        ),
        (
            "oncefn_bare_param",
            "fn call(f: OnceFn() -> String) -> String { f() }\nfn main() {\n    \
             let s: String = \"a\";\n    println(call(|| s));\n}\n",
            "Declare the parameter `f: own OnceFn(...)`",
        ),
        (
            "ufcs_receiver_marker",
            "struct Counter { n: i64 }\nimpl Counter {\n    fn bump(mut ref self) { self.n += 1; }\n}\n\
             fn main() {\n    let mut c = Counter { n: 0 };\n    Counter.bump(c);\n    \
             println(c.n);\n}\n",
            "its receiver is marked",
        ),
        (
            "own_escaping",
            "fn h(f: own escaping Fn(i64)) { f(1); }\nfn main() {\n    h(|x| println(x));\n}\n",
            "`own` is not written beside `escaping`",
        ),
        (
            "refutable_param",
            "fn f(Some(x): Option[i64]) -> i64 { x }\nfn main() {\n    println(f(Some(1)));\n}\n",
            "a parameter's pattern must be irrefutable",
        ),
        (
            "fn_value_type_args",
            "fn srt[T](x: own T) -> T { x }\nfn main() {\n    let f = srt[i64];\n    \
             println(f(1));\n}\n",
            "a function takes no type arguments where it is used",
        ),
    ] {
        let (ok, diag) = check(tag, src);
        assert!(!ok, "{tag}: {diag}");
        assert!(diag.contains(want), "{tag}: {diag}");
    }
}

#[test]
fn section7_marked_forms_stay_accepted() {
    for (tag, src) in [
        (
            "mutfn_let_mut",
            "fn main() {\n    let mut n = 0;\n    let mut f = || { n += 1; };\n    f();\n    \
             println(n);\n}\n",
        ),
        (
            "oncefn_own_param",
            "fn call(f: own OnceFn() -> String) -> String { f() }\nfn main() {\n    \
             let s: String = \"a\";\n    println(call(|| s));\n}\n",
        ),
        (
            "ufcs_marked",
            "struct Counter { n: i64 }\nimpl Counter {\n    fn bump(mut ref self) { self.n += 1; }\n}\n\
             fn main() {\n    let mut c = Counter { n: 0 };\n    Counter.bump(mut c);\n    \
             println(c.n);\n}\n",
        ),
        (
            "escaping_alone",
            "fn h(f: escaping Fn(i64)) { f(1); }\nfn main() {\n    h(|x| println(x));\n}\n",
        ),
    ] {
        let (ok, diag) = check(tag, src);
        assert!(ok, "{tag}: {diag}");
    }
}
