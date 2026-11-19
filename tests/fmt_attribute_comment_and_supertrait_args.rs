//! Two `karac fmt` gaps found by a round trip of design.md's examples:
//! - B-2026-11-19-47: a comment ending an attribute's line
//!   (`#[derive(Copy, Clone)] // why`) moved onto the end of the previous
//!   item (`} // why`), where it described the wrong thing, and two such
//!   comments in a row did not format to a fixed point. It now goes on its
//!   own line above the attribute, for an item, a field and a variant.
//! - B-2026-11-19-48: a supertrait bound lost its arguments:
//!   `trait PartialOrd[Rhs = Self]: PartialEq[Rhs]` came out as
//!   `: PartialEq`, a different trait.

use std::process::{Command, Stdio};

fn fmt(src: &str, tag: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "karac-fmt-attr-super-{tag}-{}.kara",
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
    assert_eq!(once, want, "{src}");
    assert_eq!(
        fmt(&once, &format!("{tag}-again")),
        once,
        "not a fixed point"
    );
}

#[test]
fn a_comment_ending_an_attribute_line_goes_above_the_attribute() {
    fmt_fixed_point(
        "struct A {\n    x: i64,\n}\n\n#[derive(Copy, Clone)] // note on B\nstruct B {\n    y: i64,\n}\n",
        "item",
        "struct A {\n    x: i64,\n}\n\n// note on B\n#[derive(Copy, Clone)]\nstruct B {\n    y: i64,\n}\n",
    );
    // The design.md example: a trailing comment on the item before, then one
    // on the attribute line.
    fmt_fixed_point(
        "#[derive(Copy, Clone)]\nstruct Point { x: f64, y: f64 }          // OK: both fields are Copy\n\n\
         #[derive(Copy, Clone)]                   // ERROR: name is a String, which is not Copy\n\
         struct User { id: u64, name: String }\n",
        "design",
        "#[derive(Copy, Clone)]\nstruct Point {\n    x: f64,\n    y: f64,\n} // OK: both fields are Copy\n\n\
         // ERROR: name is a String, which is not Copy\n#[derive(Copy, Clone)]\nstruct User {\n    id: u64,\n    name: String,\n}\n",
    );
    fmt_fixed_point(
        "struct S {\n    a: i64,\n    #[deprecated] // old field\n    b: i64,\n}\n\nenum E {\n    A,\n    #[deprecated] // gone\n    B,\n}\n",
        "member",
        "struct S {\n    a: i64,\n    // old field\n    #[deprecated]\n    b: i64,\n}\n\nenum E {\n    A,\n    // gone\n    #[deprecated]\n    B,\n}\n",
    );
    // A trailing comment on any other line stays where it is.
    fmt_fixed_point(
        "fn main() {\n    let a = 1; // trailing\n    println(a);\n}\n",
        "plain",
        "fn main() {\n    let a = 1; // trailing\n    println(a);\n}\n",
    );
}

#[test]
fn a_supertrait_bound_keeps_its_arguments() {
    fmt_fixed_point(
        "trait PartialEqX[Rhs = Self] { fn eqx(self, other: Rhs) -> bool; }\n\
         trait PartialOrdX[Rhs = Self]: PartialEqX[Rhs] { fn lt(self, other: Rhs) -> bool; }\n\
         marker trait Both[T]: PartialEqX[T] + PartialOrdX[T];\n",
        "super",
        "trait PartialEqX[Rhs = Self] {\n    fn eqx(self, other: Rhs) -> bool;\n}\n\n\
         trait PartialOrdX[Rhs = Self]: PartialEqX[Rhs] {\n    fn lt(self, other: Rhs) -> bool;\n}\n\n\
         marker trait Both[T]: PartialEqX[T] + PartialOrdX[T];\n",
    );
}
