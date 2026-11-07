//! design.md § Type inference (B-2026-11-06-7): a generic call's type
//! parameter that only its result names is a type variable a later use in
//! the body solves (`let mut h = PriorityQueue.new(); h.push(x)`). One that
//! nothing solves is still an error, at the call, and a bound on the impl a
//! later method call resolved to is still checked once the use has fixed it.

use std::path::PathBuf;
use std::process::Command;

fn check(tag: &str, src: &str) -> (bool, String) {
    let path: PathBuf =
        std::env::temp_dir().join(format!("karac-later-use-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

const BAG: &str = "struct Bag[T] {\n    items: Vec[T],\n}\n\nimpl[T] Bag[T] {\n    \
    fn new() -> Bag[T] {\n        Bag { items: Vec.new() }\n    }\n\n    \
    fn push(mut ref self, x: own T) {\n        self.items.push(x);\n    }\n}\n\n";

#[test]
fn a_push_after_the_call_fixes_its_type_parameter() {
    let src = format!(
        "{BAG}fn empty[T]() -> Vec[T] {{\n    Vec.new()\n}}\n\nfn main() {{\n    \
         let mut h = PriorityQueue.new();\n    h.push(3);\n    \
         let mut b = Bag.new();\n    b.push(\"x\");\n    \
         let mut v = empty();\n    v.push(1.5);\n    println(v);\n}}\n"
    );
    let (ok, text) = check("fixed", &src);
    assert!(ok, "{text}");
}

#[test]
fn a_call_s_type_parameter_nothing_fixes_is_still_refused() {
    let src = format!("{BAG}fn main() {{\n    let b = Bag.new();\n    println(1);\n}}\n");
    let (ok, text) = check("unfixed", &src);
    assert!(!ok, "{text}");
    assert!(
        text.contains(":16:13: cannot infer type parameter 'T' of `Bag[T]`"),
        "{text}"
    );
}

#[test]
fn an_impl_bound_is_checked_once_a_later_use_fixes_the_receiver() {
    let src = "fn main() {\n    let mut h = PriorityQueue.new();\n    h.push(1.5);\n}\n";
    let (ok, text) = check("bound", src);
    assert!(!ok, "{text}");
    assert!(
        text.contains(":3:5: method 'push' is not callable on this `PriorityQueue`")
            && text.contains("`f64` does not implement `Ord`"),
        "{text}"
    );
}

/// B-2026-11-06-11: the same for a binding of `None` / `Ok(..)`, a bare-`T`
/// result, and a callee parameter spelled like the caller's own.
#[test]
fn an_option_a_bare_result_and_a_same_named_parameter_are_fixed_later() {
    let src = format!(
        "{BAG}fn make[T: Default]() -> T {{\n    T.default()\n}}\n\n\
         fn g[T](x: T) {{\n    let mut b = Bag.new();\n    b.push(1);\n}}\n\n\
         fn main() {{\n    let mut best = None;\n    best = Some(3);\n    \
         let mut r = Ok(1);\n    r = Err(\"bad\");\n    \
         let x = make();\n    let y: i64 = x;\n    g(y);\n}}\n"
    );
    let (ok, text) = check("option", &src);
    assert!(ok, "{text}");
}

#[test]
fn a_none_nothing_fixes_is_still_refused() {
    let (ok, text) = check(
        "none",
        "fn main() {\n    let o = None;\n    println(1);\n}\n",
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains(":2:13: cannot infer type parameter 'T' of `Option[T]`"),
        "{text}"
    );
}

/// B-2026-11-06-15: design.md § Type inference, "An unsolved variable is an
/// error at the end of the body": a `Vec.new()` used only for `.len()`, and
/// a closure whose parameter nothing types.
#[test]
fn an_unsolved_constructor_and_an_untyped_closure_parameter_are_refused() {
    let (ok, text) = check(
        "unsolved-vec",
        "fn main() {\n    let v = Vec.new();\n    println(v.len());\n}\n",
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains(":2:13: cannot infer type parameter 'T' of `Vec[T]`"),
        "{text}"
    );
    let (ok, text) = check(
        "untyped-closure",
        "fn main() {\n    let f = |x| x;\n    let g = |x| x + 1;\n    println(g(2));\n}\n",
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains(":2:13: cannot infer closure parameter type"),
        "{text}"
    );
    assert_eq!(text.matches("cannot infer").count(), 1, "{text}");
}
