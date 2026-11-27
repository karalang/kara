//! A cast of an operand nothing has typed: after the error that left it so
//! (`s.bytes().fold(..)` with the `.iter()` left out, whose closure parameter
//! nothing then types) the cast adds no second error, and alone it says what
//! to annotate rather than naming an internal type variable
//! (B-2026-11-26-81).

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

fn file(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-open-cast-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    (dir, f)
}

fn karac(args: &[&str], f: &std::path::Path) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(f)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// `karac check`'s verdict and output.
fn check(src: &str) -> (bool, String) {
    let (dir, f) = file(src);
    let (ok, out, err) = karac(&["check"], &f);
    let _ = std::fs::remove_dir_all(&dir);
    (ok, format!("{out}{err}"))
}

/// The file `karac fix` writes for `src`.
fn fixed(src: &str) -> String {
    let (dir, f) = file(src);
    let _ = karac(&["fix"], &f);
    let out = std::fs::read_to_string(&f).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn a_cast_of_an_open_operand_names_no_type_variable() {
    let src =
        "fn digest(s: String) -> i64 { s.bytes().fold(0, |check, b| check * 31 + b as i64) }\n\
               fn main() { println(digest(\"ab\".to_string())); }\n";
    let (ok, out) = check(src);
    assert!(!ok, "{out}");
    assert_eq!(out.matches("error[").count(), 1, "{out}");
    assert!(out.contains("E0236") && !out.contains("?T"), "{out}");
    let after = fixed(src);
    assert!(after.contains("s.bytes().iter().fold("), "{after}");
    let (ok, out) = check(&after);
    assert!(ok, "{out}\n{after}");
    let (ok, out) = check("fn main() { let f = |b| b as i64; println(1); }\n");
    assert!(!ok && !out.contains("?T"), "{out}");
    assert!(
        out.contains("cannot cast a value whose type nothing has fixed yet to 'i64'"),
        "{out}"
    );
}
