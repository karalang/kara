//! `karac fmt` keeps every comment in the file: `//` and `/* */` comments,
//! `///` and `//!` doc comments, on their own line or ending a line of code,
//! between items, in a function body, among struct fields, enum variants,
//! impl methods and match arms, and in an otherwise empty block. The AST
//! carries none of them, so the formatter used to drop them all
//! (B-2026-10-30-16). The output formats to itself, and an impl or import
//! list holding a comment keeps its order rather than move code away from
//! its comment.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn fmt(src: &str, tag: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "karac-fmt-comments-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = karac(&["fmt", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn karac_fmt_keeps_every_comment() {
    let src = "//! Module doc.

// top comment
import std.io.*;

/// Adds one.
fn inc(x: i64)->i64 {
    x + 1 // trailing on the tail
}

/* block
   comment */
struct P {
    // field note
    n: i64, // n trailing
}

enum Shape {
    // the empty one
    Dot,
    Line(i64), // one payload
}

impl P {
    // zeta stays first
    fn zeta(self) -> i64 { self.n }
    fn alpha(self) -> i64 { self.n } // alpha trailing
}

fn todo() {
    // nothing yet
}

fn main() {
    // inside
    let x = 1; // trailing
    /* mid */ let y = 2;
    if x > 0 {
        // in if
        println(x);
    }
    let s = Shape.Line(3);
    let n = match s {
        // a dot
        Shape.Dot => 0,
        Shape.Line(k) => k, // a line
    };
    let p = P { n: y };
    todo();
    println(p.zeta() + p.alpha() + inc(n));
    // last line before brace
} // main ends
// eof
";
    let once = fmt(src, "all");
    for want in [
        "//! Module doc.\n\n// top comment\nimport std.io.*;\n",
        "/// Adds one.\nfn inc(x: i64) -> i64 {\n    x + 1 // trailing on the tail\n}\n",
        "/* block\n   comment */\nstruct P {\n    // field note\n    n: i64, // n trailing\n}\n",
        "    // the empty one\n    Dot,\n    Line(i64), // one payload\n",
        "    // zeta stays first\n    fn zeta(self) -> i64 {",
        "    fn alpha(self) -> i64 {\n        self.n\n    } // alpha trailing\n}\n",
        "fn todo() {\n    // nothing yet\n}\n",
        "    // inside\n    let x = 1; // trailing\n    /* mid */\n    let y = 2;\n",
        "        // in if\n        println(x);\n",
        "        // a dot\n        Shape.Dot => 0,\n        Shape.Line(k) => k, // a line\n    };\n",
        "    // last line before brace\n} // main ends\n// eof\n",
    ] {
        assert!(once.contains(want), "missing {want:?} in:\n{once}");
    }
    assert!(
        once.find("fn zeta").unwrap() < once.find("fn alpha").unwrap(),
        "{once}"
    );
    assert_eq!(fmt(&once, "again"), once);
}

#[test]
fn karac_fmt_keeps_a_comment_inside_an_expression() {
    // A comment inside an expression the formatter puts on one line moves to
    // the end of that line; a block comment that follows a `//` one gets its
    // own line rather than ending up inside it.
    let src = "fn main() {
    let v = [
        1, // one
        2, /* two
        lines */
    ];
    println(v[0]);
}
";
    let once = fmt(src, "expr");
    assert!(
        once.contains("    let v = [1, 2]; // one\n    /* two\n        lines */\n"),
        "{once}"
    );
    assert_eq!(fmt(&once, "expr-again"), once);
}

#[test]
fn karac_fmt_still_sorts_imports_and_methods_without_comments() {
    let src = "import std.io.*;
import std.collections.*;

struct Q { n: i64 }

impl Q {
    fn zeta(self) -> i64 { self.n }
    fn alpha(self) -> i64 { self.n }
}

fn main() {
    let q = Q { n: 1 };
    println(q.zeta() + q.alpha());
}
";
    let out = fmt(src, "sorted");
    assert!(
        out.starts_with("import std.collections.*;\nimport std.io.*;\n"),
        "{out}"
    );
    assert!(
        out.find("fn alpha").unwrap() < out.find("fn zeta").unwrap(),
        "{out}"
    );
}

#[test]
fn karac_fmt_quotes_test_names_and_drops_needless_semicolons() {
    // `test "name"` kept its name but lost the quotes, so the output did not
    // parse; a block-like statement (`for`, `if`, `match`) gained a `;`. The
    // last statement keeps its `;`, which stops it being the block's value.
    let src = "fn pick(c: bool) {
    for i in 0..2 { println(i); };
    if c { println(1); }
    let n = match c { true => 1, false => 2 };
    match c { true => println(n), false => {} };
    if c { 1 } else { 2 };
}

test \"pick \\\"twice\\\"\" {
    pick(true);
}
";
    let once = fmt(src, "semi");
    for want in [
        "        println(i);\n    }\n    if c {",
        "        println(1);\n    }\n    let n = match c {",
        "        false => 2,\n    };\n",
        "        false => {},\n    }\n    if c {",
        "    } else {\n        2\n    };\n}\n",
        "test \"pick \\\"twice\\\"\" {\n    pick(true);\n}\n",
    ] {
        assert!(once.contains(want), "missing {want:?} in:\n{once}");
    }
    assert_eq!(fmt(&once, "semi-again"), once);
}
