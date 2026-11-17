//! B-2026-11-17-97: a trait's default method may carry its own
//! `where Self: Bound`. The body is checked under the bound, an implementor
//! that misses it still implements the trait and lacks only that method, and
//! a call on one is refused naming the bound. `Iterator.cycle` is such a
//! method (`where Self: Clone`), so a program's own `Clone` iterator cycles;
//! until this row `cycle` was an inherent method on each library adaptor and
//! `where Self: Clone` did not parse ("'Self' is a reserved keyword").

use std::path::PathBuf;
use std::process::{Command, Output};

fn karac(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn scratch(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-selfbound-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

/// `karac check`'s stderr and exit code.
fn check(tag: &str, src: &str) -> (String, Option<i32>) {
    let f = scratch(tag, src);
    let out = karac(&["check", f.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
    (text(&out.stderr), out.status.code())
}

/// `karac run --interp`'s stdout, asserting success.
fn run(tag: &str, src: &str) -> String {
    let f = scratch(tag, src);
    let out = karac(&["run", "--interp", f.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
    assert!(out.status.success(), "{tag}: {}", text(&out.stderr));
    text(&out.stdout)
}

const COUNTDOWN: &str = "struct Countdown {\n    n: i64,\n}\n\n\
impl Iterator for Countdown {\n    type Item = i64;\n\n    \
fn next(mut ref self) -> Option[i64] {\n        if self.n == 0 {\n            \
return None;\n        }\n        self.n -= 1;\n        Some(self.n)\n    }\n}\n\n";

const TWICE: &str = "trait Named {\n    fn name(self) -> String;\n\n    \
fn twice(own self) -> (Self, Self) where Self: Clone {\n        \
let c = self.clone();\n        (c, self)\n    }\n}\n\n";

#[test]
fn a_program_clone_iterator_cycles() {
    let src = format!(
        "#[derive(Clone)]\n{COUNTDOWN}fn main() {{\n    \
let a: Vec[i64] = Countdown {{ n: 3 }}.cycle().take(7).collect();\n    \
println(f\"{{a}}\");\n}}\n"
    );
    assert_eq!(run("clone_iter", &src), "[2, 1, 0, 2, 1, 0, 2]\n");
}

#[test]
fn a_program_iterator_that_is_not_clone_has_no_cycle() {
    let src = format!(
        "{COUNTDOWN}fn main() {{\n    \
let a: Vec[i64] = Countdown {{ n: 3 }}.cycle().take(7).collect();\n    \
println(f\"{{a}}\");\n}}\n"
    );
    let (err, code) = check("plain_iter", &src);
    assert_eq!(code, Some(1), "{err}");
    assert!(
        err.contains("no method 'cycle' on type 'Countdown'")
            && err.contains("`where Self: Clone`")
            && err.contains("#[derive(Clone)]"),
        "{err}"
    );
    // The call stops at `cycle`: `take` does not report the same bound.
    assert_eq!(err.matches("error[").count(), 1, "{err}");
}

#[test]
fn a_generic_iterator_cycles_only_under_a_clone_bound() {
    let ok = format!(
        "#[derive(Clone)]\n{COUNTDOWN}\
fn firsts[I: Iterator[Item = i64] + Clone](it: own I) -> Vec[i64] {{\n    \
it.cycle().take(3).collect()\n}}\n\n\
fn main() {{\n    let a = firsts(Countdown {{ n: 2 }});\n    println(f\"{{a}}\");\n}}\n"
    );
    assert_eq!(run("generic_ok", &ok), "[1, 0, 1]\n");
    let unbounded = ok.replace("[Item = i64] + Clone]", "[Item = i64]]");
    let (err, code) = check("generic_unbounded", &unbounded);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("write `I: Clone`"), "{err}");
}

/// A program trait's default method with its own bound: the `Clone`
/// implementor calls it, the other implements the trait all the same.
#[test]
fn a_trait_default_method_carries_its_own_self_bound() {
    let src = format!(
        "{TWICE}#[derive(Clone)]\nstruct Tag {{\n    s: String,\n}}\n\n\
impl Named for Tag {{\n    fn name(self) -> String {{\n        self.s.clone()\n    }}\n}}\n\n\
struct Plain {{\n    s: String,\n}}\n\n\
impl Named for Plain {{\n    fn name(self) -> String {{\n        self.s.clone()\n    }}\n}}\n\n\
fn main() {{\n    let (x, y) = Tag {{ s: \"t\".to_string() }}.twice();\n    \
println(f\"{{x.name()}}{{y.name()}}\");\n    \
println(Plain {{ s: \"p\".to_string() }}.name());\n}}\n"
    );
    assert_eq!(run("trait_ok", &src), "tt\np\n");
}

#[test]
fn a_call_on_an_implementor_that_misses_the_bound_is_refused() {
    let src = format!(
        "{TWICE}struct Plain {{\n    s: String,\n}}\n\n\
impl Named for Plain {{\n    fn name(self) -> String {{\n        self.s.clone()\n    }}\n}}\n\n\
fn main() {{\n    let (x, y) = Plain {{ s: \"p\".to_string() }}.twice();\n    \
println(x.name());\n    println(y.name());\n}}\n"
    );
    let (err, code) = check("trait_unmet", &src);
    assert_eq!(code, Some(1), "{err}");
    assert!(
        err.contains("needs `Self: Clone`")
            && err.contains("`Plain` does not implement `Clone`")
            && err.contains("#[derive(Clone)]"),
        "{err}"
    );
    // Not the body's `self.clone()`, which the bound covers.
    assert!(!err.contains("no method 'clone'"), "{err}");
    assert!(!err.contains("reserved keyword"), "{err}");
}

/// `Self` outside a method still names nothing.
#[test]
fn a_free_function_where_self_is_still_refused() {
    let (err, code) = check(
        "free_fn",
        "fn f(x: i64) -> i64 where Self: Clone {\n    x\n}\n\nfn main() {\n    println(f(1));\n}\n",
    );
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("unknown type parameter 'Self'"), "{err}");
}
