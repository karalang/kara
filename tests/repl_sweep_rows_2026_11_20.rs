//! The REPL sweep against deferred.md § Interactive Evaluation Model
//! (2026-11-20): `--auto-clone` over a move in a bare-expression cell
//! (B-2026-11-20-93), `:save` of a session that used `?` (B-2026-11-20-94),
//! a line ending in an operator and input ending mid-cell
//! (B-2026-11-20-95), and `:type`'s errors (B-2026-11-20-96).

use karac::repl::{ReplOptions, Session};
use std::io::Write;
use std::process::{Command, Output, Stdio};

fn karac_repl(args: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("repl")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn karac repl");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("karac repl")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The move is placed in the cell as typed (`eat(w)`'s `w` at column 5,
/// not 13 of the `let _ = eat(w);` the cell runs as), so `--auto-clone`
/// finds it and clones there.
#[test]
fn repl_sweep_move_in_a_bare_expression_cell_is_placed_and_auto_cloned() {
    let cells = "fn eat(x: own Vec[i64]) -> i64 { x.len() }\n\
                 let w: Vec[i64] = [1, 2];\n\
                 eat(w)\n\
                 w.len()\n";
    let plain = karac_repl(&[], cells);
    let err = text(&plain.stderr);
    assert!(err.contains("(moved at cell 3:1:5)"), "{err}");
    let cloned = karac_repl(&["--auto-clone"], cells);
    let (out, err) = (text(&cloned.stdout), text(&cloned.stderr));
    assert!(err.contains("perf[auto-clone-in-repl]"), "{out}\n{err}");
    assert!(!err.contains("E0500"), "{err}");
    assert!(out.contains("\n2\n2\n"), "{out}\n{err}");
}

/// A session that used `?` saves as the `fn main() -> Result[(), AnyError]`
/// it ran as, ending in `Ok(())`, so the file checks.
#[test]
fn repl_sweep_saved_session_with_question_mark_checks() {
    let mut s = Session::with_options(ReplOptions::default());
    for cell in ["let n = i64.parse(\"42\")?;", "println(f\"{n}\");"] {
        let r = s.evaluate_cell_captured(cell);
        assert!(r.errors.is_empty(), "{cell}: {:?}", r.errors);
    }
    let saved = s.render_exported_session();
    assert!(
        saved.contains("fn main() -> Result[(), AnyError]"),
        "{saved}"
    );
    assert!(saved.contains("    Ok(())\n}\n"), "{saved}");
    let dir = std::env::temp_dir().join(format!("karac-repl-save-q-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("s.kara");
    std::fs::write(&f, &saved).unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["run", "--interp", f.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(run.status.success(), "{saved}\n{}", text(&run.stderr));
    assert_eq!(text(&run.stdout), "42\n");
}

/// A session with no `?` keeps its plain `fn main()`.
#[test]
fn repl_sweep_saved_session_without_question_mark_keeps_plain_main() {
    let mut s = Session::with_options(ReplOptions::default());
    let r = s.evaluate_cell_captured("let n = 1;");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let saved = s.render_exported_session();
    assert!(!saved.contains("Result[(), AnyError]"), "{saved}");
    assert!(!saved.contains("Ok(())"), "{saved}");
}

/// A line ending in an operator (`+`, `and`) reads on; an open range does
/// not; input that ends inside a cell says so and runs nothing.
#[test]
fn repl_sweep_operator_line_continues_and_eof_mid_cell_is_named() {
    let out = karac_repl(
        &[],
        "let t = 1 +\n    2;\nt\nlet ok = true and\n  false;\nok\nlet r = 0..;\nlet u = 3;\nu\n",
    );
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(stdout.contains("\n3\nfalse\n3\n"), "{stdout}\n{stderr}");
    assert!(stderr.is_empty(), "{stderr}");

    let out = karac_repl(&[], "let z = 5;\nfn g(x: i64) -> i64 {\n    x + 1\n");
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("input ended inside an unfinished cell"),
        "{stderr}"
    );
    assert!(!stderr.contains("fn main()"), "{stderr}");
    assert!(!stderr.contains("<repl>"), "{stderr}");
}

/// `:type`'s errors point into the expression as typed.
#[test]
fn repl_sweep_type_errors_name_the_expression() {
    let s = Session::with_options(ReplOptions::default());
    let errs = s.type_of("nosuch").unwrap_err().join("\n");
    assert!(
        errs.contains(":type:1:1: undefined name 'nosuch'"),
        "{errs}"
    );
    assert!(errs.contains("1 | nosuch\n"), "{errs}");
    assert!(!errs.contains("__t"), "{errs}");
    assert!(!errs.contains("<repl>"), "{errs}");
    let errs = s.type_of("1 +").unwrap_err().join("\n");
    assert!(errs.contains(":type:1:4:"), "{errs}");
    assert!(!errs.contains("__t"), "{errs}");
}
