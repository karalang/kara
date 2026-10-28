//! A dotted variant pattern names the variant of the enum before its last
//! segment (design.md § Enums), so one spelled through an enum other than
//! the scrutinee's never matches, and `karac check` refuses it
//! (B-2026-10-27-27). Only the last segment used to be read: `Other.Slow`
//! against a `Mode` took `Mode.Slow`'s arm, and `Other.P(x)` bound `Mode.P`'s
//! payload.

use std::path::PathBuf;
use std::process::{Command, Output};

fn write(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-variant-root-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn check(tag: &str, src: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(write(tag, src))
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

const ENUMS: &str =
    "enum Mode { Slow, P(i64), S { a: i64 } }\nenum Other { Slow, P(i64), S { a: i64 } }\n";

fn refused(tag: &str, arm: &str, scrutinee: &str, written: &str) {
    let src = format!(
        "{ENUMS}fn main() {{\n    match {scrutinee} {{\n        {arm} => println(\"other\"),\n        _ => println(\"mode\"),\n    }}\n}}\n"
    );
    let out = check(tag, &src);
    let err = text(&out.stderr);
    assert!(!out.status.success(), "{tag}: {err}");
    assert!(
        err.contains(&format!(
            "pattern `{written}` names a variant of `Other`, but the scrutinee has type `Mode`"
        )),
        "{tag}: {err}"
    );
}

#[test]
fn a_variant_pattern_through_another_enum_is_refused() {
    refused("unit", "Other.Slow", "Mode.Slow", "Other.Slow");
    refused("tuple", "Other.P(x)", "Mode.P(4)", "Other.P");
    refused("struct", "Other.S { a }", "Mode.S { a: 3 }", "Other.S");
}

/// The scrutinee's own enum, spelled through a type alias, is its own.
#[test]
fn a_variant_pattern_through_an_alias_of_the_scrutinee_enum_is_accepted() {
    let src = format!(
        "{ENUMS}type M2 = Mode;\nfn main() {{\n    match Mode.P(4) {{\n        M2.P(x) => println(f\"alias {{x}}\"),\n        \
         Mode.Slow | Mode.S {{ .. }} => println(\"mode\"),\n    }}\n}}\n"
    );
    let path = write("alias", &src);
    let out = check("alias", &src);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["run", "--interp"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "alias 4\n");
}
