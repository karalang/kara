//! The Rust-isms the audit in `scripts/mend/rustism_audit.py` found with no
//! machine-applicable fix, each now diagnosed once with a fix `karac fix`
//! applies, leaving a program that checks and runs: `pub(crate)` and its
//! siblings, an unreachable `_ =>` arm, `?` in a `main` returning `()` over
//! an error that is no `Error` (or an `Option`), tuple structs and their
//! `Name(x)` / `.0`, `.collect::<Vec<_>>()`, and `impl<T: Add<Output = T>>`.
//! And the library's error types that did not implement `Display` and
//! `Error` (design.md §10), which `?` could not pass into `AnyError`.

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-rustism-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

/// The diagnostics `check` reports for `src`, then the text `karac fix`
/// leaves (run until it stops changing), which must check clean and print
/// `stdout`.
fn fixes_to(name: &str, src: &str, stdout: &str) -> (String, String) {
    let path = scratch(name, src);
    let before = text(&karac(&["check"], &path));
    for _ in 0..3 {
        let was = std::fs::read_to_string(&path).unwrap();
        let out = karac(&["fix"], &path);
        assert!(out.status.success(), "{}", text(&out));
        if std::fs::read_to_string(&path).unwrap() == was {
            break;
        }
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{fixed}\n{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        stdout,
        "{}",
        text(&run)
    );
    (before, fixed)
}

#[test]
fn rust_scoped_visibility_is_refused_with_kara_visibility_as_its_fix() {
    let src = "pub(crate) fn f() -> i64 { 1 }\npub(super) struct S { pub(crate) x: i64 }\npub(self) fn g() -> i64 { 2 }\nfn main() { let s = S { x: f() }; println(f\"{s.x} {g()}\"); }\n";
    let (before, fixed) = fixes_to("pub_crate.kara", src, "1 2\n");
    assert!(
        before.contains(
            "`pub(crate)` is Rust: a Kāra item is visible to its whole package by default"
        ),
        "{before}"
    );
    assert!(
        before.contains("`pub(self)` is Rust: Kāra limits an item to its directory with `private`"),
        "{before}"
    );
    assert_eq!(
        fixed,
        "fn f() -> i64 { 1 }\nstruct S { x: i64 }\nprivate fn g() -> i64 { 2 }\nfn main() { let s = S { x: f() }; println(f\"{s.x} {g()}\"); }\n"
    );
}

#[test]
fn an_unreachable_wildcard_arm_is_deleted_with_its_line() {
    let src = "enum C { Red, Blue }\nfn name(c: C) -> String {\n    match c {\n        C.Red => \"r\", // the warm one\n        C.Blue => \"b\",\n        _ => \"?\",\n    }\n}\nfn main() { println(name(C.Blue)); }\n";
    let (before, fixed) = fixes_to("wildcard_arm.kara", src, "b\n");
    assert!(before.contains("unreachable match arm"), "{before}");
    assert_eq!(
        fixed,
        "enum C { Red, Blue }\nfn name(c: C) -> String {\n    match c {\n        C.Red => \"r\", // the warm one\n        C.Blue => \"b\",\n    }\n}\nfn main() { println(name(C.Blue)); }\n"
    );
}

#[test]
fn an_unreachable_arm_sharing_a_line_is_cut_after_the_arm_before_it() {
    let src = "enum C { Red, Blue }\nfn name(c: C) -> i64 { match c { C.Red => 1, C.Blue => 2, C.Red => 3 } }\nfn main() { println(name(C.Blue)); }\n";
    let (_, fixed) = fixes_to("arm_inline.kara", src, "2\n");
    assert_eq!(
        fixed,
        "enum C { Red, Blue }\nfn name(c: C) -> i64 { match c { C.Red => 1, C.Blue => 2 } }\nfn main() { println(name(C.Blue)); }\n"
    );
}

#[test]
fn question_in_a_unit_main_declares_the_error_it_passes_up() {
    let src = "fn parse(s: String) -> Result[i64, String] { if s == \"1\" { Ok(1) } else { Err(\"bad\") } }\nfn main() {\n    let n = parse(\"1\")?;\n    println(f\"{n}\");\n}\n";
    let (before, fixed) = fixes_to("question_string.kara", src, "1\n");
    assert!(
        before.contains("declare `fn main() -> Result[(), String]`"),
        "{before}"
    );
    assert_eq!(
        fixed,
        "fn parse(s: String) -> Result[i64, String] { if s == \"1\" { Ok(1) } else { Err(\"bad\") } }\nfn main() -> Result[(), String] {\n    let n = parse(\"1\")?;\n    println(f\"{n}\");\n    Ok(())\n}\n"
    );
}

#[test]
fn question_in_a_unit_main_unwraps_what_cannot_pass_up() {
    let src = "fn first(v: Vec[i64]) -> Option[i64] { v.first().cloned() }\nfn parse(s: String) -> Result[i64, String] { if s == \"1\" { Ok(1) } else { Err(\"bad\") } }\nfn main() {\n    let a = first(Vec[3, 4])?;\n    let n = parse(\"1\")?;\n    let m = i64.parse(\"7\")?;\n    println(f\"{a} {n} {m}\");\n}\n";
    let (before, fixed) = fixes_to("question_mixed.kara", src, "3 1 7\n");
    assert!(
        before.contains("`main` returns `()` and cannot return an `Option`"),
        "{before}"
    );
    assert_eq!(
        fixed,
        "fn first(v: Vec[i64]) -> Option[i64] { v.first().cloned() }\nfn parse(s: String) -> Result[i64, String] { if s == \"1\" { Ok(1) } else { Err(\"bad\") } }\nfn main() -> Result[(), AnyError] {\n    let a = first(Vec[3, 4]).unwrap();\n    let n = parse(\"1\").unwrap();\n    let m = i64.parse(\"7\")?;\n    println(f\"{a} {n} {m}\");\n    Ok(())\n}\n"
    );
}

#[test]
fn a_tuple_struct_gets_named_fields_and_its_uses_follow() {
    let src = "struct Lcg(i64);\nstruct P(pub i64, String);\nimpl Lcg {\n    fn next(mut ref self) -> i64 { self.0 = self.0 * 3 + 1; self.0 }\n}\nfn main() {\n    let mut r = Lcg(5);\n    let p = P(1, \"a\");\n    println(f\"{r.next()} {p.0} {p.1}\");\n}\n";
    let (before, fixed) = fixes_to("tuple_struct.kara", src, "16 1 a\n");
    assert!(
        before.contains("`struct Lcg(..)` is a Rust tuple struct, which Kāra does not have"),
        "{before}"
    );
    assert_eq!(
        fixed,
        "struct Lcg { value: i64 }\nstruct P { pub f0: i64, f1: String }\nimpl Lcg {\n    fn next(mut ref self) -> i64 { self.value = self.value * 3 + 1; self.value }\n}\nfn main() {\n    let mut r = Lcg { value: 5 };\n    let p = P { f0: 1, f1: \"a\" };\n    println(f\"{r.next()} {p.f0} {p.f1}\");\n}\n"
    );
}

#[test]
fn a_distinct_types_dot_zero_is_its_raw_value() {
    let src = "distinct type Meters = i64;\nfn main() {\n    let m = Meters(5);\n    println(f\"{m.0}\");\n}\n";
    let (before, fixed) = fixes_to("distinct_zero.kara", src, "5\n");
    assert!(
        before.contains("`Meters` is a distinct type, not a tuple: its value is `.raw()`"),
        "{before}"
    );
    assert!(fixed.contains("println(f\"{m.raw()}\");"), "{fixed}");
}

#[test]
fn collect_into_a_vec_hole_is_annotated_on_its_binding() {
    let src = "fn main() {\n    let v = Vec[1, 2, 3];\n    let d = v.iter().map(|x| x * 2).collect::<Vec<_>>();\n    let cs = \"ab\".chars().collect::<Vec<_>>();\n    println(f\"{d.len()} {cs.len()}\");\n}\n";
    let (_, fixed) = fixes_to("collect_hole.kara", src, "3 2\n");
    assert_eq!(
        fixed,
        "fn main() {\n    let v = Vec[1, 2, 3];\n    let d: Vec[i64] = v.iter().map(|x| x * 2).collect();\n    let cs: Vec[char] = \"ab\".chars().collect();\n    println(f\"{d.len()} {cs.len()}\");\n}\n"
    );
}

#[test]
fn angle_generics_with_an_associated_binding_inside_are_fixed() {
    let src = "struct F[T] { v: Vec[T] }\nimpl<T: Copy + Add<Output = T>> F<T> {\n    fn first(self) -> T { self.v[0] }\n}\nfn main() { let f = F { v: Vec[4, 5] }; println(f.first()); }\n";
    let (_, fixed) = fixes_to("impl_angle_assoc.kara", src, "4\n");
    assert!(
        fixed.contains("impl[T: Copy + Add[Output = T]] F[T] {"),
        "{fixed}"
    );
}

#[test]
fn library_errors_display_and_pass_into_anyerror() {
    let src = "fn decode(b: Vec[u8]) -> Result[String, AnyError] {\n    let s = String.from_utf8(b)?;\n    Ok(s)\n}\nfn main() {\n    match decode(Vec[255u8]) {\n        Ok(s) => println(s),\n        Err(e) => println(f\"err: {e}\"),\n    }\n    let d: DiscriminantError[i64] = DiscriminantError.OutOfRange { value: 9 };\n    println(f\"{NulError.InteriorNul} | {ChannelError.Full} | {SemaphoreError.Timeout} | {d}\");\n    println(f\"{NetError.Tls(TlsFailure.Handshake)} | {NetError.Other(5)} | {TlsError.Protocol(2)}\");\n}\n";
    let path = scratch("library_errors.kara", src);
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "err: invalid UTF-8 byte\nthe text contains a NUL byte | channel full | timed out waiting for a permit | no variant has the discriminant 9\nTLS: handshake failed | os error 5 | TLS protocol error 2\n",
        "{}",
        text(&run)
    );
}

#[test]
fn a_cli_error_displays_its_message() {
    let src = "import std.cli.{CliError};\nfn main() {\n    let e = CliError { message: \"bad flag\" };\n    let a: AnyError = AnyError.from(e);\n    println(f\"{a}\");\n}\n";
    let path = scratch("cli_error.kara", src);
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "bad flag\n",
        "{}",
        text(&run)
    );
}
