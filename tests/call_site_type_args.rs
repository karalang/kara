//! design.md §8: `[T]` names and declares types and is never applied at a
//! call site. Types at a call come from a qualified receiver
//! (`u32.size_of()`, `Vec[i64].new()`) or an annotation. `karac check`
//! reports each older call-site form with the spelling that replaces it.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn write(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-tyargs-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn layout_queries_take_a_qualified_receiver_and_return_i64() {
    let src = r#"
struct P { a: i64, b: u8 }
fn main() {
    let s: i64 = u32.size_of();
    let a: i64 = i64.align_of();
    let v = Vec[i64].size_of();
    println(f"{s} {a} {P.size_of()} {v > 0}");
}
"#;
    let path = write("layout", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac()
        .args(["run", "--interp"])
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "4 8 16 true\n");
}

#[test]
fn check_names_the_spelling_that_replaces_each_call_site_form() {
    let src = r#"
fn pair[A, B](a: A, b: B) -> (A, B) { (a, b) }
fn make[const N: i64]() -> i64 { N }
fn main() {
    let q = pair[i64, bool](1, true);
    let s = size_of[u32]();
    let p = ptr.null[u8]();
    let n = make[4]();
    println(f"{q.0} {s} {n}");
}
"#;
    let path = write("forms", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    for e in [
        "`pair` cannot take type arguments at the call",
        "write `u32.size_of()`",
        "write `ptr.null()` and annotate the result's type",
        "`make` cannot take a const argument at the call",
    ] {
        assert!(err.contains(e), "expected `{e}` in: {err}");
    }
    assert_eq!(
        err.matches("no call-site type arguments").count(),
        4,
        "{err}"
    );
}
