//! A table stored in a struct field whose type names a hasher walks in a
//! fixed order across processes, as an annotated `let`'s does, whether the
//! struct literal builds it or an assignment to the field replaces it
//! (B-2026-10-25-31). The default table in the same struct is the control:
//! its order must still move, or the run proves nothing.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"struct Index {
    fx: Map[i64, i64, FxBuildHasher],
    sip: Map[i64, i64],
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
    let mut built = Index { fx: Map.new(), sip: Map.new() };
    let mut assigned = Index { fx: Map.new(), sip: Map.new() };
    assigned.fx = Map.new();
    for i in 0..12 {
        built.fx.insert(i, i);
        built.sip.insert(i, i);
        assigned.fx.insert(i * 7, i);
    }
    println(f"built:{keys(built.fx)}");
    println(f"assigned:{keys(assigned.fx)}");
    println(f"sip:{sip_keys(built.sip)}");
}
"#;

#[test]
fn mir_hasher_field_tables_walk_in_a_fixed_order() {
    let dir = std::env::temp_dir().join(format!("karac-hasher-fields-{}", std::process::id()));
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
            assert_eq!(lines.len(), 3, "{cmd}: {lines:?}");
            samples.push(lines);
        }
        if samples.is_empty() {
            continue;
        }
        for (i, tag) in ["built:", "assigned:"].iter().enumerate() {
            let mut keys: Vec<i64> = samples[0][i]
                .trim_start_matches(tag)
                .split_whitespace()
                .map(|k| k.parse().unwrap())
                .collect();
            keys.sort_unstable();
            let step = if i == 0 { 1 } else { 7 };
            assert_eq!(
                keys,
                (0..12).map(|k| k * step).collect::<Vec<_>>(),
                "{cmd}: {tag}"
            );
            assert!(
                samples.iter().all(|s| s[i] == samples[0][i]),
                "{cmd}: the {tag} FxBuildHasher field's order moved between processes: {samples:?}"
            );
        }
        assert!(
            samples.iter().any(|s| s[2] != samples[0][2]),
            "{cmd}: the default field's order did not move across 6 processes: {samples:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
