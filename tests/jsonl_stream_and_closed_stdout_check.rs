//! B-2026-11-05-13: `karac check --output=jsonl` streams every diagnostic the
//! batch `--output=json` document lists, as `diagnostic` events with the same
//! ids, and its counts, `build_id` and `timestamp` are real (design.md
//! § Streaming phase events).
//! B-2026-11-05-9 (and -13's last item): a closed stdout (`karac test | head`,
//! `karac check --output=jsonl | head -1`) ends the run quietly, with no Rust
//! panic; and `assert_eq` refuses operands that cannot print (design.md §14
//! Assertions: `assert_eq[T: PartialEq + Debug]`).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch(tag: &str, src: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("karac-jls-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

fn check(path: &Path, output: &str) -> (Option<i32>, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.arg("check");
    if output != "text" {
        cmd.arg(format!("--output={output}"));
    }
    let out = cmd.arg(path).stdin(Stdio::null()).output().expect("karac");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The value of `"key":` in one JSON line: a string's contents, or the raw
/// token up to the next `,` or `}`.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = &line[line.find(&format!("\"{key}\":"))? + key.len() + 3..];
    match rest.strip_prefix('"') {
        Some(s) => s.split('"').next(),
        None => rest.split([',', '}']).next(),
    }
}

/// A type error, a lint warning and two ownership errors: three phases' worth.
const MIXED: &str = "fn take(v: own Vec[i32]) {}\n\
fn main() {\n\
    let x: i32 = \"a\";\n\
    let v: Vec[i32] = Vec.new();\n\
    take(v);\n\
    take(v);\n\
    let w: Vec[i32] = Vec.new();\n\
    take(w);\n\
    take(w);\n\
    println(x);\n\
}\n";

#[test]
fn jsonl_streams_every_batch_diagnostic_with_its_id_and_phase() {
    let path = scratch("mixed", MIXED);
    let (bcode, batch, _) = check(&path, "json");
    let (lcode, stream, lerr) = check(&path, "jsonl");
    let _ = std::fs::remove_file(&path);
    assert_eq!(bcode, Some(1), "{batch}");
    assert_eq!(lcode, Some(1), "{stream}{lerr}");

    let lines: Vec<&str> = stream.lines().collect();
    assert!(
        lines.iter().all(|l| l.starts_with("{\"type\":")),
        "every line is a typed event:\n{stream}"
    );
    let diagnostics: Vec<String> = lines
        .iter()
        .filter(|l| l.starts_with("{\"type\":\"diagnostic\","))
        .map(|l| format!("{{{}", &l["{\"type\":\"diagnostic\",".len()..]))
        .collect();
    // The batch document's `diagnostics` array, entry for entry.
    let array = &batch[batch.find("\"diagnostics\":[").unwrap() + "\"diagnostics\":".len()..];
    let array = array.trim_end().strip_suffix('}').unwrap();
    assert_eq!(
        format!("[{}]", diagnostics.join(",")),
        array,
        "the stream's diagnostics are the batch document's"
    );
    let phases: Vec<&str> = diagnostics
        .iter()
        .filter_map(|d| field(d, "phase"))
        .collect();
    assert!(phases.contains(&"typecheck"), "{stream}");
    assert!(phases.contains(&"ownership"), "{stream}");

    // Each phase's diagnostics come out at its boundary, before its
    // `phase_complete`, whose error count is theirs.
    let mut errors_by_count = 0;
    let mut pending: Vec<&str> = Vec::new();
    for l in &lines {
        if l.starts_with("{\"type\":\"diagnostic\",") {
            pending.push(l);
        } else if l.starts_with("{\"type\":\"phase_complete\",") {
            let phase = field(l, "phase").unwrap();
            let errors: usize = field(l, "errors").unwrap().parse().unwrap();
            let warnings: usize = field(l, "warnings").unwrap().parse().unwrap();
            assert!(
                pending.iter().all(|d| field(d, "phase") == Some(phase)),
                "{phase}'s diagnostics are its own:\n{stream}"
            );
            let count = |s: &str| {
                pending
                    .iter()
                    .filter(|d| field(d, "severity") == Some(s))
                    .count()
            };
            assert_eq!(errors, count("error"), "{phase}:\n{stream}");
            assert_eq!(warnings, count("warning"), "{phase}:\n{stream}");
            errors_by_count += errors;
            pending.clear();
        }
    }
    assert!(
        pending.is_empty(),
        "every diagnostic precedes a phase_complete:\n{stream}"
    );

    let start = lines[0];
    let complete = *lines.last().unwrap();
    assert!(
        complete.starts_with("{\"type\":\"build_complete\","),
        "{stream}"
    );
    let total: usize = field(complete, "total_errors").unwrap().parse().unwrap();
    assert_eq!(
        total, errors_by_count,
        "total_errors is the phases' sum:\n{stream}"
    );
    assert_eq!(total, 3, "{stream}");
    assert_eq!(field(complete, "success"), Some("false"));

    let id = field(start, "build_id").expect("build_start carries a build_id");
    assert_eq!(id.len(), 26, "a ULID: {id}");
    assert!(
        id.bytes()
            .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b)),
        "{id}"
    );
    assert_eq!(field(complete, "build_id"), Some(id), "one id per build");
    let ts = field(start, "timestamp").unwrap();
    assert!(
        ts.len() == 20 && ts.ends_with('Z') && &ts[4..5] == "-" && &ts[10..11] == "T",
        "an RFC 3339 UTC timestamp: {ts}"
    );
    assert!(start.contains("\"args\":[\"check\","), "{start}");
}

