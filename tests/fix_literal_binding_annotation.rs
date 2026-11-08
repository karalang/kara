//! B-2026-11-08-20: design.md §8 Literals lets an operator or a method leave a
//! binding's literal at its default `i64` (`let mut b = 0; b += y` with
//! `y: i32`), so a narrower use is refused where it meets one, far from the
//! `let`. The refusal names the binding and `karac fix` writes its
//! annotation, `let mut b: i32 = 0`.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn scratch(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

fn fix_until_clean(name: &str, src: &str) -> (String, String) {
    let file = scratch(name, src);
    for _ in 0..4 {
        let _ = karac(&["fix", &file]);
    }
    let fixed = std::fs::read_to_string(&file).unwrap();
    let check = karac(&["check", &file]);
    assert!(
        check.status.success(),
        "{name}: {}\n{fixed}",
        String::from_utf8_lossy(&check.stderr)
    );
    let _ = std::fs::remove_file(&file);
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    (fixed, out)
}

const PLUS: &str = "fn plus(y: i32) -> i32 {
    let mut b = 0;
    b += y;
    b
}

fn main() {
    println(plus(4));
}
";

#[test]
fn a_narrowing_from_a_defaulted_literal_names_the_binding() {
    let file = scratch("literal-binding-check", PLUS);
    let out = karac(&["check", "--output=json", &file]);
    let _ = std::fs::remove_file(&file);
    let json = String::from_utf8_lossy(&out.stdout);
    assert!(
        json.contains("`b` is an 'i64' because its literal kept the default"),
        "{json}"
    );
    assert!(
        json.contains("\"old\":\"    let mut b = 0;\",\"new\":\"    let mut b: i32 = 0;\""),
        "{json}"
    );
}

#[test]
fn karac_fix_annotates_each_defaulted_literal_binding() {
    let (fixed, out) = fix_until_clean("literal-binding-plus", PLUS);
    assert!(fixed.contains("let mut b: i32 = 0;"), "{fixed}");
    assert_eq!(out, "4\n");
    // Kata 1067's Rust template: `res` is refused at the return, then `p`
    // at the `+=` whose operand it widens.
    let src = "fn count(d: i32, n: i32) -> i32 {
    let mut res = 0;
    let mut p = 1;
    while p <= n {
        let (a, b) = (n / p, n % p);
        res += (a + 9 - d) / 10 * p;
        if a % 10 == d {
            res += b + 1;
        }
        p *= 10;
    }
    res
}

fn main() {
    println(count(1, 13));
}
";
    let (fixed, out) = fix_until_clean("literal-binding-count", src);
    assert!(fixed.contains("let mut res: i32 = 0;"), "{fixed}");
    assert!(fixed.contains("let mut p: i32 = 1;"), "{fixed}");
    assert_eq!(out, "6\n");
}

#[test]
fn a_literal_that_does_not_fit_gets_no_annotation_fix() {
    let src = "fn f(y: u8) -> u8 {
    let mut b = 300;
    b += y;
    b
}

fn main() {
    println(f(1));
}
";
    let file = scratch("literal-binding-unfit", src);
    let out = karac(&["check", "--output=json", &file]);
    let _ = std::fs::remove_file(&file);
    let json = String::from_utf8_lossy(&out.stdout);
    assert!(json.contains("would narrow"), "{json}");
    assert!(!json.contains("let mut b: u8"), "{json}");
}
