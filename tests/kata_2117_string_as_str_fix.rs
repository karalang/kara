//! Kata 2117's limbs arm strips the zeros from its digits with
//! `strip_suffix` in a loop, which needs a `Str` it can reassign:
//! `let mut kept: Str = digits;` with `digits` a `String`. That was
//! "expected 'Str', found 'String'" and nothing more, while a `String`
//! passes as a `Str` at a call and strings.md names `as_str()` for every
//! other place. The mismatch now says so and carries the edit.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn kata_2117_string_bound_as_str_gets_as_str() {
    let path = std::env::temp_dir().join(format!("karac-k2117-as-str-{}.kara", std::process::id()));
    std::fs::write(
        &path,
        "fn main() {
    let digits: String = f\"{12 * 100}\";
    let mut kept: Str = digits;
    while let Some(rest) = kept.strip_suffix(\"0\") {
        kept = rest;
    }
    println(f\"{kept} {digits.len() - kept.len()}\");
}
",
    )
    .unwrap();
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        stderr.contains("take the view with `.as_str()`"),
        "{stderr}"
    );
    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        fixed.contains("let mut kept: Str = digits.as_str();"),
        "{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "12 2\n");
}

#[test]
fn kata_2117_string_in_a_sum_gets_no_as_str_edit() {
    let path = std::env::temp_dir().join(format!(
        "karac-k2117-as-str-sum-{}.kara",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "fn main() {
    let a: String = \"x\";
    let kept: Str = a + \"y\";
    println(kept);
}
",
    )
    .unwrap();
    let file = path.to_str().unwrap();
    let check = karac(&["check", "--output=json", file]);
    let _ = std::fs::remove_file(&path);
    let out = String::from_utf8_lossy(&check.stdout).to_string();
    assert!(out.contains("found 'String'"), "{out}");
    assert!(!out.contains(".as_str()\""), "{out}");
}
