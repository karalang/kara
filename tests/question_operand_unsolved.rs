//! A `?` whose value a statement discards (`Err(1)?;`, `g()?;`) leaves its
//! operand's `Ok` type unsolved: nothing after it can fix that type. Check
//! refused it with the MIR builder's internal "an expression whose type does
//! not lower ... is not lowered yet". It now gets design.md § Type
//! inference's "cannot infer type parameter", at the operand
//! (B-2026-11-03-7; a sibling of B-2026-10-29-36).

use std::process::{Command, Stdio};

fn karac(tag: &str, cmd: &str, src: &str) -> (bool, String) {
    let path = std::env::temp_dir().join(format!(
        "karac-question-unsolved-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(path);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

const G: &str = "fn g[T]() -> Result[T, i64] { Err(2) }\n";

#[test]
fn a_discarded_question_operand_names_its_unsolved_type() {
    for (tag, body, at, fix) in [
        (
            "err-literal",
            "fn f() -> Result[i64, i64] { if true { Err(1)?; } Ok(0) }\n",
            ":2:40:",
            "to return the error, write `return Err(..)`",
        ),
        (
            "generic-call",
            "fn f() -> Result[i64, i64] { g()?; Ok(0) }\n",
            ":2:30:",
            "bind the result with a type annotation",
        ),
        (
            "par-for",
            "fn f() -> Result[i64, i64] {\n    par for i in 0..2 { if i == 1 { Err(1)?; } }\n    Ok(0)\n}\n",
            ":3:37:",
            "to return the error, write `return Err(..)`",
        ),
    ] {
        let src = format!("{G}{body}fn main() {{ println(f().is_err()); }}\n");
        let (ok, out) = karac(tag, "check", &src);
        assert!(!ok, "{tag}: check accepted it:\n{src}");
        assert!(
            out.contains(at)
                && out.contains(
                    "cannot infer type parameter 'T' of `Result[T, i64]`, this `?` operand's type"
                )
                && out.contains(fix)
                && !out.contains("not lowered"),
            "{tag}:\n{out}"
        );
        assert_eq!(out.matches("error[").count(), 1, "{tag}:\n{out}");
    }
}

#[test]
fn a_question_whose_operand_type_is_fixed_still_runs() {
    // Controls: an annotated operand, a `?` bound with an annotation, and
    // the `return Err(..)` the hint names.
    let src = format!(
        "{G}fn f() -> Result[i64, i64] {{ let e: Result[i64, i64] = Err(1); e?; Ok(0) }}\n\
         fn k() -> Result[i64, i64] {{ let v: i64 = g()?; Ok(v) }}\n\
         fn h() -> Result[i64, i64] {{ if true {{ return Err(1); }} Ok(0) }}\n\
         fn main() {{ println(f().is_err()); println(k().is_err()); println(h().is_err()); }}\n"
    );
    let (ok, out) = karac("controls", "check", &src);
    assert!(ok, "{out}");
    let (_, out) = karac("controls", "__mir-run", &src);
    assert_eq!(out, "true\ntrue\ntrue\n");
}

/// `Ok(1)?`: the operand's error type is what nothing fixes. Check said
/// "'?' cannot propagate error 'E' as 'i64': target is not a named type",
/// a conversion refusal for what is an unsolved parameter.
#[test]
fn an_ok_literal_question_names_its_unsolved_error_type() {
    for (tag, body) in [
        ("stmt", "fn f() -> Result[i64, i64] { Ok(1)?; Ok(0) }\n"),
        (
            "let",
            "fn f() -> Result[i64, i64] { let v = Ok(1)?; Ok(v) }\n",
        ),
    ] {
        let src = format!("{body}fn main() {{ println(f().is_err()); }}\n");
        let (ok, out) = karac(tag, "check", &src);
        assert!(!ok, "{tag}: check accepted it:\n{src}");
        assert!(
            out.contains(
                "cannot infer type parameter 'E' of `Result[i64, E]`, this `?` operand's type"
            ) && out.contains("`let r: Result[i64, i64] = ..; r?`")
                && !out.contains("not a named type")
                && !out.contains("not lowered"),
            "{tag}:\n{out}"
        );
        assert_eq!(out.matches("error[").count(), 1, "{tag}:\n{out}");
    }
}
