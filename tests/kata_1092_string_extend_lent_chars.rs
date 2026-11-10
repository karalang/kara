//! `s.extend(v.iter())` on a `String`, with `v` a `Vec[char]`, writes the
//! chars (B-2026-11-10-20). Kata 1092 merged two words with
//! `out.extend(x[i..].iter())`; `String` has `Extend[char]` and
//! `Extend[String]`, and an iterator lending `ref char` matched neither
//! exactly, so `karac check` refused the call as ambiguous and named UFCS.
//! A `Vec`'s `extend` reads a lent `Copy` item out, and a `String`'s now
//! does the same for a lent `char`. Lent `String`s still need `.cloned()`,
//! as they would to extend a `Vec`.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("kata-1092-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

const LENT_CHARS: &str = "fn main() {
    let mut s = String.new();
    let v: Vec[char] = ['a', 'b', 'c'];
    s.extend(v.iter());
    s.extend(v[1..].iter());
    s.extend(\"xy\".chars());
    let w: Vec[String] = [\"P\", \"Q\"];
    s.extend(w);
    s.extend(v.iter().rev());
    println(s);
}
";

#[test]
fn a_string_extends_by_an_iterator_lending_chars() {
    let (dir, path) = fixture("chars", LENT_CHARS);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let runners: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for runner in runners {
        let out = karac().arg(runner).arg(&path).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "abcbcxyPQcba\n",
            "{runner}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_string_extended_by_lent_strings_is_told_to_clone_them() {
    let (dir, path) = fixture(
        "strings",
        "fn main() {\n    let mut s = String.new();\n    let w: Vec[String] = [\"x\", \"y\"];\n    s.extend(w.iter());\n    println(s);\n}\n",
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    let text =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success() && text.contains(".cloned()") && !text.contains("ambiguous"),
        "lent Strings must be refused with the `.cloned()` hint: {text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