#[test]
fn jsonl_on_a_resolve_error_names_it_blocking_the_typecheck() {
    let path = scratch("resolve", "fn main() {\n    let y = undefined_name;\n}\n");
    let (code, stream, _) = check(&path, "jsonl");
    let _ = std::fs::remove_file(&path);
    assert_eq!(code, Some(1));
    let diagnostic = stream
        .lines()
        .find(|l| l.starts_with("{\"type\":\"diagnostic\","))
        .expect("the resolve error is streamed");
    assert_eq!(field(diagnostic, "phase"), Some("resolve"));
    let skipped = stream
        .lines()
        .find(|l| l.contains("\"type\":\"phase_skipped\"") && l.contains("\"phase\":\"typecheck\""))
        .expect("typecheck is skipped");
    assert!(skipped.contains("\"blocking\":[\"d1\"]"), "{skipped}");
}

#[test]
fn jsonl_on_a_clean_program_counts_nothing_and_succeeds() {
    let path = scratch("clean", "fn main() {\n    let x = 1;\n    println(x);\n}\n");
    let (code, stream, _) = check(&path, "jsonl");
    let _ = std::fs::remove_file(&path);
    assert_eq!(code, Some(0), "{stream}");
    assert!(!stream.contains("\"type\":\"diagnostic\""), "{stream}");
    let last = stream.lines().last().unwrap();
    assert!(
        last.contains("\"success\":true,\"total_errors\":0,\"total_warnings\":0"),
        "{last}"
    );
}

/// Run `cmd` with its stdout's reader closed before it writes, and return its
/// exit status and stderr.
fn with_stdout_closed(cmd: &mut Command) -> (Option<i32>, String) {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("karac");
    drop(child.stdout.take());
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    (child.wait().unwrap().code(), err)
}

#[test]
fn jsonl_into_a_closed_pipe_ends_quietly() {
    let path = scratch("epipe", MIXED);
    let (code, err) = with_stdout_closed(Command::new(env!("CARGO_BIN_EXE_karac")).args([
        "check",
        "--output=jsonl",
        path.to_str().unwrap(),
    ]));
    let _ = std::fs::remove_file(&path);
    assert!(!err.contains("panicked"), "{err}");
    assert_eq!(
        code,
        Some(141),
        "the status a SIGPIPE'd process reports: {err}"
    );
}

#[test]
fn karac_test_into_a_closed_pipe_ends_quietly() {
    let dir = std::env::temp_dir().join(format!("karac-jls-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(
        dir.join("src/main_test.kara"),
        "test \"one\" { assert_eq(1, 1); }\ntest \"two\" { assert_eq(2, 2); }\n",
    )
    .unwrap();
    let (code, err) = with_stdout_closed(
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .env_remove("KARAC_TEST_JIT")
            .current_dir(&dir)
            .args(["test", "--interp"]),
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!err.contains("panicked"), "{err}");
    assert_eq!(code, Some(141), "{err}");
}

#[test]
fn assert_eq_refuses_operands_that_cannot_print() {
    let path = scratch(
        "nodebug",
        "#[derive(PartialEq)]\nstruct P { x: i32 }\n\
         fn main() {\n    assert_eq(P { x: 1 }, P { x: 1 });\n    println(1);\n}\n",
    );
    let (code, out, err) = check(&path, "text");
    let _ = std::fs::remove_file(&path);
    assert_eq!(code, Some(1), "{out}{err}");
    assert!(
        err.contains("`assert_eq` needs its operands to implement `Debug`"),
        "{err}"
    );

    let path = scratch(
        "debug",
        "#[derive(PartialEq, Debug)]\nstruct P { x: i32 }\n\
         fn main() {\n    assert_ne(P { x: 1 }, P { x: 2 });\n    println(1);\n}\n",
    );
    let (code, out, err) = check(&path, "text");
    let _ = std::fs::remove_file(&path);
    assert_eq!(code, Some(0), "{out}{err}");
}
