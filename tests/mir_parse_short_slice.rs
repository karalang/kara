//! `i64.parse(date[5..7]).unwrap()` (B-2026-11-14-43, kata 1154) costs
//! what a digit loop costs. Three changes make it so, each pinned here:
//! the `Result` is `Ok` of the native's answer built in line, the library's
//! `parse_result_int` called only for text that holds no number (its
//! error walk had kept it out of line, a call frame on every parse); the
//! inline digit loop has one copy per sign, each starting at a constant,
//! so a constant length unrolls; and `s[lo..hi]`'s refused range ends in
//! the panic rather than joining the inline view, so `date[5..7]`'s length
//! is the constant 2. The answers, errors included, are the same on the
//! interpreter and the built binary.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const PARSES: &str = r#"fn md(d: Str) -> i64 {
    i64.parse(d[5..7]).unwrap()
}

fn show(r: Result[i64, ParseError]) -> String {
    match r { Ok(n) => f"ok {n}", Err(e) => f"err {e}" }
}

fn main() {
    for s in ["12", " -7 ", "", "  ", "x1", "99999999999999999999", "+5", "-", "-0"] {
        println(show(i64.parse(s)));
    }
    let t = "x-42y";
    println(show(i64.parse(t[1..4])));
    let a: Result[i8, ParseError] = i8.parse("-128");
    let b: Result[i8, ParseError] = i8.parse("128");
    let u: Result[u8, ParseError] = u8.parse("256");
    let v: Result[u8, ParseError] = u8.parse("-1");
    println(f"{a.unwrap()} {b.is_err()} {u.is_err()} {v.is_err()}");
    let f: Result[f64, ParseError] = f64.parse("1.5");
    let g: Result[i128, ParseError] = i128.parse("-170141183460469231731687303715884105728");
    println(f"{f.unwrap()} {g.unwrap()}");
    let d = "2019-02-10";
    println(md(d) + i64.parse(d[0..4]).unwrap() + i64.parse(d[8..10]).unwrap());
}
"#;

const WANT: &str = "ok 12\nok -7\nerr cannot parse a number from an empty string\n\
err cannot parse a number from an empty string\nerr invalid digit found in string\n\
err number too large to fit in target type\nok 5\nerr invalid digit found in string\nok 0\n\
ok -42\n-128 true true true\n1.5 -170141183460469231731687303715884105728\n2031\n";

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0");
    c
}

#[test]
fn mir_parse_result_is_ok_in_line() {
    let l = karac::mir::lower::build_source(PARSES).unwrap_or_else(|e| panic!("{e}"));
    let b = l
        .program
        .bodies
        .values()
        .find(|b| b.instance.name == "md")
        .expect("md");
    let md = karac::mir::pretty::pretty_body(b, &l.tys);
    assert!(md.contains("Result.Ok { copy (_"), "{md}");
    assert!(md.contains("parse_result_int[i64]("), "{md}");
}

#[test]
fn mir_parse_short_slice_answers_and_unrolls() {
    let d = std::env::temp_dir().join(format!("karac-parse-short-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let file = d.join("main.kara");
    std::fs::write(&file, PARSES).unwrap();
    let file = file.to_str().unwrap();
    let interp = karac().args(["__mir-run", file]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        WANT,
        "{}",
        String::from_utf8_lossy(&interp.stderr)
    );
    let exe = d.join("main");
    let ll = d.join("main.ll");
    let built = karac()
        .env("KARAC_MIR_LL", &ll)
        .args(["build", file, "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    })
    .is_none()
    {
        return;
    }
    let ran = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&ran.stdout), WANT);
    let ir = std::fs::read_to_string(&ll).unwrap();
    let md: Vec<&str> = ir
        .lines()
        .skip_while(|l| !(l.starts_with("define") && l.contains("@kara.md(")))
        .take_while(|l| *l != "}")
        .collect();
    assert!(!md.is_empty(), "no md in the IR");
    let md = md.join("\n");
    for label in ["dec.pos:", "dec.neg:"] {
        assert!(md.contains(label), "no {label} in md:\n{md}");
    }
    let slow: Vec<&str> = md
        .lines()
        .skip_while(|l| !l.starts_with("cut.slow:"))
        .take_while(|l| !l.is_empty())
        .collect();
    assert!(!slow.is_empty(), "no cut.slow in md:\n{md}");
    // Before, the block tested the runtime's answer and joined the view
    // on a 0 it never gives: `br i1 %r, label %ok, label %abort`.
    assert!(
        slow.last()
            .is_some_and(|l| l.trim_start().starts_with("br label %abort")),
        "a refused range can join the view:\n{}",
        slow.join("\n")
    );
    let _ = std::fs::remove_dir_all(&d);
}
