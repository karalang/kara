//! `Option[String].as_deref()` views the string as an `Option[Str]`
//! without moving or allocating (library/core-types.md), so an owned
//! optional string meets a `Some` of a `Str` constant in `==` and in a
//! `match` on string patterns (B-2026-11-09-97's ruling).

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
    let dir = std::env::temp_dir().join(format!("karac-option-as-deref-{}", std::process::id()));
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
        &["__mir-run", "__mir-llvm-run"]
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
fn as_deref_views_an_optional_string_as_an_optional_str() {
    runs(
        "as_deref.kara",
        "const TOKEN: Str = \"purge\";
fn lookup(k: i64) -> Option[String] {
    if k > 0 { Some(f\"key{k}\") } else { None }
}
fn main() {
    let h: Option[String] = Some(\"purge\".to_string());
    let n: Option[String] = None;
    println(f\"{h.as_deref() == Some(TOKEN)} {n.as_deref() == Some(TOKEN)} {n.as_deref().is_none()}\");
    let g = lookup(2);
    let label = match g.as_deref() {
        Some(\"key2\") => \"two\",
        Some(_) => \"other\",
        None => \"none\",
    };
    println(f\"{label} {lookup(0).as_deref().unwrap_or(\"-\")} {g.as_deref().map(|s| s.len()).unwrap_or(0)}\");
    println(f\"{g.unwrap()}\");
}
",
        "true false true\ntwo - 4\nkey2\n",
    );
}
