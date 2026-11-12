//! `karac check` runs the MIR move and borrow checks over every function
//! of the program's own that it can build on its own, not only those `main`
//! reaches, and over a library, which has no `main` (B-2026-11-04-56). A
//! view of a function's own local escaping (design.md §5 Strings, core
//! semantics §5.6) is refused where it is written, not at the first
//! caller's build. A function with type parameters is built only where a
//! call gives them, and what the builder does not lower yet in a function
//! nothing reaches is left to the build.

use std::path::{Path, PathBuf};
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-check-unreached-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `karac check` on `path` (a file, or a package directory when `None`).
fn check(dir: &Path, path: Option<&Path>) -> (bool, String) {
    let mut cmd = karac();
    cmd.arg("check").current_dir(dir);
    if let Some(p) = path {
        cmd.arg(p);
    }
    let out = cmd.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

fn check_source(tag: &str, src: &str) -> (bool, String) {
    let dir = scratch(tag);
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    let r = check(&dir, Some(&path));
    let _ = std::fs::remove_dir_all(&dir);
    r
}

const ESCAPING_VIEW: &str =
    "pub fn bad() -> Str {\n    let s = \"abc\".to_string();\n    s.as_str()\n}\n";

#[test]
fn check_refuses_a_borrow_error_in_a_function_main_never_calls() {
    let (ok, out) = check_source("uncalled", &format!("{ESCAPING_VIEW}\nfn main() {{ }}\n"));
    assert!(!ok, "check must refuse: {out}");
    assert!(out.contains("3:5") && out.contains("E0516"), "{out}");
}

#[test]
fn check_refuses_a_borrow_error_in_an_uncalled_method() {
    let src = "struct S { n: i64 }\n\nimpl S {\n    fn label(ref self) -> Str {\n        let s = self.n.to_string();\n        s.as_str()\n    }\n}\n\nfn main() {\n    let s = S { n: 1 };\n    println(s.n);\n}\n";
    let (ok, out) = check_source("method", src);
    assert!(!ok, "check must refuse: {out}");
    assert!(out.contains("6:9") && out.contains("E0516"), "{out}");
}

#[test]
fn check_refuses_a_borrow_error_in_a_library_package() {
    let dir = scratch("lib");
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"lib56\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.kara"), ESCAPING_VIEW).unwrap();
    let (ok, out) = check(&dir, None);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok, "check must refuse: {out}");
    assert!(
        // The host's separator: `src\lib.kara` on Windows.
        out.replace('\\', "/").contains("src/lib.kara:3:5") && out.contains("E0516"),
        "{out}"
    );
}

#[test]
fn check_passes_sound_uncalled_functions_and_generic_ones() {
    let src = "pub fn good() -> String {\n    let s = \"abc\".to_string();\n    s\n}\n\nfn first[T: Clone](v: ref Vec[T]) -> T {\n    v[0].clone()\n}\n\nstruct S { n: i64 }\n\nimpl S {\n    fn get(ref self) -> i64 { self.n }\n}\n\nfn main() { }\n";
    let (ok, out) = check_source("sound", src);
    assert!(ok, "check must pass: {out}");
}
