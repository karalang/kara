//! Which `ParseError` an integer or float `parse` reports at the edges
//! (docs/library/core-types.md § parse, B-2026-11-13-21). `parse` ignores
//! whitespace around the number, so a whitespace-only input is `Empty`, as
//! `""` is, and `" -1 "` for `u8` is `Underflow`, as `"-1"` is; whitespace-only
//! was `InvalidDigit` for an integer and `Invalid` for a float. An integer
//! never reports `Invalid`: `u8.parse("-0")` and `"-00"` were `Invalid`, and
//! are `InvalidDigit`, since `-0` is no value below the minimum. The
//! whitespace is Unicode's, as `str::trim`'s is, and the check keeps std's
//! panic hook out of the binary (tests/mir_panic_free_natives.rs).

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn runs_to(tag: &str, src: &str, want: &str) {
    let path = std::env::temp_dir().join(format!(
        "karac-parse-error-edges-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, &path);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{tag} {lane:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_file(&path);
}

const SHOW: &str = "fn k[T](r: Result[T, ParseError]) -> String {\n    match r {\n        \
    Ok(_) => \"Ok\".to_string(),\n        Err(ParseError.Empty) => \"Empty\".to_string(),\n        \
    Err(ParseError.InvalidDigit) => \"InvalidDigit\".to_string(),\n        \
    Err(ParseError.Overflow) => \"Overflow\".to_string(),\n        \
    Err(ParseError.Underflow) => \"Underflow\".to_string(),\n        \
    Err(ParseError.Invalid) => \"Invalid\".to_string(),\n    }\n}\n";

#[test]
fn a_negative_zero_for_an_unsigned_type_is_an_invalid_digit() {
    let src = format!(
        "{SHOW}fn main() {{\n    println(k(u8.parse(\"-0\")));\n    println(k(u8.parse(\"-00\")));\n    \
         println(k(u64.parse(\"-0\")));\n    println(k(u8.parse(\"-5x\")));\n    \
         println(k(u8.parse(\"-1\")));\n    println(k(u8.parse(\"+0\")));\n    \
         println(k(i8.parse(\"-0\")));\n}}\n"
    );
    runs_to(
        "neg-zero",
        &src,
        "InvalidDigit\nInvalidDigit\nInvalidDigit\nInvalidDigit\nUnderflow\nOk\nOk\n",
    );
}

#[test]
fn whitespace_only_input_is_empty_and_surrounding_whitespace_is_ignored() {
    let src = format!(
        "{SHOW}fn main() {{\n    println(k(i64.parse(\"   \")));\n    println(k(i64.parse(\"\\n\")));\n    \
         println(k(u8.parse(\" \\t \")));\n    println(k(f64.parse(\"  \")));\n    \
         println(k(i64.parse(\"\")));\n    println(k(u8.parse(\" -1 \")));\n    \
         println(k(u8.parse(\" 300\\n\")));\n    println(k(i64.parse(\" 1x \")));\n    \
         println(k(i64.parse(\"\\u{{a0}}\\u{{3000}}\")));\n    println(k(u8.parse(\"\\u{{a0}}-1\")));\n}}\n"
    );
    runs_to(
        "whitespace",
        &src,
        "Empty\nEmpty\nEmpty\nEmpty\nEmpty\nUnderflow\nOverflow\nInvalidDigit\nEmpty\nUnderflow\n",
    );
}
