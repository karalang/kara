//! `None`, `Some(x)`, `Ok(x)` and `Err(e)` given to a table's `insert`,
//! `contains`, `remove`, `get` or `contains_key` take the table's element
//! or key type, as `push` gives them the `Vec`'s (B-2026-10-27-35).
//! Inferred alone they were `Option[()]`, a one-byte value the compiled
//! table read 32 bytes of (an ASan stack-buffer-overflow). Both backends
//! must print what the spec gives, and the ASan lane must run clean.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"fn main() {
    let mut o: Set[Option[String]] = Set.new();
    o.insert(None);
    o.insert(Some("a"));
    println(f"{o.contains(None)} {o.contains(Some("a"))} {o.contains(Some("b"))}");
    o.remove(None);
    let mut k: Map[Option[String], i64] = Map.new();
    k.insert(None, 1);
    k.insert(Some("x"), 2);
    println(f"{k.get(None).unwrap()} {k.contains_key(Some("x"))} {k.len()} {o.len()}");
    let mut r: Set[Result[i64, String]] = Set.new();
    r.insert(Ok(3));
    r.insert(Err("e"));
    println(f"{r.contains(Ok(3))} {r.contains(Err("e"))} {r.contains(Err("f"))}");
    let mut ss: SortedSet[Option[i64]] = SortedSet.new();
    ss.insert(None);
    ss.insert(Some(2));
    println(f"{ss.contains(None)} {ss.len()}");
}
"#;

const WANT: &str = "true true false\n1 true 2 1\ntrue true false\ntrue 2\n";

fn run_both(name: &str, src: &str, want: &str) {
    let dir = std::env::temp_dir().join(format!("karac-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let mut ran = 0;
    for cmd in ["__mir-run", "__mir-llvm-run"] {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg(cmd)
            .arg(&file)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        if cmd == "__mir-llvm-run" && stderr.contains("undefined reference") {
            if common::link_or_skip(Err(stderr)).is_none() {
                continue;
            }
            unreachable!("link_or_skip passes only a successful link");
        }
        assert!(out.status.success(), "{cmd}: {stderr}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{cmd}");
        ran += 1;
    }
    assert!(ran >= 1, "neither backend ran");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_open_value_needle_takes_the_tables_element_type() {
    run_both("open-needle", SRC, WANT);
}

#[test]
fn an_open_value_needle_runs_clean_under_asan() {
    if std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_some() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("karac-open-needle-asan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-llvm-asan")
        .arg(&file)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);
    if out.status.code() == Some(3) && err.contains("Linker failed") {
        let _ = common::link_or_skip(Err(err));
        return;
    }
    assert_eq!(out.status.code(), Some(0), "stderr:\n{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), WANT);
}
