//! `karac fmt` keeps a comment beside the parameter or call argument it
//! follows: a list with a comment between two of its elements, or after the
//! last, is laid out one element per line, so the comment does not move into
//! the body or to the end of the statement (B-2026-11-07-44). A list with no
//! such comment stays on one line, and the output formats to itself.

use std::process::{Command, Stdio};

fn fmt(src: &str, tag: &str) -> String {
    let path =
        std::env::temp_dir().join(format!("karac-fmt-lists-{tag}-{}.kara", std::process::id()));
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
fn a_comment_after_a_parameter_or_argument_stays_beside_it() {
    fmt_fixed_point(
        "fn add(a: i64, // first\n        b: i64 /* second */) -> i64 { a + b }\nfn main() {\n    println(add(1, // one\n        2));\n}\n",
        "params-args",
        "fn add(\n    a: i64, // first\n    b: i64, /* second */\n) -> i64 {\n    a + b\n}\n\nfn main() {\n    println(add(\n        1, // one\n        2,\n    ));\n}\n",
    );
}

#[test]
fn a_method_receiver_and_method_call_args_keep_their_comments() {
    fmt_fixed_point(
        "struct P { x: i64 }\nimpl P {\n    fn at(self, i: i64, // index\n          j: i64) -> i64 { self.x + i + j }\n}\nfn main() {\n    let p = P { x: 1 };\n    println(p.at(2, // two\n        3));\n}\n",
        "method",
        "struct P {\n    x: i64,\n}\n\nimpl P {\n    fn at(\n        self,\n        i: i64, // index\n        j: i64,\n    ) -> i64 {\n        self.x + i + j\n    }\n}\n\nfn main() {\n    let p = P {\n        x: 1,\n    };\n    println(p.at(\n        2, // two\n        3,\n    ));\n}\n",
    );
}

#[test]
fn a_list_without_a_comment_between_elements_stays_on_one_line() {
    fmt_fixed_point(
        "// lead\nfn add(a: i64, b: i64) -> i64 { a + b } // after\nfn main() { println(add(1, 2)); // end\n}\n",
        "flat",
        "// lead\nfn add(a: i64, b: i64) -> i64 {\n    a + b\n} // after\n\nfn main() {\n    println(add(1, 2)); // end\n}\n",
    );
}
