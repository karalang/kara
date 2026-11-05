//! B-2026-11-05-2: a `MutFn` local passed to an owned parameter moves, so a
//! second pass is a use after move (design.md §7, core-semantics §1.1).
//! B-2026-11-04-40: in a trait's default body copied into an impl, `self.m()`
//! is that trait's `m`, beside another trait's `m` on the same type.

use std::process::{Command, Stdio};

fn check(tag: &str, src: &str) -> (bool, String) {
    let path = std::env::temp_dir().join(format!("karac-mfa-{tag}-{}.kara", std::process::id()));
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

const TAKE: &str = "fn take(f: own MutFn()) { let mut f = f; f(); }\n";

#[test]
fn a_mutfn_passed_to_an_owned_parameter_twice_is_a_use_after_move() {
    for (tag, body) in [
        (
            "own_twice",
            "fn main() {\n    let mut n = 0;\n    let f = || { n += 1; };\n    take(f);\n    \
             take(f);\n    println(n);\n}\n",
        ),
        (
            "own_then_call",
            "fn main() {\n    let mut n = 0;\n    let mut f = || { n += 1; };\n    take(f);\n    \
             f();\n    println(n);\n}\n",
        ),
    ] {
        let (ok, diag) = check(tag, &format!("{TAKE}{body}"));
        assert!(!ok, "{tag}: {diag}");
        assert!(
            diag.contains("owned parameter `f` moved it"),
            "{tag}: {diag}"
        );
    }
}

#[test]
fn a_mutfn_moved_once_on_each_path_or_lent_is_accepted() {
    for (tag, body) in [
        (
            "once",
            "fn main() {\n    let mut n = 0;\n    let f = || { n += 1; };\n    take(f);\n    \
             println(n);\n}\n",
        ),
        (
            "branches",
            "fn main() {\n    let mut n = 0;\n    let c = n > 5;\n    let f = || { n += 1; };\n    \
             if c { take(f); } else { take(f); }\n    println(n);\n}\n",
        ),
        (
            "lent_then_moved",
            "fn twice(f: mut ref MutFn()) { f(); f(); }\nfn main() {\n    let mut n = 0;\n    \
             let mut f = || { n += 1; };\n    twice(mut f);\n    take(f);\n    println(n);\n}\n",
        ),
    ] {
        let (ok, diag) = check(tag, &format!("{TAKE}{body}"));
        assert!(ok, "{tag}: {diag}");
    }
}

const TRAITS: &str =
    "trait A {\n    fn m(self) -> i64;\n    fn d(self) -> i64 { self.m() + 10 }\n}\n\
                      trait B {\n    fn m(self) -> i64;\n}\nstruct S {}\n\
                      impl A for S { fn m(self) -> i64 { 1 } }\n\
                      impl B for S { fn m(self) -> i64 { 2 } }\n";

#[test]
fn a_copied_default_body_calls_its_own_traits_method() {
    let (ok, diag) = check(
        "default_self",
        &format!("{TRAITS}fn main() {{\n    let s = S {{}};\n    println(s.d());\n}}\n"),
    );
    assert!(ok, "{diag}");
}

#[test]
fn a_call_on_the_concrete_type_stays_ambiguous() {
    let (ok, diag) = check(
        "concrete_ambiguous",
        &format!("{TRAITS}fn main() {{\n    let s = S {{}};\n    println(s.m());\n}}\n"),
    );
    assert!(!ok, "{diag}");
    assert!(diag.contains("ambiguous method 'm'"), "{diag}");
}
