//! B-2026-11-20-68: `karac fix` applies exactly the machine-applicable
//! diagnostics `karac check --output=json` reports. The style lints are off
//! in both unless the same `-W NAME` turns one on in both; before the fix,
//! `fix` turned all of them on by itself, so it rewrote code `check` had
//! reported nothing about.

use std::path::PathBuf;
use std::process::{Command, Output};

fn karac(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn scratch(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-fixagree-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

const SRC: &str = "fn double(n: i64) -> i64 {\n    let mut m = n;\n    m = m * 2;\n    return m;\n}\n\nfn main() {\n    println(double(21));\n}\n";

/// The number of diagnostics carrying a `fix_diff` in `check --output=json`.
fn fixable(extra: &[&str], p: &str) -> usize {
    let mut args = vec!["check", "--output=json"];
    args.extend_from_slice(extra);
    args.push(p);
    text(&karac(&args).stdout).matches("\"fix_diff\":[").count()
}

#[test]
fn fix_check_agree_style_lints_off_by_default() {
    let f = scratch("off", SRC);
    let p = f.to_str().unwrap();
    assert_eq!(fixable(&[], p), 0);
    let out = karac(&["fix", p]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(std::fs::read_to_string(&f).unwrap(), SRC);
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
}

#[test]
fn fix_check_agree_one_w_flag_turns_on_one_lint_in_both() {
    let f = scratch("one", SRC);
    let p = f.to_str().unwrap();
    assert_eq!(fixable(&["-W", "needless_return"], p), 1);
    let out = karac(&["fix", "-W", "needless_return", p]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let after = std::fs::read_to_string(&f).unwrap();
    // Only the lint asked for: the `m = m * 2` stays.
    assert!(after.contains("    m = m * 2;\n    m\n}"), "{after}");
    assert_eq!(fixable(&["-W", "needless_return"], p), 0);
    assert_eq!(fixable(&["-W", "manual_compound_assign"], p), 1);
    let out = karac(&["fix", "-W", "manual_compound_assign", p]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let after = std::fs::read_to_string(&f).unwrap();
    assert!(after.contains("    m *= 2;\n    m\n}"), "{after}");
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
}

#[test]
fn fix_check_agree_unknown_flag_is_refused() {
    let f = scratch("bad", SRC);
    let out = karac(&["fix", "--frobnicate", f.to_str().unwrap()]);
    assert!(!out.status.success());
    assert_eq!(std::fs::read_to_string(&f).unwrap(), SRC);
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
}
