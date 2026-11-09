//! B-2026-11-09-27, found porting the mini_nestedtext app: a `let … else`
//! whose pattern binds part of an element (`let Json.Array(queries) =
//! row[3] else { … };`) was refused with "cannot move a non-`Copy` value
//! out of a collection element" and no fix, where the same element taken
//! by a plain `let` already got the borrow `ref row[3]` when the binding is
//! only read. The let-else now gets the same repair, and so does a struct
//! or variant pattern in a plain `let`; a binding that is moved on keeps
//! the refusal without it.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn check_err(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-msg-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let check = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&check.stderr).into_owned()
}

fn fix_until_clean(name: &str, src: &str) -> (String, String) {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    let file = path.to_str().unwrap().to_string();
    std::fs::write(&path, src).unwrap();
    for _ in 0..3 {
        let _ = karac(&["fix", &file]);
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    let check = karac(&["check", &file]);
    assert!(
        check.status.success(),
        "{name}: {}\n{fixed}",
        String::from_utf8_lossy(&check.stderr)
    );
    let _ = std::fs::remove_file(&path);
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    (fixed, out)
}

const SHAPES: &str = "enum Shape {
    Many(Vec[String]),
    One(String),
}

struct Pair {
    name: String,
    n: i64,
}
";

#[test]
fn a_let_else_over_an_element_read_only_is_borrowed() {
    let src = format!(
        "{SHAPES}
fn count(shapes: Vec[Shape]) -> i64 {{
    let Shape.Many(xs) = shapes[0] else {{
        return 0;
    }};
    xs.len()
}}

fn main() {{
    let v = [Shape.Many([\"a\".to_string(), \"b\".to_string()].to_vec()), Shape.One(\"c\".to_string())].to_vec();
    println(count(v));
}}
"
    );
    let err = check_err("let-else-element", &src);
    assert!(
        err.contains("the binding is only read, so the fix borrows the element"),
        "{err}"
    );
    let (fixed, out) = fix_until_clean("let-else-element", &src);
    assert!(
        fixed.contains("let Shape.Many(xs) = ref shapes[0] else {"),
        "{fixed}"
    );
    assert_eq!(out, "2\n");
}

#[test]
fn a_let_else_over_a_field_of_an_element_read_only_is_borrowed() {
    let src = "struct Row {
    cells: Option[Vec[String]],
}

fn width(rows: Vec[Row]) -> i64 {
    let Some(cells) = rows[1].cells else {
        return -1;
    };
    cells.len()
}

fn main() {
    let rows = [Row { cells: None }, Row { cells: Some([\"x\".to_string()].to_vec()) }].to_vec();
    println(width(rows));
}
";
    let err = check_err("let-else-field", src);
    assert!(
        err.contains("the binding is only read, so the fix borrows the field"),
        "{err}"
    );
    let (fixed, out) = fix_until_clean("let-else-field", src);
    assert!(
        fixed.contains("let Some(cells) = ref rows[1].cells else {"),
        "{fixed}"
    );
    assert_eq!(out, "1\n");
}

#[test]
fn a_let_else_whose_binding_is_moved_on_keeps_the_refusal() {
    let src = format!(
        "{SHAPES}
fn take(xs: own Vec[String]) -> i64 {{
    xs.len()
}}

fn count(shapes: Vec[Shape]) -> i64 {{
    let Shape.Many(xs) = shapes[0] else {{
        return 0;
    }};
    take(xs)
}}

fn main() {{
    let v = [Shape.Many([\"a\".to_string()].to_vec())].to_vec();
    println(count(v));
}}
"
    );
    let err = check_err("let-else-moved", &src);
    assert!(err.contains("out of a collection element"), "{err}");
    assert!(!err.contains("so the fix borrows"), "{err}");
}

#[test]
fn a_struct_pattern_let_over_an_element_read_only_is_borrowed() {
    let src = format!(
        "{SHAPES}
fn describe(pairs: Vec[Pair]) -> String {{
    let Pair {{ name, n }} = pairs[0];
    f\"{{name}} {{n + 1}}\"
}}

fn main() {{
    let v = [Pair {{ name: \"p\".to_string(), n: 4 }}].to_vec();
    println(describe(v));
}}
"
    );
    let (fixed, out) = fix_until_clean("let-struct-element", &src);
    assert!(
        fixed.contains("let Pair { name, n } = ref pairs[0];"),
        "{fixed}"
    );
    assert_eq!(out, "p 5\n");
}
