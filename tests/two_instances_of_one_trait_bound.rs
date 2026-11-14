//! B-2026-11-13-47: a type parameter bounded by two instances of one generic
//! trait (`X: Conv[String] + Conv[f64]`) kept only the first, so
//! `Conv[f64].conv(x)` was typed by `Conv[String]` and refused with
//! "expected 'f64', found 'String'". Each instance is now its own candidate:
//! the qualified call picks the one it names, and a bare `x.conv()` is
//! ambiguous, naming both.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-two-instances-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

const CONV: &str = "trait Conv[T] { fn conv(self) -> T; }\n\
    impl Conv[String] for i64 { fn conv(self) -> String { f\"n{self}\" } }\n\
    impl Conv[f64] for i64 { fn conv(self) -> f64 { self as f64 } }\n";

#[test]
fn qualified_call_picks_the_named_instance_of_a_bound() {
    let src = format!(
        "{CONV}fn both[X: Conv[String] + Conv[f64]](x: X) -> String {{\n    \
         let s: String = Conv[String].conv(x);\n    \
         let f: f64 = Conv[f64].conv(x);\n    \
         f\"{{s}} {{f}}\"\n}}\n\
         fn main() {{ println(both(5)); }}\n"
    );
    let (dir, path) = fixture("qualified", &src);
    let check = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        check.status.success(),
        "check must accept:\n{src}\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "n5 5.0\n");
}

#[test]
fn bare_call_on_two_instances_of_a_bound_is_ambiguous() {
    let src = format!(
        "{CONV}fn one[X: Conv[String] + Conv[f64]](x: X) -> f64 {{ x.conv() }}\n\
         fn main() {{ println(one(5)); }}\n"
    );
    let (dir, path) = fixture("bare", &src);
    let check = karac().arg("check").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&check.stderr);
    assert!(!check.status.success(), "check must refuse:\n{src}");
    assert!(
        err.contains("ambiguous method 'conv'") && err.contains("`Conv[String]`, `Conv[f64]`"),
        "{err}"
    );
}
