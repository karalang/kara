//! `pure fn` in an `extern` block (design review 2026-10-07): the foreign
//! function has no effects. Only extern functions take `pure`.

use karac::ast::{ExternItem, Item};
use std::process::Command;

fn extern_fn_is_pure(src: &str) -> bool {
    let r = karac::parse(src);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let Item::ExternBlock(b) = &r.program.items[0] else {
        panic!("expected an extern block");
    };
    let ExternItem::Function(f) = &b.items[0] else {
        panic!("expected an extern fn");
    };
    f.is_pure
}

#[test]
fn pure_marks_an_extern_function() {
    assert!(extern_fn_is_pure(
        "unsafe extern \"C\" { pure fn abs(x: i32) -> i32; }"
    ));
    assert!(!extern_fn_is_pure(
        "unsafe extern \"C\" { fn abs(x: i32) -> i32; }"
    ));
    assert!(extern_fn_is_pure(
        "unsafe extern \"C\" { #[link_name(\"abs\")] pub pure fn abs2(x: i32) -> i32; }"
    ));
}

#[test]
fn pure_is_rejected_off_extern_and_with_a_with_clause() {
    let r = karac::parse("pure fn f() {}");
    assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
    assert!(r.errors[0]
        .message
        .contains("only functions in an `extern` block"));

    let r = karac::parse("unsafe extern \"C\" { pure fn g() with blocks; }");
    assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
    assert!(r.errors[0].message.contains("takes no `with` clause"));
}

/// `pure` removes the ABI's default `blocks`, so a public caller needs no
/// `with` clause; without it the same program is an effect error.
#[test]
fn pure_extern_has_no_effects() {
    let check = |pure: &str| {
        let src = format!(
            "/// # Safety\n/// abs has no preconditions.\nunsafe extern \"C\" {{\n    {pure}fn abs(x: i32) -> i32;\n}}\n\n\
             pub fn f() -> i32 {{\n    // Safety: no preconditions.\n    unsafe {{ abs(-3) }}\n}}\n\n\
             fn main() {{\n    println(f\"{{f()}}\");\n}}\n"
        );
        let dir =
            std::env::temp_dir().join(format!("karac-pure-{}-{}", std::process::id(), pure.len()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.kara");
        std::fs::write(&path, src).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["check", path.to_str().unwrap()])
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let (ok, err) = check("pure ");
    assert!(ok, "pure extern should need no `with`: {err}");
    let (ok, err) = check("");
    assert!(!ok);
    assert!(err.contains("performs effects [blocks]"), "{err}");
}

#[test]
fn formatter_keeps_pure() {
    let src = "unsafe extern \"C\" {\n    pure fn abs(x: i32) -> i32;\n}\n";
    let r = karac::parse(src);
    let out = karac::formatter::format_program(&r.program);
    assert!(out.contains("pure fn abs"), "{out}");
}
