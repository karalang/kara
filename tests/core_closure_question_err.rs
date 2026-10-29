//! A closure whose error type comes only from a `?` inside it: the `?`
//! demands solve the closure's `Result[_, E]`, and an `if` or `match` whose
//! arms are `Ok(..)` takes that solved type. The `if` kept the unsolved `E`,
//! so `check` passed and the MIR build refused "an expression whose type
//! does not lower (Type(UnknownParam("E")))" (B-2026-10-28-23).

use std::process::Command;

const NUM: &str = "fn num(s: Str) -> Result[i64, String] {
    match i64.parse(s) {
        Ok(n) => Ok(n),
        Err(_) => Err(f\"bad number '{s}'\"),
    }
}
";

fn check_and_run(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-closureq-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn a_closure_s_if_of_ok_arms_takes_the_error_type_of_its_question_mark() {
    let src = format!(
        "{NUM}
fn run(a: Str, b: Str) -> Result[i64, String] {{
    let get = |s: Str| {{
        if s.len() > 0 {{ Ok(Some(num(s)?)) }} else {{ Ok(None) }}
    }};
    let x = get(a)? ?? 0;
    let y = get(b)? ?? 0;
    Ok(x + y)
}}

fn main() {{
    match run(\"2\", \"\") {{
        Ok(n) => println(n),
        Err(e) => println(e),
    }}
    match run(\"x\", \"1\") {{
        Ok(n) => println(n),
        Err(e) => println(e),
    }}
}}
"
    );
    assert_eq!(check_and_run("if", &src), "2\nbad number 'x'\n");
}

#[test]
fn a_closure_s_match_of_ok_arms_takes_the_error_type_of_its_question_mark() {
    let src = format!(
        "{NUM}
fn main() {{
    let g = |s: Str| {{
        match s.len() {{
            0 => Ok(0),
            _ => Ok(num(s)? * 2),
        }}
    }};
    println(g(\"21\").unwrap());
    match g(\"q\") {{
        Ok(n) => println(n),
        Err(e) => println(e),
    }}
}}
"
    );
    assert_eq!(check_and_run("match", &src), "42\nbad number 'q'\n");
}
