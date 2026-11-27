//! `a + b.as_str()`, a `String` plus a `Str` as Rust writes it: the fix
//! drops the `.as_str()` that made the view, lending a place (`a + ref b`,
//! as `karac fix` makes of Rust's `a + &b`) and adding an owned temporary as
//! it is (`a + b.clone()`), where it appended `.to_string()` and copied the
//! view back into a new `String` (B-2026-11-26-82).

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

fn file(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-str-concat-as-str-{}-{}",
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

fn runs_everywhere(src: &str, want: &str) {
    let (ok, out) = check(src);
    assert!(ok, "check:\n{out}\n{src}");
    let mut lanes = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        lanes.extend(["__mir-llvm-run", "__mir-llvm-asan"]);
    }
    for cmd in lanes {
        let (dir, f) = file(src);
        let (ok, out, err) = karac(&[cmd], &f);
        let _ = std::fs::remove_dir_all(&dir);
        if cmd != "__mir-run"
            && common::link_or_skip(if ok { Ok(()) } else { Err(err.clone()) }).is_none()
        {
            continue;
        }
        assert!(ok, "{cmd}: {err}\n{src}");
        assert_eq!(out, want, "{cmd}\n{src}");
    }
}

const SRC: &str = "struct W { s: String }

fn arm(x: i64) -> String {
    f\"<{x}>\"
}

fn main() {
    let a = \"x\".to_string();
    let b = \"y\".to_string();
    let w = W { s: \"z\".to_string() };
    let c = a.clone() + b.as_str();
    let e = a.clone() + b.clone().as_str();
    let g = arm(1) + arm(2).as_str();
    let h = a.clone() + w.s.as_str();
    println(c);
    println(e);
    println(g);
    println(h);
    println(b);
}
";

#[test]
fn string_plus_as_str_drops_the_as_str() {
    let (ok, out) = check(SRC);
    assert!(!ok && out.contains("E0208"), "{out}");
    let after = fixed(SRC);
    for want in [
        "let c = a.clone() + ref b;",
        "let e = a.clone() + b.clone();",
        "let g = arm(1) + arm(2);",
        "let h = a.clone() + ref w.s;",
    ] {
        assert!(after.contains(want), "{want}\n{after}");
    }
    runs_everywhere(&after, "xy\nxy\n<1><2>\nxz\ny\n");
    // A `Str` operand of its own still takes `.to_string()`.
    let after = fixed("fn main() { let a = \"x\".to_string(); let s = \"abc\".to_string(); let t = a + s[1..]; println(t); }\n");
    assert!(after.contains("let t = a + s[1..].to_string();"), "{after}");
}
