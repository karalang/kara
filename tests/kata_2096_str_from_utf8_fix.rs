//! Kata 2096's Rust arm, LeetCode's template, ends with
//! `ans.push_str(std::str::from_utf8(&d[i..]).unwrap())`. `karac fix`
//! dropped the `std::` and made the `::` a `.`, leaving `str.from_utf8`,
//! and then took the resolver's nearest name for `str` and wrote
//! `ptr.from_utf8`, which cannot be typed. Rust's `str::from_utf8` is
//! Kāra's `String.from_utf8`: the path fix now writes that, and an
//! undefined `str` suggests `String`.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn kata_2096_std_str_from_utf8_fixes_to_string() {
    let src = "fn main() {
    let b: Vec[u8] = [104u8, 105u8, 33u8].to_vec();
    let mut ans = \"U\".repeat(2);
    ans.push_str(std::str::from_utf8(&b[1..]).unwrap());
    let t = str::from_utf8(&b).unwrap();
    println(f\"{ans} {t}\");
}
";
    let file = write("k2096-from-utf8", src);
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        stderr.contains("write `String.from_utf8`") && stderr.contains("`String.from_utf8`"),
        "{stderr}"
    );
    let fix = karac(&["fix", &file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        fixed.contains("ans.push_str(String.from_utf8(b[1..]).unwrap());")
            && fixed.contains("let t = String.from_utf8(b).unwrap();"),
        "{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "UUi! hi!\n");
}

/// `str.from_utf8`, written with the `.` already, suggests `String`, not
/// the nearest name `ptr`.
#[test]
fn kata_2096_bare_str_suggests_string() {
    let src = "fn main() {
    let b: Vec[u8] = [104u8, 105u8].to_vec();
    let s = str.from_utf8(b).unwrap();
    println(f\"{s}\");
}
";
    let file = write("k2096-bare-str", src);
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(stderr.contains("did you mean 'String'?"), "{stderr}");
    let _ = karac(&["fix", &file]);
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        fixed.contains("let s = String.from_utf8(b).unwrap();"),
        "{fixed}"
    );
}
