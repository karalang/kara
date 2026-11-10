//! `Result.unwrap_or_default()` gives the value, or `T`'s default on `Err`,
//! as `Option.unwrap_or_default` does on `None` (B-2026-11-10-46). The
//! error is dropped, which the ASan lane checks for a heap error.

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn write(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-result-unwrap-or-default-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

/// Checks clean and prints `stdout` on every MIR lane this build has.
fn runs(name: &str, src: &str, stdout: &str) {
    let path = write(name, src);
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run", "__mir-llvm-asan"]
    } else {
        &["__mir-run"]
    };
    for b in backends {
        let run = karac(&[b], &path);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            stdout,
            "{b}: {}",
            text(&run)
        );
    }
}

#[test]
fn result_unwrap_or_default_gives_the_default_on_err() {
    runs(
        "ok.kara",
        "fn parse(s: Str) -> Result[i64, String] {
    if s == \"42\" { Ok(42) } else { Err(f\"not a number: {s}\") }
}
fn main() {
    let a: Result[String, String] = Ok(\"kept\".to_string());
    let b: Result[String, String] = Err(\"boom\".to_string());
    println(f\"[{a.unwrap_or_default()}] [{b.unwrap_or_default()}]\");
    println(f\"{parse(\"42\").unwrap_or_default()} {parse(\"x\").unwrap_or_default()}\");
    let v: Result[Vec[i64], String] = Err(\"no\".to_string());
    println(f\"{v.unwrap_or_default().len()}\");
    let bytes: Vec[u8] = [104u8, 105u8].to_vec();
    let bad: Vec[u8] = [255u8].to_vec();
    println(f\"[{String.from_utf8(bytes).unwrap_or_default()}] [{String.from_utf8(bad).unwrap_or_default()}]\");
}
",
        "[kept] []\n42 0\n0\n[hi] []\n",
    );
}

#[test]
fn result_unwrap_or_default_needs_a_default_item() {
    let path = write(
        "nodef.kara",
        "struct P { x: i64 }
fn main() {
    let r: Result[P, String] = Ok(P { x: 1 });
    println(f\"{r.unwrap_or_default().x}\");
}
",
    );
    let check = karac(&["check"], &path);
    assert!(!check.status.success());
    let out = text(&check);
    assert!(
        out.contains("trait bound `T: Default` is not satisfied; `P` does not implement `Default`"),
        "{out}"
    );
}
