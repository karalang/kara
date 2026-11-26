//! E0200 on an untyped `.try_into()` suggests an annotation that parses:
//! the error type written out (`Result[Target, String]` for the integers,
//! whose narrowing `try_from` fails with a `String`), or a named `E` when
//! the candidate impls disagree. It wrote `Result[Target, _]`, which a copy
//! into the program refused E0001 "Expected type, found `_`"
//! (B-2026-11-26-72).

use std::process::Command;

fn karac(args: &[&str], name: &str, src: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-try-into-hint-{name}-{}-{}",
        args[0].trim_start_matches('_'),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn check_text(name: &str, src: &str) -> (bool, String) {
    let out = karac(&["check"], name, src);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn try_into_hint_names_the_integers_error_type() {
    let (ok, text) = check_text(
        "ints",
        "fn main() { let x: i64 = 300; let r = x.try_into(); println(r.is_ok()); }\n",
    );
    assert!(!ok, "ints: accepted:\n{text}");
    assert!(
        text.contains("`let r: Result[Target, String] = v.try_into();`"),
        "ints: {text}"
    );
    assert!(!text.contains("Result[Target, _]"), "ints: {text}");
}

#[test]
fn try_into_hint_as_written_checks() {
    let (ok, text) = check_text(
        "applied",
        "fn main() { let x: i64 = 300; let r: Result[u8, String] = x.try_into(); println(r.is_ok()); }\n",
    );
    assert!(ok, "applied: {text}");
}

#[test]
fn try_into_hint_with_disagreeing_errors_names_a_placeholder() {
    let (ok, text) = check_text(
        "mixed",
        "struct Small { n: i64 }\nenum TooBig { Over }\nimpl TryFrom[i64] for Small {\n    type Error = TooBig;\n    fn try_from(n: i64) -> Result[Small, TooBig] { if n < 10 { Ok(Small { n: n }) } else { Err(TooBig.Over) } }\n}\nfn main() { let x: i64 = 3; let r = x.try_into(); println(r.is_ok()); }\n",
    );
    assert!(!ok, "mixed: accepted:\n{text}");
    assert!(
        text.contains("`let r: Result[Target, E] = v.try_into();` with `E` the impl's error type"),
        "mixed: {text}"
    );
}
