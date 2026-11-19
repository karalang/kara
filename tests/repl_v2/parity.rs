//! The REPL against `karac run`: a program pasted cell by cell behaves as
//! the program does (deferred.md § Interactive Evaluation Model: the
//! session is the body of one implicit `main`). Found by feeding the
//! corpus/core programs to `karac repl` statement by statement.

use super::{karac_repl, session};

const DROPS: &str = "struct D { n: i64 }
impl Drop for D { fn drop(mut ref self) { println(f\"drop {self.n}\") } }";

/// A binding lives until the session ends, as a `main` local does: its
/// `Drop` runs once, at the end, latest binding first, not at the end of
/// the cell that made it.
#[test]
fn repl_parity_drop_runs_once_at_session_end() {
    let mut s = session();
    assert!(s.evaluate_cell_captured(DROPS).errors.is_empty());
    let r = s.evaluate_cell_captured("let a = D { n: 1 };");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout, "");
    let r = s.evaluate_cell_captured("let b = D { n: 2 };");
    assert_eq!(r.stdout, "");
    let r = s.evaluate_cell_captured("println(a.n + b.n);");
    assert_eq!(r.stdout, "3\n");
    assert_eq!(s.end().expect("session end"), "drop 2\ndrop 1\n");
}

/// The binary runs the session's end at `:quit` and at end of input.
#[test]
fn repl_parity_quit_and_eof_drop_the_bindings() {
    for tail in [":quit\n", ""] {
        let input = format!("{DROPS}\nlet a = D {{ n: 1 }}\nprintln(\"cell\")\n{tail}");
        let out = karac_repl(&[], &input, false);
        // End of input prints the newline the prompt's line lacked.
        let stdout = String::from_utf8_lossy(&out.stdout).replace("\n\n", "\n");
        assert!(
            stdout.contains("cell\ndrop 1\n"),
            "tail {tail:?}: {stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(stdout.matches("drop 1").count(), 1, "{stdout}");
    }
}

/// `?` is legal in any cell; an `Err` stops that cell only.
#[test]
fn repl_parity_question_mark_in_a_cell() {
    let mut s = session();
    let r = s.evaluate_cell_captured("let v = i64.parse(\"5\")?;");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let r = s.evaluate_cell_captured("let w = i64.parse(\"x\")?;");
    assert!(!r.errors.is_empty(), "the Err stops the cell");
    let r = s.evaluate_cell_captured("println(v);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout, "5\n");
}

/// A labeled block's `'d` is a label, not a char literal that leaves the
/// cell's braces open.
#[test]
fn repl_parity_labeled_block_in_a_cell() {
    let out = karac_repl(
        &[],
        "let r = 'd: { if true { break 'd 4 }; 5 }\nprintln(r)\n:quit\n",
        false,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.lines().last(), Some("4"), "{stdout}");
}

/// Re-entering an impl replaces the methods it declares again, inherent
/// or trait, instead of making them ambiguous.
#[test]
fn repl_parity_impl_reentered_replaces_its_methods() {
    let mut s = session();
    for cell in [
        "struct Q { x: i64 }",
        "impl Q { fn get(self) -> i64 { self.x } fn keep(self) -> i64 { 1 } }",
        "impl Q { fn get(self) -> i64 { self.x * 10 } }",
        "trait T { fn n(self) -> i64; }",
        "impl T for Q { fn n(self) -> i64 { 1 } }",
        "impl T for Q { fn n(self) -> i64 { 2 } }",
    ] {
        let r = s.evaluate_cell_captured(cell);
        assert!(r.errors.is_empty(), "{cell}: {:?}", r.errors);
    }
    let r = s.evaluate_cell_captured("let q = Q { x: 3 }; println(q.get() + q.keep() + q.n());");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout, "33\n");
}

/// A redefined struct leaves later cells working; a value made with the
/// earlier definition is out of scope, and using it says which cell
/// redefined its type.
#[test]
fn repl_parity_struct_redefinition_retires_its_values() {
    let mut s = session();
    assert!(s
        .evaluate_cell_captured("struct P { x: i64 }")
        .errors
        .is_empty());
    assert!(s
        .evaluate_cell_captured("let p = P { x: 1 };")
        .errors
        .is_empty());
    let r = s.evaluate_cell_captured("struct P { y: i64 }");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let r = s.evaluate_cell_captured("let q = P { y: 2 }; println(q.y);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout, "2\n");
    let r = s.evaluate_cell_captured("println(p.x);");
    assert!(!r.errors.is_empty());
    assert!(
        r.notes
            .iter()
            .any(|n| n.contains("`P` was redefined in cell 3")),
        "{:?}",
        r.notes
    );
}

/// A destructuring `let` is a statement cell, not a broken item.
#[test]
fn repl_parity_destructuring_let_cell() {
    let mut s = session();
    for cell in [
        "struct H { xs: Vec[i64], n: i64 }",
        "let h = H { xs: [1, 2], n: 3 };",
        "let H { xs, n } = h;",
    ] {
        let r = s.evaluate_cell_captured(cell);
        assert!(r.errors.is_empty(), "{cell}: {:?}", r.errors);
    }
    let r = s.evaluate_cell_captured("println(xs.len() + n);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout, "5\n");
}

/// Items pasted in file order may name later ones: a cell that only names
/// items no cell defines yet is held, and joins when they are defined,
/// mutual recursion included.
#[test]
fn repl_parity_item_naming_a_later_item_is_held() {
    let mut s = session();
    let r = s.evaluate_cell_captured("fn wrap(x: i64) -> i64 { leaf(x) + 1 }");
    assert!(
        r.errors.len() == 1 && r.errors[0].contains("`leaf`"),
        "{:?}",
        r.errors
    );
    let r = s.evaluate_cell_captured(
        "fn even(n: i64) -> bool { if n == 0 { true } else { odd(n - 1) } }",
    );
    assert!(r.errors[0].contains("held"), "{:?}", r.errors);
    let r = s.evaluate_cell_captured(
        "fn odd(n: i64) -> bool { if n == 0 { false } else { even(n - 1) } }",
    );
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let r = s.evaluate_cell_captured("fn leaf(x: i64) -> i64 { x * 2 }");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let r = s.evaluate_cell_captured("println(wrap(4)); println(even(4));");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout, "9\ntrue\n");
    // A plain error is still an error, not held.
    let r = s.evaluate_cell_captured("fn bad() -> i64 { \"s\" }");
    assert!(
        !r.errors.is_empty() && !r.errors[0].contains("held"),
        "{:?}",
        r.errors
    );
}

/// The logger a cell set up is the one later cells log through.
#[test]
fn repl_parity_logger_set_up_in_one_cell_holds_in_the_next() {
    let mut s = session();
    for cell in [
        "import std.log;",
        "import std.log.{Level, info, warn};",
        "log.init(log.json_backend(log.Stream.Stdout), level: Level.Warn);",
    ] {
        let r = s.evaluate_cell_captured(cell);
        assert!(r.errors.is_empty(), "{cell}: {:?}", r.errors);
    }
    let r = s.evaluate_cell_captured("info(\"dropped\"); warn(\"kept\");");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(
        r.stdout,
        "{\"level\":\"warn\",\"msg\":\"kept\",\"task_id\":null}\n"
    );
}

/// Piped input, as a program pasted into the REPL arrives, joins a line
/// that continues a method chain to the statement above it.
#[test]
fn repl_parity_piped_chain_continues_the_statement() {
    let out = karac_repl(
        &[],
        "let xs: Vec[i64] = [1, 2, 3];\nlet total = xs\n    .iter()\n    .map(|x| x * 10)\n    .sum();\nprintln(total);\nprintln(total + 1)\n:quit\n",
        false,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.is_empty(), "{stderr}");
    assert_eq!(
        stdout.lines().skip(1).collect::<Vec<_>>(),
        ["60", "61"],
        "{stdout}"
    );
}

const R_DROPS: &str = "struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f\"d{self.id}\") } }";

/// B-2026-11-19-2: a binding a later `let` shadows lives on until the
/// session ends, as in `main`, and the session's bindings drop latest
/// first, in the order they were made.
#[test]
fn repl_parity_shadowed_binding_drops_at_session_end() {
    let mut s = session();
    for cell in [
        R_DROPS,
        "let x = R { id: 1 };",
        "let y = R { id: 5 };",
        "let x = R { id: x.id + 1 };",
        "let x = R { id: x.id + 1 };",
    ] {
        let r = s.evaluate_cell_captured(cell);
        assert!(r.errors.is_empty(), "{cell}: {:?}", r.errors);
        assert_eq!(r.stdout, "", "{cell}");
    }
    let r = s.evaluate_cell_captured("println(f\"{x.id} {y.id}\");");
    assert_eq!(r.stdout, "3 5\n");
    assert_eq!(s.end().expect("session end"), "d3\nd2\nd5\nd1\n");
}

/// B-2026-11-19-2: what is left of a struct a field moved out of drops at
/// the session's end, field by field, last first.
#[test]
fn repl_parity_partially_moved_struct_drops_at_session_end() {
    let mut s = session();
    for cell in [
        R_DROPS,
        "struct P { a: R, b: R, c: R }",
        "let p = P { a: R { id: 7 }, b: R { id: 8 }, c: R { id: 9 } };",
        "let x = p.b;",
    ] {
        let r = s.evaluate_cell_captured(cell);
        assert!(r.errors.is_empty(), "{cell}: {:?}", r.errors);
        assert_eq!(r.stdout, "", "{cell}");
    }
    assert_eq!(s.end().expect("session end"), "d8\nd9\nd7\n");
}

/// B-2026-11-19-2: a cell's top-level `defer` runs at the session's end,
/// between the drops of the bindings made before and after it.
#[test]
fn repl_parity_defer_runs_at_session_end() {
    let mut s = session();
    for cell in [
        R_DROPS,
        "let a = R { id: 1 };",
        "defer { println(\"defer\"); }",
        "let b = R { id: 2 };",
    ] {
        let r = s.evaluate_cell_captured(cell);
        assert!(r.errors.is_empty(), "{cell}: {:?}", r.errors);
        assert_eq!(r.stdout, "", "{cell}");
    }
    assert_eq!(
        s.evaluate_cell_captured("println(\"body\");").stdout,
        "body\n"
    );
    assert_eq!(s.end().expect("session end"), "d2\ndefer\nd1\n");
}
