//! The REPL's user-visible behaviour, pinned before its cells move onto one
//! interpreter per session (B-2026-11-19-3): meta-commands, magics,
//! redefinition and recovery from a refused or panicking cell.

use super::{karac_repl, session};

/// Each cell's output, `Ok` for what it printed, `Err` for its errors joined.
fn outputs(s: &mut karac::repl::Session, cells: &[&str]) -> Vec<Result<String, String>> {
    cells
        .iter()
        .map(|cell| {
            let r = s.evaluate_cell_captured(cell);
            match r.errors.is_empty() {
                true => Ok(r.stdout),
                false => Err([r.notes, r.errors].concat().join("\n")),
            }
        })
        .collect()
}

fn stdout_stderr(out: &std::process::Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `:type` reads a binding's and an expression's type, `:effects` the
/// session's, `:reset` drops the bindings, and an unknown or incomplete
/// meta-command says how to use it, with the session going on.
#[test]
fn repl_baseline_meta_commands() {
    let input = "let a = 21;\n:type a\n:type a > 1\n:effects\n:nonsense\n:type\n:save\n\
                 :reset\nprintln(a);\nlet b = 5;\nprintln(b);\n:quit\n";
    let out = karac_repl(&[], input, false);
    let (stdout, stderr) = stdout_stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{stdout}\n{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        &lines[1..],
        [
            "i64",
            "bool",
            "session effects: none",
            "(cleared 1 persistent let binding(s); items + history kept)",
            "5",
        ],
        "{stdout}"
    );
    for want in [
        "unknown meta-command ':nonsense'. Try :help for the supported list.",
        "usage: :type <expr>",
        "usage: :save <path.kara>",
        "undefined name 'a'",
    ] {
        assert!(stderr.contains(want), "{want}: {stderr}");
    }
}

/// `:help` names every meta-command.
#[test]
fn repl_baseline_help_lists_the_meta_commands() {
    let out = karac_repl(&[], ":help\n:quit\n", false);
    let (stdout, _) = stdout_stderr(&out);
    for cmd in [
        ":help",
        ":quit",
        ":type",
        ":effects",
        ":save",
        ":reset",
        ":provide",
        ":end-provide",
        ":dep",
    ] {
        assert!(stdout.contains(&format!("  {cmd}")), "{cmd}: {stdout}");
    }
}

/// The magics answer, and an unknown or empty one lists what there is.
#[test]
fn repl_baseline_magics() {
    let mut s = session();
    let out = outputs(
        &mut s,
        &[
            "fn double(x: i64) -> i64 { x * 2 }",
            "let a = 21;",
            "let v = [1, 2, 3];",
        ],
    );
    assert!(out.iter().all(Result::is_ok), "{out:?}");
    let magic = |s: &mut karac::repl::Session, m: &str| {
        let o = s.dispatch_magic(m);
        (o.ok, o.text)
    };
    let (ok, text) = magic(&mut s, "%ownership");
    assert!(ok);
    assert_eq!(text.trim_end(), "a: owned (stack)\nv: owned (stack)");
    let (ok, text) = magic(&mut s, "%timeline");
    assert!(ok);
    assert_eq!(
        text.trim_end(),
        "cell 1: (pure)\ncell 2: (pure)\ncell 3: (pure)"
    );
    let (ok, text) = magic(&mut s, "%rc");
    assert!(ok);
    assert_eq!(text.trim_end(), "(no RC fallbacks in current session)");
    let (ok, text) = magic(&mut s, "%show a");
    assert!(ok);
    assert_eq!(text.trim_end(), "21");
    let (ok, text) = magic(&mut s, "%effects");
    assert!(ok);
    // The session's own effects, as `:effects` reports them; it listed
    // the library's `println` (B-2026-11-20-131).
    assert_eq!(text, "session effects: none", "{text}");
    let (ok, text) = magic(&mut s, "%explain TYPE_MISMATCH");
    assert!(ok, "{text}");
    let (ok, text) = magic(&mut s, "%explain a");
    assert!(!ok);
    assert!(text.starts_with("unknown diagnostic class 'a'"), "{text}");
    let (ok, text) = magic(&mut s, "%nonsense");
    assert!(!ok);
    assert!(
        text.starts_with("unknown magic `%nonsense`. Supported: %effects"),
        "{text}"
    );
    let (ok, text) = magic(&mut s, "%");
    assert!(!ok);
    assert!(text.starts_with("empty magic command."), "{text}");
    // The magics leave the session as it was.
    assert_eq!(
        outputs(&mut s, &["double(a) + v.len()"]),
        [Ok("45\n".to_string())]
    );
}

/// A redefined function is called by the cells after it; a redefined
/// struct takes the values made with the earlier one out of scope, with a
/// note, and the new definition is what later cells build. A refused cell
/// after that does not report the retired value's cell.
#[test]
fn repl_baseline_redefinition() {
    let mut s = session();
    let out = outputs(
        &mut s,
        &[
            "fn double(x: i64) -> i64 { x * 2 }",
            "let a = 21;",
            "double(a)",
            "fn double(x: i64) -> i64 { x * 3 }",
            "double(a)",
            "struct P { x: i64 }",
            "let p = P { x: 1 };",
            "println(p.x);",
            "struct P { x: i64, y: i64 }",
            "println(p.x);",
            "let q = P { x: 1, y: 2 };",
            "println(q.y);",
            "let bad: i64 = \"s\";",
            "println(a + q.x);",
        ],
    );
    let ok = |s: &str| Ok(s.to_string());
    assert_eq!(out[2], ok("42\n"));
    assert_eq!(out[4], ok("63\n"));
    assert_eq!(out[7], ok("1\n"));
    assert_eq!(out[8], ok(""));
    let Err(e) = &out[9] else { panic!("{out:?}") };
    assert!(e.contains("`P` was redefined in cell"), "{e}");
    assert!(e.contains("undefined name 'p'"), "{e}");
    assert_eq!(out[11], ok("2\n"));
    let Err(e) = &out[12] else { panic!("{out:?}") };
    assert!(e.contains("expected 'i64', found 'String'"), "{e}");
    assert!(
        !e.contains("missing field"),
        "the retired `let p` was replayed: {e}"
    );
    assert_eq!(out[13], ok("22\n"));
}

/// A struct redefined after another item replaces the earlier one.
#[test]
fn repl_baseline_redefinition_after_another_item() {
    let mut s = session();
    let out = outputs(
        &mut s,
        &[
            "fn d(x: i64) -> i64 { x * 2 }",
            "struct P { x: i64 }",
            "struct P { x: i64, y: i64 }",
            "let q = P { x: 1, y: 2 };",
            "println(q.y + d(1));",
        ],
    );
    assert!(out.iter().all(Result::is_ok), "{out:?}");
    assert_eq!(out[4], Ok("4\n".to_string()));
}

/// A refused cell (a type error, an undefined name, a bad item) and a
/// panicking one leave the session as it was: their bindings are not kept,
/// and the cells after them see the earlier ones' values.
#[test]
fn repl_baseline_error_recovery() {
    let mut s = session();
    let out = outputs(
        &mut s,
        &[
            "let a = 21;",
            "let mut v = [1, 2, 3].to_vec();",
            "let bad: i64 = \"s\";",
            "let z = undefined_name + 1;",
            "fn broken(x: i64) -> i64 { x + \"s\" }",
            "v.push(4); let i = v[10];",
            "println(a + v.len());",
            "println(bad);",
            "broken(1)",
        ],
    );
    assert!(out[0].is_ok() && out[1].is_ok(), "{out:?}");
    for (i, want) in [
        (2, "expected 'i64', found 'String'"),
        (3, "undefined name 'undefined_name'"),
        (5, "index out of bounds"),
        (7, "undefined name 'bad'"),
        (8, "undefined name 'broken'"),
    ] {
        let Err(e) = &out[i] else {
            panic!("cell {i}: {out:?}")
        };
        assert!(e.contains(want), "cell {i}: {e}");
    }
    assert!(out[4].is_err(), "{out:?}");
    // The panicking cell's push is rolled back with it.
    assert_eq!(out[6], Ok("24\n".to_string()));
    assert_eq!(s.cell_history().len(), 3, "{:?}", s.cell_history());
}
