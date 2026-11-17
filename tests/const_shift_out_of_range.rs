//! A constant shift amount that is negative or at least the shifted type's
//! width is a compile-time error, as constant overflow is: design.md says
//! such a shift traps, so a constant one is known to (B-2026-11-16-323).
//! The amount is judged at the type the shifted value ends with: a
//! literal's context type, its default `i64`, or a binding's type.

use std::process::Command;

fn check(name: &str, body: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-shift-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(
        &file,
        format!("fn main() {{\n    {body}\n    println(y);\n}}\n"),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text)
}

fn refused(name: &str, body: &str, amount: i64, ty: &str) {
    let (ok, text) = check(name, body);
    assert!(!ok, "`{body}` passed check:\n{text}");
    let want = format!("this shift amount is {amount}, which is out of range for '{ty}'");
    assert!(
        text.contains(&want),
        "`{body}`: expected `{want}`, got:\n{text}"
    );
    assert_eq!(
        text.matches("this shift amount").count(),
        1,
        "`{body}` reported twice:\n{text}"
    );
}

fn accepted(name: &str, body: &str) {
    let (ok, text) = check(name, body);
    assert!(ok, "`{body}` was refused:\n{text}");
}

#[test]
fn a_constant_shift_past_a_literal_context_type_is_refused() {
    refused("i8", "let y: i8 = 1 << 8;", 8, "i8");
    refused("u8-10", "let y: u8 = 1 << 10;", 10, "u8");
    refused("i32", "let y: i32 = 1 << 32;", 32, "i32");
    refused("u8-shr", "let y: u8 = 255 >> 8;", 8, "u8");
    refused("i128", "let y: i128 = 1 << 128;", 128, "i128");
}

#[test]
fn a_constant_shift_past_the_default_i64_is_refused() {
    refused("i64-ann", "let y: i64 = 1 << 64;", 64, "i64");
    refused("i64-default", "let y = 1 << 64;", 64, "i64");
    refused("nested", "let y = (1 << 70) + 1;", 70, "i64");
    refused("negative", "let y = 1 >> -1;", -1, "i64");
}

#[test]
fn a_constant_shift_of_a_typed_value_is_refused() {
    refused("binding", "let a: i8 = 1; let y = a << 8;", 8, "i8");
    refused("suffixed", "let a: u64 = 1; let y = a >> 64u32;", 64, "u64");
    refused("peer", "let a: u8 = 3; let y = a + (1 << 9);", 9, "u8");
}

#[test]
fn shifts_inside_the_width_or_by_a_variable_pass_check() {
    accepted("i8-7", "let y: i8 = 1 << 7;");
    accepted("i64-63", "let y = 1 << 63;");
    accepted("i128-100", "let y: i128 = 1 << 100;");
    accepted("u128-peer", "let a: u128 = 3; let y = a + (1 << 100);");
    accepted("i16-chain", "let y: i16 = 1 << 15 >> 15;");
    accepted("variable", "let a: i64 = 1; let s = 70; let y = a << s;");
}
