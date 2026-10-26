//! A table whose type names a hasher walks in a fixed order across
//! processes wherever the type is written, not only on an annotated `let`
//! or a plain struct's field: a function's return type (by its tail or a
//! `return`), an owned parameter, and a `shared` struct's field
//! (B-2026-10-25-31). The default table is the control: its order must
//! still move, or the run proves nothing.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"shared struct Registry {
    fx: Map[i64, i64, FxBuildHasher],
}

fn made(n: i64) -> Map[i64, i64, FxBuildHasher] {
    let mut m = Map.new();
    for i in 0..n {
        m.insert(i, i);
    }
    m
}

fn early(n: i64) -> Map[i64, i64, FxBuildHasher] {
    let mut m = Map.new();
    for i in 0..n {
        m.insert(i * 3, i);
    }
    if n > 0 {
        return m;
    }
    Map.new()
}

fn filled(m: own Map[i64, i64, FxBuildHasher]) -> Map[i64, i64, FxBuildHasher] {
    let mut m = m;
    for i in 0..12 {
        m.insert(i * 5, i);
    }
    m
}

fn keys(m: ref Map[i64, i64, FxBuildHasher]) -> String {
    let mut out = "";
    for (k, _) in m {
        out = out + " " + k.to_string();
    }
    out
}

fn sip_keys(m: ref Map[i64, i64]) -> String {
    let mut out = "";
    for (k, _) in m {
        out = out + " " + k.to_string();
    }
    out
}

fn main() {
    let r = Registry { fx: Map.new() };
    for i in 0..12 {
        r.fx.insert(i * 11, i);
    }
    let mut sip: Map[i64, i64] = Map.new();
    for i in 0..12 {
        sip.insert(i, i);
    }
    println(f"returned:{keys(made(12))}");
    println(f"early:{keys(early(12))}");
    println(f"param:{keys(filled(Map.new()))}");
    println(f"shared:{keys(r.fx)}");
    println(f"sip:{sip_keys(sip)}");
}
"#;

#[test]
fn mir_hasher_returned_param_and_shared_field_tables_walk_in_a_fixed_order() {
    let dir = std::env::temp_dir().join(format!("karac-hasher-returns-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    for cmd in ["__mir-run", "__mir-llvm-run"] {
        let mut samples: Vec<Vec<String>> = Vec::new();
        for _ in 0..6 {
            let out = Command::new(env!("CARGO_BIN_EXE_karac"))
                .arg(cmd)
                .arg(&file)
                .env_remove("KARAC_HASH_SEED")
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            if cmd == "__mir-llvm-run" && stderr.contains("undefined reference") {
                if common::link_or_skip(Err(stderr)).is_none() {
                    break;
                }
                unreachable!("link_or_skip passes only a successful link");
            }
            assert!(out.status.success(), "{cmd}: {stderr}");
            let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::to_string)
                .collect();
            assert_eq!(lines.len(), 5, "{cmd}: {lines:?}");
            samples.push(lines);
        }
        if samples.is_empty() {
            continue;
        }
        let tags = [
            ("returned:", 1),
            ("early:", 3),
            ("param:", 5),
            ("shared:", 11),
        ];
        for (i, (tag, step)) in tags.iter().enumerate() {
            let mut keys: Vec<i64> = samples[0][i]
                .trim_start_matches(tag)
                .split_whitespace()
                .map(|k| k.parse().unwrap())
                .collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                (0..12).map(|k| k * step).collect::<Vec<_>>(),
                "{cmd}: {tag}"
            );
            assert!(
                samples.iter().all(|s| s[i] == samples[0][i]),
                "{cmd}: the {tag} FxBuildHasher table's order moved between processes: {samples:?}"
            );
        }
        assert!(
            samples.iter().any(|s| s[4] != samples[0][4]),
            "{cmd}: the default table's order did not move across 6 processes: {samples:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
