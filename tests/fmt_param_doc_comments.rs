//! `karac fmt` keeps a `///` before a parameter before that parameter
//! (B-2026-11-15-26): a doc comment before the first parameter, or before
//! a method's receiver, moved into the body, where it documents nothing
//! and `karac check` warns that it is discarded. The list is laid out one
//! parameter per line, the output formats to itself, and it parses with
//! the doc comment on its parameter.

use std::process::{Command, Stdio};

fn fmt(src: &str, tag: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "karac-fmt-param-doc-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["fmt", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn fmt_fixed_point(src: &str, tag: &str, want: &str) {
    let once = fmt(src, tag);
    assert_eq!(once, want, "{tag}");
    assert_eq!(
        fmt(&once, &format!("{tag}-again")),
        once,
        "{tag}: idempotent"
    );
}

#[test]
fn a_doc_comment_before_the_first_parameter_stays_before_it() {
    let want = "fn double(\n    /// The number to double.\n    n: i64,\n) -> i64 {\n    n * 2\n}\n";
    fmt_fixed_point(
        "fn double(\n    /// The number to double.\n    n: i64,\n) -> i64 { n * 2 }\n",
        "first",
        want,
    );
    let parsed = karac::parse(want);
    let Some(karac::ast::Item::Function(f)) = parsed.program.items.first() else {
        panic!("expected a function");
    };
    assert_eq!(
        f.params[0].doc_comment.as_deref(),
        Some("The number to double.")
    );
}

#[test]
fn a_doc_comment_before_a_receiver_stays_before_it() {
    fmt_fixed_point(
        "struct P { x: i64 }\nimpl P {\n    fn plus(\n        /// on self\n        self,\n        /// on n\n        n: i64,\n    ) -> i64 { self.x + n }\n}\n",
        "receiver",
        "struct P {\n    x: i64,\n}\n\nimpl P {\n    fn plus(\n        /// on self\n        self,\n        /// on n\n        n: i64,\n    ) -> i64 {\n        self.x + n\n    }\n}\n",
    );
}

#[test]
fn a_parameter_list_with_no_comment_stays_on_one_line() {
    fmt_fixed_point(
        "fn add(a: i64, b: i64) -> i64 { a + b }\n",
        "plain",
        "fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\n",
    );
}
