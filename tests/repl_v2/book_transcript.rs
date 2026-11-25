//! The REPL the book's chapter 1b shows (docs/book/src/ch01b-two-surfaces.md),
//! held to its transcript: a bare expression prints its value, `:effects`
//! lists the session's own functions, `:type` sees the session's bindings,
//! and `:save` writes a file that checks and runs unmodified.

use super::{karac_repl, session};

/// The chapter's binary search, as its REPL transcript types it.
const BINARY_SEARCH: &str = "fn binary_search(haystack: ref Vec[i32], needle: i32) -> Option[i64] {
    let mut lo: i64 = 0;
    let mut hi: i64 = haystack.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        let value = haystack[mid];
        if value == needle { return Some(mid); }
        else if value < needle { lo = mid + 1; }
        else { hi = mid; }
    }
    None
}";

/// Each cell's output, `Ok` for what it printed, `Err` for its errors
/// joined.
fn outputs(cells: &[&str]) -> (karac::repl::Session, Vec<Result<String, String>>) {
    let mut s = session();
    let out = cells
        .iter()
        .map(|cell| {
            let r = s.evaluate_cell_captured(cell);
            match r.errors.is_empty() {
                true => Ok(r.stdout),
                false => Err(r.errors.join("\n")),
            }
        })
        .collect();
    (s, out)
}

/// The chapter's transcript, cell by cell, prints what the chapter shows.
#[test]
fn repl_book_transcript_prints_what_the_book_shows() {
    let cells = [
        BINARY_SEARCH,
        "let nums = [1, 3, 5, 7, 9, 11, 13];",
        "binary_search(nums, 7)",
        "binary_search(nums, 100)",
        "binary_search(nums, 5)",
        "let nums = [10, 20, 30, 40, 50];",
        "binary_search(nums, 30)",
        "let owned = [1, 2, 3];",
        "let sum: i32 = owned.iter().sum();",
        "println(f\"sum={sum}, owned still here: {owned.len()}\");",
        "let s: String = \"hello\".to_string();",
        "let taken = s;",
        "println(s);",
    ];
    let (_, out) = outputs(&cells);
    let ok = |s: &str| Ok(s.to_string());
    assert_eq!(out[2], ok("Some(3)\n"));
    assert_eq!(out[3], ok("None\n"));
    assert_eq!(out[4], ok("Some(2)\n"));
    assert_eq!(out[6], ok("Some(2)\n"));
    assert_eq!(out[9], ok("sum=6, owned still here: 3\n"));
    for (i, o) in out.iter().enumerate().take(12) {
        assert!(o.is_ok(), "cell {}: {o:?}", i + 1);
    }
    let err = out[12].clone().unwrap_err();
    assert!(
        err.contains("cell 13:1:9: value 's' moved here, used again here (moved at cell 12:1:13)"),
        "{err}"
    );
}

/// A bare final expression prints its value, as `%show` renders it; a
/// binding shown is lent, so it is still there after, whatever its type.
#[test]
fn repl_bare_expression_shows_its_value_without_moving_it() {
    let (_, out) = outputs(&[
        "1 + 2",
        "let x = 5;",
        "x * 2",
        "\"hi\".to_string()",
        "let s = \"hi\".to_string();",
        "s",
        "s",
        "let t = (1, s);",
        "t",
        "t",
        "println(t.1);",
        "let y = 4; y + 1",
        "// the last value\nx",
        "let v = [1, 2, 3];\n    v.len()",
        "if x > 3 { \"big\" } else { \"small\" }",
    ]);
    let want = [
        "3\n",
        "",
        "10\n",
        "\"hi\"\n",
        "",
        "\"hi\"\n",
        "\"hi\"\n",
        "",
        "(1, \"hi\")\n",
        "(1, \"hi\")\n",
        "hi\n",
        "5\n",
        "5\n",
        "3\n",
        "\"big\"\n",
    ];
    for (i, (got, want)) in out.iter().zip(want).enumerate() {
        assert_eq!(got, &Ok(want.to_string()), "cell {}", i + 1);
    }
}

/// A final expression of type `()` prints nothing and is kept, and saved,
/// as the statement it is; a unit-valued `if` lent to the recording call
/// is ordinary MIR (it built `()` into a `ref ()` slot before).
#[test]
fn repl_unit_final_expression_prints_nothing() {
    let (s, out) = outputs(&[
        "println(\"a\")",
        "let x = 5;",
        "if x > 3 {\n    println(\"big\");\n} else {\n    println(\"small\");\n}",
        "()",
    ]);
    assert_eq!(out[0], Ok("a\n".to_string()));
    assert_eq!(out[2], Ok("big\n".to_string()));
    assert_eq!(out[3], Ok(String::new()));
    assert_eq!(s.cell_history()[0], "println(\"a\");");
    let saved = s.render_exported_session();
    assert!(!saved.contains("let _ = println"), "{saved}");
}

