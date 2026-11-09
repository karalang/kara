//! A program's impl of a library trait for a library type is allowed (the
//! application is exempt from the orphan rule) unless the library already
//! has it: `impl Clone for String` replaced the library's `clone`
//! (B-2026-11-09-31). And the builtin traits' qualified calls:
//! `Clone.clone(s)` is `s.clone()`, and `Default.default()` names no type,
//! so it is refused with a fix naming the expected one (B-2026-11-09-32).
//! Both were typed as errors with no diagnostic, and MIR refused the
//! program as "a type that failed to check is not lowered yet".

use std::process::{Command, Output};

fn karac(args: &[&str], name: &str, src: &str) -> (Output, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-builtin-ufcs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .output()
        .unwrap();
    (out, path)
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn program_impl_clone_for_string_is_refused_as_the_librarys() {
    let src = "impl Clone for String {\n    fn clone(self) -> String { \"x\" }\n}\nfn main() {\n    let s = \"ab\";\n    println(s.clone());\n}\n";
    let (out, _) = karac(&["check"], "clone_string.kara", src);
    let t = text(&out);
    assert!(!out.status.success(), "{t}");
    assert!(
        t.contains(".kara:1:1: conflicting impl: the standard library already implements `Clone` for `String`"),
        "{t}"
    );
}

#[test]
fn program_impl_of_a_library_trait_the_library_lacks_is_allowed() {
    let src = "impl Default for Ordering {\n    fn default() -> Ordering { Ordering.Less }\n}\nfn main() {\n    let o = Ordering.default();\n    println(o == Ordering.Less);\n}\n";
    let (out, _) = karac(&["check"], "default_ordering.kara", src);
    assert!(out.status.success(), "{}", text(&out));
    let (out, _) = karac(&["__mir-run"], "default_ordering_run.kara", src);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "true\n",
        "{}",
        text(&out)
    );
}

#[test]
fn clone_dot_clone_of_a_string_checks_and_runs() {
    let src = "fn main() {\n    let s = \"ab\";\n    let t: String = Clone.clone(s);\n    println(f\"{s} {t}\");\n}\n";
    let (out, _) = karac(&["check"], "ufcs_clone.kara", src);
    assert!(out.status.success(), "{}", text(&out));
    let (out, _) = karac(&["__mir-run"], "ufcs_clone_run.kara", src);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ab ab\n",
        "{}",
        text(&out)
    );
}

#[test]
fn default_dot_default_is_refused_with_the_expected_type_as_its_fix() {
    let src = "#[derive(Default)]\nstruct W { n: i64 }\nfn f(w: W) -> i64 { w.n }\nfn main() {\n    let d: i64 = Default.default();\n    let w: W = Default.default();\n    println(f\"{d} {w.n} {f(Default.default())}\");\n}\n";
    let (out, path) = karac(&["check"], "ufcs_default.kara", src);
    let t = text(&out);
    assert!(!out.status.success(), "{t}");
    assert_eq!(
        t.matches("`Default.default()` names no type").count(),
        3,
        "{t}"
    );
    let fixed = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("fix")
        .arg(&path)
        .output()
        .unwrap();
    assert!(fixed.status.success(), "{}", text(&fixed));
    let now = std::fs::read_to_string(&path).unwrap();
    assert!(now.contains("let d: i64 = i64.default();"), "{now}");
    assert!(now.contains("let w: W = W.default();"), "{now}");
    assert!(now.contains("{f(W.default())}"), "{now}");
    let run = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "0 0 0\n",
        "{}",
        text(&run)
    );
}
