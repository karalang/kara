//! The gaps kata 1056 (Confusing Number) found writing its arms the way Rust
//! writes them:
//!
//! - `std::iter::successors(..)` was fixed to `successors(..)`, which named
//!   nothing: the library now has it (library/iterators.md, Sources);
//! - `.try_fold(0, |acc, d| ...)` was "no method 'try_fold'": it now folds
//!   while the function returns `Some` or `Ok`, through the prelude's `Try`;
//! - `collect()` into an `Option[String]` was "`Option[String]` does not
//!   implement `FromIterator`": `Option[C]` and `Result[C, E]` now gather
//!   `Option` and `Result` items, and stop at the first `None` or `Err`;
//! - a method's closure whose result type only the `let` names
//!   (`let r: Result[i64, String] = it.try_fold(1, |a, x| Ok(a * x))`) was
//!   "a closure with no function type" in MIR: a method's bare generic
//!   return now takes the expectation, as a free function's does.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("karac-k1056-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

/// `check` succeeds and `__mir-run` prints `want`.
fn accepted(tag: &str, src: &str, want: &str) {
    let path = write(tag, src);
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        want,
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// `check` fails with `message`.
fn refused(tag: &str, src: &str, message: &str) {
    let path = write(tag, src);
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let _ = std::fs::remove_file(&path);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(!check.status.success(), "accepted:\n{src}");
    assert!(err.contains(message), "missing {message:?} in:\n{err}");
}

#[test]
fn successors_walks_until_its_function_gives_none() {
    accepted(
        "successors",
        r#"
fn main() {
    let digits: Vec[i64] = successors(Some(4096), |x| if x >= 10 { Some(x / 10) } else { None }).collect();
    println(f"{digits}");
    let words: Vec[String] = successors(Some("a".to_string()), |w| if w.len() < 3 { Some(w.clone() + "b") } else { None }).collect();
    println(f"{words}");
    let none: Vec[i64] = successors(None, |x: ref i64| Some(x + 1)).collect();
    println(f"{none.len()}");
}
"#,
        "[4096, 409, 40, 4]\n[a, ab, abb]\n0\n",
    );
}

#[test]
fn a_program_s_own_successors_keeps_the_name() {
    accepted(
        "own-successors",
        r#"
fn successors(n: i64) -> i64 {
    n + 1
}

fn main() {
    println(f"{successors(1)}");
}
"#,
        "2\n",
    );
}

#[test]
fn try_fold_stops_at_the_first_none_or_err() {
    accepted(
        "try-fold",
        r#"
fn main() {
    println(f"{[1, 2, 3].iter().try_fold(0, |acc, x| acc.checked_add(x))}");
    println(f"{[i64.MAX, 1].iter().try_fold(0, |acc, x| acc.checked_add(x))}");
    let mut seen = 0;
    let t = (0..100).try_fold(0, |acc, x| { seen += 1; if x < 4 { Some(acc + x) } else { None } });
    println(f"{t} after {seen}");
    let r: Result[i64, String] = (1..6).try_fold(1, |acc, x| if x < 5 { Ok(acc * x) } else { Err(f"stop at {x}") });
    println(f"{r}");
    let r: Result[i64, String] = (1..5).try_fold(1, |acc, x| Ok(acc * x));
    println(f"{r}");
}
"#,
        "Some(6)\nNone\nNone after 5\nErr(stop at 5)\nOk(24)\n",
    );
}

#[test]
fn collect_into_option_or_result_stops_at_the_first_none_or_err() {
    accepted(
        "collect-option",
        r#"
fn lengths(words: ref Vec[String]) -> Result[Vec[i64], String] {
    words.iter().map(|w| if w.len() < 3 { Ok(w.len()) } else { Err(f"too long: {w}") }).collect()
}

fn main() {
    let turned: Option[String] = "6089".chars().rev().map(|c| match c {
        '0' => Some('0'),
        '6' => Some('9'),
        '8' => Some('8'),
        '9' => Some('6'),
        _ => None,
    }).collect();
    println(f"{turned}");
    let mut pulled = 0;
    let some: Option[Vec[i64]] = (0..10).map(|x| { pulled += 1; if x < 3 { Some(x) } else { None } }).collect();
    println(f"{some} after {pulled}");
    let set: Option[Set[i64]] = [3, 1, 3].iter().map(|x| Some(*x)).collect();
    println(f"{set.unwrap().len()}");
    let map: Option[SortedMap[i64, i64]] = [3, 1, 3].iter().map(|x| Some((*x, *x * 2))).collect();
    println(f"{map}");
    println(f"{lengths(["ab".to_string(), "c".to_string()])}");
    println(f"{lengths(["ab".to_string(), "cdef".to_string()])}");
    let numbers: Result[Vec[i64], ParseError] = ["12", "x7"].iter().map(|w| i64.parse(w)).collect();
    println(f"{numbers.is_err()}");
}
"#,
        "Some(6809)\nNone after 4\n2\nSome({1: 2, 3: 6})\nOk([2, 1])\nErr(too long: cdef)\ntrue\n",
    );
}

#[test]
fn borrowed_values_inside_some_do_not_make_an_owned_collection() {
    refused(
        "collect-borrowed",
        r#"
fn main() {
    let o: Option[Vec[i64]] = [3, 1, 3].iter().map(|x| Some(x)).collect();
    println(f"{o}");
}
"#,
        "`Option[Vec[i64]]` gathers each `Some`'s value into `Vec[i64]`, which implements `FromIterator[i64]`, not `FromIterator[ref i64]`; each item is a borrowed value, so put a copy in the `Some`: `Some(*x)`",
    );
    refused(
        "collect-wrong-payload",
        r#"
fn main() {
    let r: Result[Vec[i64], String] = [1, 2].iter().map(|x| if *x > 0 { Ok(*x > 1) } else { Err("no".to_string()) }).collect();
    println(f"{r}");
}
"#,
        "not `FromIterator[bool]`",
    );
}

#[test]
fn a_method_s_closure_takes_its_result_type_from_the_let() {
    accepted(
        "method-ret",
        r#"
struct Pair {
    v: i64,
}

impl Pair {
    fn call[A, R](ref self, init: own A, f: own MutFn(own A, own i64) -> R) -> R {
        f(init, self.v)
    }
}

fn main() {
    let p = Pair { v: 2 };
    let r: Result[i64, String] = p.call(1, |x, y| Ok(x + y));
    println(f"{r}");
}
"#,
        "Ok(3)\n",
    );
}
