//! Two machine fixes, each pinned through the Mend loop: `karac check`
//! names the gap, `karac fix` repairs it in one or more rounds, and the
//! fixed program checks clean and runs.
//!
//! B-2026-11-18-76: E0509 for a struct with a `Str` field stored where no
//! view may go (a `Mutex`'s value, a `shared` value's field) names that
//! field and gives it the owned type `String`; a shorthand field built
//! from a `Str` (`E { kind }`) then takes `kind: kind.to_string()`.
//!
//! B-2026-11-18-77: E0208 for `==` on `Result[T, E]` whose `E` lacks
//! `PartialEq` attaches `#[derive(PartialEq)]` on the program's `E`.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// Check, fix until nothing is left, check clean, run: the fixed text
/// and its stdout.
fn mend(name: &str, src: &str, want_code: &str, want_msg: &str) -> (String, String) {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        err.contains(want_code) && err.contains(want_msg),
        "{name}: {err}"
    );
    for _ in 0..3 {
        karac(&["fix", file]);
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    let check = karac(&["check", file]);
    assert!(
        check.status.success(),
        "{name}: {}\n{fixed}",
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(
        run.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    (fixed, String::from_utf8_lossy(&run.stdout).into_owned())
}

#[test]
fn str_field_stored_in_a_mutex_becomes_a_string() {
    let (fixed, out) = mend(
        "b76-mutex",
        "struct E { name: String, kind: Str }
struct Lab { ex: Mutex[Vec[(i64, E)]] }
fn main() {
    let lab = Lab { ex: Mutex.new(Vec.new()) };
    let e = E { name: \"a\", kind: \"mx\" };
    lab.ex.lock().value.push((1, e));
    println(f\"{lab.ex.lock().value.len()}\");
}
",
        "E0509",
        "a view (`(i64, E)`, through its field `E.kind: Str`) is stored in the value a `Mutex` \
         holds, where a view may not go; give `E.kind` an owned type (`String`)",
    );
    assert!(
        fixed.contains("struct E { name: String, kind: String }"),
        "{fixed}"
    );
    assert_eq!(out, "1\n");
}

#[test]
fn str_field_stored_in_a_shared_value_becomes_a_string_and_its_shorthand_converts() {
    let (fixed, out) = mend(
        "b76-shared",
        "struct E { name: String, kind: Str }
shared struct Lab { ex: Vec[E] }
fn mk(n: i64, kind: Str) -> E { E { name: f\"n{n}\", kind } }
fn main() {
    let lab = Lab { ex: Vec.new() };
    let k: Str = \"mx\";
    lab.ex.push(mk(1, k));
    println(f\"{lab.ex.len()} {lab.ex[0].kind}\");
}
",
        "E0509",
        "through its field `E.kind: Str`",
    );
    assert!(
        fixed.contains("struct E { name: String, kind: String }"),
        "{fixed}"
    );
    assert!(
        fixed.contains("E { name: f\"n{n}\", kind: kind.to_string() }"),
        "{fixed}"
    );
    assert_eq!(out, "1 mx\n");
}

#[test]
fn eq_on_a_result_derives_partial_eq_on_its_error_type() {
    let (fixed, out) = mend(
        "b77-derive-arg",
        "enum E { Bad }
fn f(x: i64) -> Result[i64, E] { if x > 0 { Ok(x) } else { Err(E.Bad) } }
fn main() { println(f\"{f(1) == Ok(1)} {f(0) == Ok(0)}\"); }
",
        "E0208",
        "holds only where 'E' implements PartialEq; add #[derive(PartialEq)] to 'E'",
    );
    assert!(
        fixed.starts_with("#[derive(PartialEq)]\nenum E { Bad }"),
        "{fixed}"
    );
    assert_eq!(out, "true false\n");
}
