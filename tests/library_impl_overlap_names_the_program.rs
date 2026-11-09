//! A program's inherent impl on a library type that redefines a library
//! method is the program's error, not the library's. The library's own
//! `impl Option[Ordering]` used to be refused as the overlapping one, at a
//! line past the program's end, under "in the standard library: " repeated
//! once per pass, beside the program's own coherence error.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-impl-overlap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn assert_one_error_on_line_one(stderr: &str) {
    let errors: Vec<&str> = stderr.lines().filter(|l| l.starts_with("error[")).collect();
    assert_eq!(errors.len(), 1, "{stderr}");
    assert!(errors[0].contains(".kara:1:1:"), "{stderr}");
    assert!(
        errors[0].contains("a type this program does not declare"),
        "{stderr}"
    );
    assert!(!stderr.contains("in the standard library"), "{stderr}");
}

#[test]
fn program_impl_option_ordering_is_refused_at_the_program() {
    let (ok, err) = check(
        "ord.kara",
        "impl Option[Ordering] {\n    fn is_lt(self) -> bool { match self { Some(Ordering.Less) => true, _ => false } }\n}\nfn main() {}\n",
    );
    assert!(!ok);
    assert_one_error_on_line_one(&err);
}

#[test]
fn program_impl_option_redefining_is_some_is_refused_at_the_program() {
    let (ok, err) = check(
        "some.kara",
        "impl Option[i64] {\n    fn is_some(self) -> bool { true }\n}\nfn main() {}\n",
    );
    assert!(!ok);
    assert_one_error_on_line_one(&err);
}
