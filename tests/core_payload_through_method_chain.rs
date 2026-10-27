//! A generic call fixed only by its payload learns it through methods that
//! keep the payload (B-2026-10-27-7): `let p: P = Json.deserialize(s)
//! .map_err(f)?;`, `.context(msg)?`, `.map_err(f).unwrap()` and `let r:
//! Result[P, String] = Json.deserialize(s).map_err(f);`. Each used to be
//! refused with "cannot infer type parameter 'T' of `Result[T,
//! DeserializeError]`", where binding the call first checked.

use std::process::Command;

/// `karac check` on `src`, then its MIR run: the run's output, or the
/// check's error lines.
fn check_and_run(tag: &str, src: &str) -> Result<String, String> {
    let dir =
        std::env::temp_dir().join(format!("karac-payload-chain-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    let errors: Vec<&str> = err.lines().filter(|l| l.starts_with("error")).collect();
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    let r = karac::mir::lower::run_source(src).unwrap_or_else(|e| panic!("{tag}: build: {e}"));
    Ok(r.output)
}

fn program(ret: &str, body: &str, show_err: &str) -> String {
    format!(
        "import std.json.{{Json}};\n\
#[derive(Serialize, Deserialize)]\n\
struct P {{ x: i64 }}\n\
fn go(s: String) -> Result[i64, {ret}] {{\n    {body}\n}}\n\
fn main() {{\n    match go(Json.serialize(P {{ x: 3 }})) {{\n        Ok(x) => println(f\"{{x}}\"),\n        Err(e) => println({show_err}),\n    }}\n}}\n"
    )
}

#[test]
fn core_payload_reaches_the_call_through_a_question_mark() {
    for (tag, ret, body, show) in [
        (
            "map_err",
            "String",
            "let p: P = Json.deserialize(s).map_err(|e| e.to_string())?;\n    Ok(p.x)",
            "e",
        ),
        (
            "context",
            "AnyError",
            "let p: P = Json.deserialize(s).context(\"reading P\")?;\n    Ok(p.x)",
            "e.to_string()",
        ),
    ] {
        let src = program(ret, body, show);
        assert_eq!(check_and_run(tag, &src), Ok("3\n".to_string()), "{tag}");
    }
}

#[test]
fn core_payload_reaches_the_call_through_unwrap_and_a_binding() {
    let body = "let p: P = Json.deserialize(s).map_err(|e| e.to_string()).unwrap();\n    \
                let r: Result[P, String] = Json.deserialize(s).map_err(|e| e.to_string());\n    \
                Ok(p.x + r.unwrap().x)";
    let src = program("String", body, "e");
    assert_eq!(check_and_run("unwrap", &src), Ok("6\n".to_string()));
}
