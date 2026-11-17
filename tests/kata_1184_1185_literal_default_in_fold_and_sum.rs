//! Two refusals with no reason and no fix, found by the Rust chain arms of
//! katas 1184 and 1185. Both are right to refuse: a literal that meets an
//! `i32` only through an operator keeps the `i64` default (design.md §8
//! Literals). What was missing is the reason and the literal's suffix,
//! which a literal bound by `let` already gets.
//!
//! B-2026-11-17-98 (kata 1184): `let (inside, outside) =
//! distance.iter().enumerate().fold((0, 0), ..)` over `i32` legs, then
//! `inside.min(outside)` returned as an `i32`. A name bound from a fold's
//! result is now bound to the seed's literal, as `let (a, b) = (0, 0)` is.
//!
//! B-2026-11-17-99 (kata 1185): `let days: i32 = (1971..year).map(|y| if
//! leap(y) { 366 } else { 365 }).sum() + [31, 28, ..].iter().take(..).sum() +
//! day`, once `karac fix` drops Rust's `sum::<i32>()` turbofishes. A
//! `sum` or `product` over a closure's literal result, or over an array of
//! literals, now names those literals and offers the suffix.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> (Output, String) {
    let path = std::env::temp_dir().join(format!(
        "karac-kata1184-1185-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let after = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    (out, after)
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

/// `check` refuses with `want` in the message; `karac fix` then writes
/// `fixed`, after which the program checks and prints `prints`.
fn refused_then_fixed(tag: &str, src: &str, want: &str, fixed: &str, prints: &str) {
    let (out, _) = karac(&format!("{tag}-check"), &["check"], src);
    assert!(!out.status.success(), "{tag}: {}", text(&out));
    assert!(text(&out).contains(want), "{tag}: {}", text(&out));
    let (out, after) = karac(&format!("{tag}-fix"), &["fix"], src);
    assert!(out.status.success(), "{tag}: {}", text(&out));
    assert!(after.contains(fixed), "{tag}: {after}");
    let (out, _) = karac(&format!("{tag}-run"), &["__mir-run"], &after);
    assert!(out.status.success(), "{tag}: {}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), prints, "{tag}");
}

#[test]
fn a_name_bound_from_a_fold_result_offers_the_seed_suffix() {
    refused_then_fixed(
        "fold-tuple",
        "fn bus(distance: Vec[i32], s: i64, d: i64) -> i32 {\n\
         let (inside, outside) = distance\n\
             .iter()\n\
             .enumerate()\n\
             .fold((0, 0), |(a, b), (i, x)| if (s..d).contains(i) { (a + x, b) } else { (a, b + x) });\n\
         inside.min(outside)\n\
         }\n\
         fn main() { println(f\"{bus([1, 2, 3, 4], 0, 2)}\"); }\n",
        "`inside` is an 'i64' because the literal it is bound to took the default (design.md §8 Literals); write that literal `0i32`",
        ".fold((0i32, 0i32),",
        "3\n",
    );
    refused_then_fixed(
        "fold-single",
        "fn total(v: Vec[i32]) -> i32 {\n\
         let t = v.iter().fold(0, |a, x| a + x);\n\
         t\n\
         }\n\
         fn main() { println(f\"{total([1, 2, 3])}\"); }\n",
        "`t` is an 'i64' because the literal it is bound to took the default",
        ".fold(0i32,",
        "6\n",
    );
}

#[test]
fn a_sum_over_literals_offers_their_suffix() {
    refused_then_fixed(
        "sum-closure-and-array",
        "fn leap(y: i32) -> bool { y % 4 == 0 and (y % 100 != 0 or y % 400 == 0) }\n\
         fn days(day: i32, month: i32, year: i32) -> i32 {\n\
         let n: i32 = (1971..year).map(|y| if leap(y) { 366 } else { 365 }).sum()\n\
             + [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31].iter().take(month - 1).sum()\n\
             + day;\n\
         n\n\
         }\n\
         fn main() { println(f\"{days(31, 8, 2019)}\"); }\n",
        "the `sum` is an 'i64' because the literals its closure returns took the default (design.md §8 Literals); write `366i32`, which types the closure's other literals too",
        "if leap(y) { 366i32 } else { 365 }",
        "17775\n",
    );
    refused_then_fixed(
        "sum-array",
        "fn days(m: i32) -> i32 {\n\
         let d: i32 = [31, 28, 31].iter().take(2).sum() + m;\n\
         d\n\
         }\n\
         fn main() { println(f\"{days(5)}\"); }\n",
        "the `sum` is an 'i64' because the literals of the array it walks took the default (design.md §8 Literals); write the first of them with the suffix `i32`, which types them all",
        "[31i32, 28, 31]",
        "64\n",
    );
}

#[test]
fn a_sum_literal_that_does_not_fit_gets_no_suffix() {
    let src = "fn f(n: i8) -> i8 {\n\
               let s: i8 = (0..3).map(|_| 300).sum() + n;\n\
               s\n\
               }\n\
               fn main() { println(f\"{f(1)}\"); }\n";
    let (out, _) = karac("no-fit", &["check"], src);
    assert!(!out.status.success());
    let t = text(&out);
    assert!(t.contains("would narrow or change sign"), "{t}");
    assert!(
        !t.contains("300i8") && !t.contains("is an 'i64' because"),
        "{t}"
    );
}
