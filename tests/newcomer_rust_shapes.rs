//! B-2026-11-07-41/42/43 — a Rust or Go developer's day-one shapes get a
//! message that names the Kāra form, and a machine fix only where the
//! repair is unambiguous: `std::thread::spawn` (no fix: there is no free
//! `spawn`), `Self { x }` (the impl's type name), `a + b;` as a function's
//! last statement (the `;` removed), and `?` in a `fn main()` returning
//! `()` (`-> Result[(), AnyError]` and `Ok(())`). Each fixed program checks.

use std::process::Command;

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn file(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-newcomer-rs-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

/// The message `karac check` gives, then the program `karac fix` leaves,
/// which must check.
fn check_then_fix(tag: &str, src: &str, message: &str, fixed: &str) {
    let path = file(tag, src);
    let p = path.to_str().unwrap();
    let (ok, out) = karac(&["check", p]);
    assert!(
        !ok && out.contains(message),
        "{tag}: want `{message}`:\n{out}"
    );
    for _ in 0..3 {
        karac(&["fix", p]);
    }
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, fixed, "{tag}: karac fix");
    let (ok, out) = karac(&["check", p]);
    assert!(ok, "{tag}: the fixed program checks:\n{out}");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// A message with no machine fix: `karac fix` leaves the file as it was.
fn check_without_fix(tag: &str, src: &str, message: &str) {
    let path = file(tag, src);
    let p = path.to_str().unwrap();
    let (ok, out) = karac(&["check", p]);
    assert!(
        !ok && out.contains(message),
        "{tag}: want `{message}`:\n{out}"
    );
    karac(&["fix", p]);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        src,
        "{tag}: no fix"
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn rust_thread_spawn_names_task_group_and_par_without_a_fix() {
    check_without_fix(
        "spawn",
        "fn main() { let h = std::thread::spawn(|| 1); println(h.join().unwrap()); }\n",
        "`std::thread::spawn` is Rust: Kāra has no free `spawn`",
    );
    check_without_fix(
        "spawn-short",
        "fn main() { let h = thread::spawn(|| 1); println(h.join()); }\n",
        "`let g = TaskGroup.new(); let h = g.spawn(|| ..);`",
    );
    // A `std::` path that is not a spawn keeps its fix.
    check_then_fix(
        "max",
        "fn main() { println(std::cmp::max(1, 2)); }\n",
        "is a Rust path",
        "fn main() { println(max(1, 2)); }\n",
    );
}

#[test]
fn self_struct_literal_is_written_with_the_impl_type_name() {
    check_then_fix(
        "self-lit",
        "struct P { x: i64 }\nimpl P { fn new(x: i64) -> Self { Self { x } } }\nfn main() { let p = P.new(1); println(p.x); }\n",
        "`Self { .. }` is Rust's struct literal: Kāra writes the type's name, `P { .. }`",
        "struct P { x: i64 }\nimpl P { fn new(x: i64) -> Self { P { x } } }\nfn main() { let p = P.new(1); println(p.x); }\n",
    );
    check_then_fix(
        "self-lit-generic",
        "struct Pair[T] { a: T, b: T }\nimpl[T] Pair[T] {\n    fn new(a: own T, b: own T) -> Self { Self { a: a, b } }\n}\nfn main() { let p = Pair.new(1, 2); println(p.a + p.b); }\n",
        "`Pair { .. }`",
        "struct Pair[T] { a: T, b: T }\nimpl[T] Pair[T] {\n    fn new(a: own T, b: own T) -> Self { Pair { a: a, b } }\n}\nfn main() { let p = Pair.new(1, 2); println(p.a + p.b); }\n",
    );
}

#[test]
fn a_trailing_semicolon_on_the_return_value_is_removed() {
    check_then_fix(
        "semi",
        "fn add(a: i64, b: i64) -> i64 { a + b; }\nfn main() { println(add(1, 2)); }\n",
        "the last statement `a + b;` has type 'i64': remove the `;` to return it",
        "fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() { println(add(1, 2)); }\n",
    );
    // A last statement of another type is not the return value: no fix.
    check_without_fix(
        "semi-other",
        "fn add(a: i64, b: i64) -> i64 { println(a + b); }\nfn main() { println(add(1, 2)); }\n",
        "can complete without producing a value",
    );
}

#[test]
fn question_mark_in_unit_main_declares_a_result_main() {
    check_then_fix(
        "q-main",
        "fn main() { let n = i64.parse(\"12\")?; println(n); }\n",
        "`main` returns `()`; to use `?` here, declare `fn main() -> Result[(), AnyError]` and end it with `Ok(())`",
        "fn main() -> Result[(), AnyError] { let n = i64.parse(\"12\")?; println(n); Ok(()) }\n",
    );
    check_then_fix(
        "q-main-lines",
        "fn main() {\n    let n = i64.parse(\"12\")?;\n    let m = i64.parse(\"3\")?;\n    println(n + m);\n}\n",
        "`main` returns `()`",
        "fn main() -> Result[(), AnyError] {\n    let n = i64.parse(\"12\")?;\n    let m = i64.parse(\"3\")?;\n    println(n + m);\n    Ok(())\n}\n",
    );
    check_then_fix(
        "q-main-tail",
        "fn main() {\n    let n = i64.parse(\"12\")?;\n    println(n)\n}\n",
        "`main` returns `()`",
        "fn main() -> Result[(), AnyError] {\n    let n = i64.parse(\"12\")?;\n    println(n);\n    Ok(())\n}\n",
    );
    // Another function gets the advice, not an edit.
    check_without_fix(
        "q-helper",
        "fn helper() { let n = i64.parse(\"12\")?; println(n); }\nfn main() { helper(); }\n",
        "declare the function's return type as `Result[T, E]`",
    );
}
