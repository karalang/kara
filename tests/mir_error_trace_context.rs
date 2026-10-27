//! An error return trace frame of a `?` on the library's `.context(msg)`
//! carries `msg` (design.md § Errors: "A site with `.context()` fills
//! `"context"`; a bare `?` leaves it `null`"), on the MIR interpreter, on
//! the built binary and in `karac run --output=json`. A program's own
//! method named `context` fills nothing. B-2026-10-25-44.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

const SRC: &str = r#"struct NotFound {
    id: i64,
}

impl Display for NotFound {
    fn to_string(ref self) -> String {
        f"no user {self.id}"
    }
}

impl Error for NotFound {}

struct Job {
    n: i64,
}

impl Job {
    fn context(ref self, msg: Str) -> Result[i64, AnyError] {
        if self.n > 5 {
            return Err(AnyError.from(i64.parse(msg).unwrap_err()));
        }
        Ok(self.n)
    }
}

fn find(id: i64) -> Result[i64, NotFound] {
    if id > 10 {
        return Err(NotFound { id: id });
    }
    Ok(id * 2)
}

fn load(id: i64) -> Result[i64, AnyError] {
    let v = find(id).context(f"while loading user {id}\nback\\slash")?;
    let w = Job { n: id }.context("x")?;
    Ok(v + w)
}

fn handle(id: i64) -> Result[i64, AnyError] {
    let v = load(id)?;
    Ok(v + 1)
}

fn main() -> Result[(), AnyError] {
    println(f"{handle(3)?}");
    let w = handle(ARG).context("in main")?;
    println(f"{w}");
    Ok(())
}
"#;

/// The trace of `handle(42)`: the `.context` sites carry their messages
/// (escaped as the text trace writes them), the bare `?` none.
const TRACE_42: &str = "Error: in main: while loading user 42\nback\\slash: no user 42\n\
Error return trace:\n  \
main.kara:34:13  find(id).context(f\"while loading user {id}\\nback\\\\slash\")?  context: while loading user 42\\nback\\\\slash\n  \
main.kara:40:13  load(id)?\n  \
main.kara:46:13  handle(ARG).context(\"in main\")?  context: in main\n";

/// The trace of `handle(7)`, which fails at the program's own `context`.
const TRACE_7: &str = "Error: in main: invalid digit found in string\n\
Error return trace:\n  \
main.kara:35:13  Job { n: id }.context(\"x\")?\n  \
main.kara:40:13  load(id)?\n  \
main.kara:46:13  handle(ARG).context(\"in main\")?  context: in main\n";

fn source(dir: &str, arg: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-trace-context-{dir}-{arg}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC.replace("ARG)", &format!("{arg})"))).unwrap();
    (dir, file)
}

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_HASH_SEED", "1");
    c
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// stderr with the program's path as `main.kara`, and the trace's `ARG`
/// as written.
fn stderr_of(o: &Output, file: &std::path::Path, arg: &str) -> String {
    String::from_utf8_lossy(&o.stderr)
        .replace(file.to_str().unwrap(), "main.kara")
        .replace(&format!("handle({arg})"), "handle(ARG)")
}

#[test]
fn mir_error_trace_frames_carry_their_context_messages() {
    for (arg, trace) in [("42", TRACE_42), ("7", TRACE_7)] {
        let (dir, file) = source("interp", arg);
        let out = karac()
            .args(["__mir-run", file.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", text(&out));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "10\n");
        assert_eq!(stderr_of(&out, &file, arg), trace, "interpreter, {arg}");

        let exe = dir.join("main");
        let built = karac()
            .args([
                "build",
                "--mir",
                file.to_str().unwrap(),
                "-o",
                exe.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&built.stderr).to_string();
        assert!(
            built.status.success() || err.contains("Linker failed"),
            "does not build: {err}"
        );
        if common::link_or_skip(if built.status.success() {
            Ok(())
        } else {
            Err(err)
        })
        .is_some()
        {
            let ran = Command::new(&exe).output().unwrap();
            assert_eq!(ran.status.code(), Some(1), "{}", text(&ran));
            assert_eq!(String::from_utf8_lossy(&ran.stdout), "10\n");
            assert_eq!(stderr_of(&ran, &file, arg), trace, "binary, {arg}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn mir_run_json_frames_carry_their_context_messages() {
    let (dir, file) = source("json", "42");
    let out = karac()
        .args(["run", "--output=json", file.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if stdout.is_empty() && common::link_or_skip(Err(text(&out))).is_none() {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let path = file.to_str().unwrap();
    let frame = |line: u32, expr: &str, context: &str| {
        format!("{{\"file\":\"{path}\",\"line\":{line},\"column\":13,\"expr\":{expr},\"context\":{context}}}")
    };
    let want = [
        frame(
            34,
            r#""find(id).context(f\"while loading user {id}\\nback\\\\slash\")?""#,
            r#""while loading user 42\nback\\slash""#,
        ),
        frame(40, r#""load(id)?""#, "null"),
        frame(46, r#""handle(42).context(\"in main\")?""#, r#""in main""#),
    ]
    .join(",");
    assert!(
        stdout.contains(&format!("{{\"error_return_trace\":[{want}]}}")),
        "{stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