/// A `()` passed for a generic parameter, which borrows (D5), is lent like
/// any other value.
#[test]
fn repl_unit_lent_to_a_generic_parameter_runs() {
    let (_, out) = outputs(&[
        "fn f[T](v: T) {}",
        "let x = 5;\nf(());\nf(if x > 3 { () } else { () });\nprintln(\"ran\");",
    ]);
    assert_eq!(out[1], Ok("ran\n".to_string()));
}

/// `:effects` is the session's own functions, their union first; the
/// library's table is not printed.
#[test]
fn repl_effects_lists_the_session_functions() {
    let (s, out) = outputs(&[
        "fn read_config() -> String {\n    FileSystem.read_to_string(\"config.toml\").unwrap()\n}",
        "fn bump(a: i64) -> i64 { a + 1 }",
    ]);
    assert!(out.iter().all(Result::is_ok), "{out:?}");
    assert_eq!(
        s.effects_report(),
        // `a + 1` can overflow, so `bump` panics: the report reads the typed
        // effect pass, as `karac query effects` does (B-2026-11-24-11).
        "session effects: allocates(Heap), blocks, panics, reads(FileSystem)\n  bump: panics\n  read_config: allocates(Heap), blocks, panics, reads(FileSystem)\n"
    );
}

/// `:type` sees the session's bindings, of any type.
#[test]
fn repl_type_of_a_session_binding() {
    let (s, out) = outputs(&[
        "let x = 5;",
        "let s = \"hi\".to_string();",
        "let v: Vec[i32] = [1, 2];",
        "let t = (1, s.clone());",
    ]);
    assert!(out.iter().all(Result::is_ok), "{out:?}");
    assert_eq!(s.type_of("x"), Ok("i64".to_string()));
    assert_eq!(s.type_of("s"), Ok("String".to_string()));
    assert_eq!(s.type_of("v"), Ok("Vec[i32]".to_string()));
    assert_eq!(s.type_of("t"), Ok("(i64, String)".to_string()));
    assert_eq!(s.type_of("x * 2"), Ok("i64".to_string()));
    assert!(s.type_of("nope").is_err());
}

/// `:save` writes only the cells that ran, a bare expression as
/// `let _ = …;`, and a `main` whose effects are a `with` clause: the file
/// checks clean and runs, printing what the session printed.
#[test]
fn repl_save_writes_a_file_that_runs_unmodified() {
    let dir = std::env::temp_dir().join(format!("karac-repl-save-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("search.kara");
    let input = format!(
        "{BINARY_SEARCH}\nlet nums = [1, 3, 5, 7, 9, 11, 13];\nbinary_search(nums, 7)\n\
         let s: String = \"hello\".to_string();\nlet taken = s;\nprintln(s);\n\
         println(taken);\ns\n:save {}\n:quit\n",
        path.display()
    );
    let out = karac_repl(&[], &input, false);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stdout.contains("Some(3)\nhello\n"), "{stdout}\n{stderr}");
    assert!(stdout.contains("session saved to"), "{stdout}\n{stderr}");
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("fn main() with "), "{saved}");
    assert!(
        saved.contains("    let _ = binary_search(nums, 7);\n"),
        "{saved}"
    );
    assert!(!saved.contains("println(s)"), "{saved}");
    assert!(!saved.contains("let _ = s;"), "{saved}");
    let karac = env!("CARGO_BIN_EXE_karac");
    let check = std::process::Command::new(karac)
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&check.stdout).to_string()
        + &String::from_utf8_lossy(&check.stderr);
    assert!(check.status.success(), "{text}\n{saved}");
    assert!(!text.contains("warning"), "{text}\n{saved}");
    let run = std::process::Command::new(karac)
        .args(["run", "--interp"])
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "hello\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `:help` says what the REPL does: bindings keep their values and a
/// mutation is seen by later cells.
#[test]
fn repl_help_describes_values_kept_across_cells() {
    let out = karac_repl(&[], ":help\n:quit\n", false);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("does NOT survive"), "{stdout}");
    assert!(!stdout.contains("recompute every time"), "{stdout}");
    assert!(stdout.contains("a mutation"), "{stdout}");
    assert!(stdout.contains("prints its value"), "{stdout}");
}
