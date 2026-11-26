//! B-2026-11-25-63 (library/strings.md, review-thread ruling 2026-11-25):
//! `s.slice(a, b)` on a `String` or a `Str` is a deprecated second spelling
//! of the view `s[a..b]`. `karac check` warns, `karac fix` rewrites it to
//! `s[a..b]`, and the program prints what it did. A user type's own `slice`
//! method is untouched.

use std::process::Command;

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-str-slice-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::write(dir.join("m.kara"), src).expect("write source");
    dir
}

fn karac(dir: &std::path::Path, args: &[&str]) -> (String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .arg("m.kara")
        .output()
        .expect("karac");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn string_slice_warns_and_is_rewritten_to_a_range_index() {
    let src = "fn main() {\n    let s = \"hello world\".to_string();\n    let i: i64 = 1;\n    println(s.slice(i, i + 3));\n    let t: Str = s[6..11];\n    println(t.slice(0, 2));\n}\n";
    let dir = scratch("fix", src);
    let (_, err) = karac(&dir, &["check"]);
    assert_eq!(
        err.matches("use of deprecated `slice(a, b)` on a string")
            .count(),
        2,
        "{err}"
    );
    let (before, _) = karac(&dir, &["run", "--interp"]);
    assert_eq!(before, "ell\nwo\n");
    karac(&dir, &["fix"]);
    let fixed = std::fs::read_to_string(dir.join("m.kara")).unwrap();
    assert!(fixed.contains("println(s[i..i + 3]);"), "{fixed}");
    assert!(fixed.contains("println(t[0..2]);"), "{fixed}");
    let (_, err) = karac(&dir, &["check"]);
    assert!(!err.contains("deprecated"), "{err}");
    let (after, _) = karac(&dir, &["run", "--interp"]);
    assert_eq!(after, before);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_user_types_own_slice_is_not_deprecated() {
    let src = "struct Buf { s: String }\nimpl Buf {\n    fn slice(self, a: i64, b: i64) -> String { self.s.substring(a, b) }\n}\nfn main() {\n    let b = Buf { s: \"abcdef\".to_string() };\n    println(b.slice(1, 3));\n}\n";
    let dir = scratch("user", src);
    let (_, err) = karac(&dir, &["check"]);
    assert!(!err.contains("deprecated"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
