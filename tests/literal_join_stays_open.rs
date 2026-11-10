//! A join of literals is a literal (design.md §8 Literals): a `let` bound
//! to an `if`, `match` or block whose every value is an unsuffixed literal
//! is one open variable, as `let x = 1` is, and a later use fixes it.
//! `let x = if c { 1 } else { 0 }; take_i32(x)` was refused as narrowing
//! an `i64` (B-2026-11-10-49, the binding half of B-2026-11-10-6). Where an
//! earlier use fixed the join as `i64`, or an operator beside a narrower
//! type kept its default, it stays refused, now with the binding's
//! explanation and fix.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-literal-join-open-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

/// The default executor (the JIT on an llvm build, the interpreter
/// otherwise) and the interpreter.
const LANES: [&[&str]; 2] = [&["run"], &["run", "--interp"]];

fn runs(tag: &str, src: &str, expected: &str) {
    let path = temp(tag, src);
    for lane in LANES {
        let out = karac(lane, &path);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            expected,
            "{tag} {lane:?}; stderr:\n{stderr}"
        );
        assert!(out.status.success(), "{tag} {lane:?}; stderr:\n{stderr}");
    }
    let _ = std::fs::remove_file(&path);
}

fn refused(tag: &str, src: &str, needle: &str) {
    let path = temp(tag, src);
    let out = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains(needle),
        "{tag}: {stderr}"
    );
}

#[test]
fn a_let_bound_join_of_literals_is_fixed_by_a_later_use() {
    runs("joins", JOINS, "1\n1\n1\n28\n-1\n1.5\n7\n");
}

#[test]
fn kata_1118_days_in_a_month_binds_its_leap_day_as_i32() {
    runs("kata-1118", KATA_1118, "29\n30\n");
}

#[test]
fn a_join_of_literals_in_a_list_literal_is_an_element_literal() {
    runs("list", LIST, "29\n[31, 29, 31]\n");
}

#[test]
fn a_join_an_earlier_use_fixed_as_i64_stays_refused() {
    refused(
        "fixed-i64",
        "fn take(n: i32) -> i32 {\n    n\n}\n\nfn main() {\n    let c = true;\n    \
         let x = if c { 1 } else { 0 };\n    let big: i64 = 2;\n    let s = x + big;\n    \
         println(s);\n    println(take(x));\n}\n",
        "from 'i64' to 'i32' would narrow",
    );
    // Beside a `u8` the join keeps its default (design.md §8 Literals), as
    // `let bit = 4` does, and the refusal says so with the binding's fix.
    refused(
        "or-into-u8",
        "fn main() {\n    let c = true;\n    let mut rods = [0u8; 3];\n    \
         let bit = match c { true => 1, false => 4 };\n    rods[0] |= bit;\n    println(rods[0]);\n}\n",
        "`bit` is an 'i64' because its literal kept the default",
    );
}

const JOINS: &str = r#"fn take(n: i32) -> i32 { n }
fn main() {
    let c = true;
    let x = if c { 1 } else { 0 };
    println(take(x));
    let z = { 1 };
    println(take(z));
    let mut rods = [0u8; 3];
    let bit = match c { true => 1, false => 4 };
    let bit2: u8 = bit;
    rods[0] |= bit2;
    println(rods[0]);
    let leap = false;
    let leap_day = if leap { 1 } else { 0 };
    let days: i32 = 28 + leap_day;
    println(days);
    let w = if c { -1 } else if leap { 2 } else { 3 };
    println(take(w));
    let f = if c { 1.5 } else { 2.0 };
    let g: f32 = f;
    println(g);
    let d = if c { 7 } else { 8 };
    println(d);
}
"#;

const KATA_1118: &str = r#"fn days(month: i32, leap: bool) -> i32 {
    let extra = if month == 2 { 0 } else { 2 };
    let leap_day = if leap { 1 } else { 0 };
    28 + extra + leap_day
}
fn main() {
    println(days(2, true));
    println(days(3, false));
}
"#;

const LIST: &str = r#"fn main() {
    let leap = true;
    let table = [31, if leap { 29 } else { 28 }, 31];
    let t: i32 = table[1];
    println(t);
    println(table);
}
"#;
