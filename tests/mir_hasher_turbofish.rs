//! A table built by a constructor qualified with a hasher,
//! `Map[K, V, FxBuildHasher].new()` or `Set[T, FxBuildHasher].new()`,
//! type checks and walks in a fixed order across hash seeds, as an
//! annotated `let`'s does (B-2026-10-25-31). The plain `Map[K, V].new()`
//! is the control: its order must move, or the run proves nothing. A
//! qualifier that is not a hasher is refused as it is in a type.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"fn main() {
    let mut fx = Map[i64, i64, FxBuildHasher].new();
    let mut set = Set[i64, FxBuildHasher].new();
    let mut sip = Map[i64, i64].new();
    for i in 0..12 {
        fx.insert(i, i);
        set.insert(i * 3);
        sip.insert(i, i);
    }
    let mut a = "map:";
    for (k, _) in fx {
        a = f"{a} {k}";
    }
    let mut b = "set:";
    for k in set {
        b = f"{b} {k}";
    }
    let mut c = "sip:";
    for (k, _) in sip {
        c = f"{c} {k}";
    }
    println(a);
    println(b);
    println(c);
}
"#;

fn scratch(name: &str, src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-hasher-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    (dir, file)
}

#[test]
fn mir_hasher_qualified_constructors_walk_in_a_fixed_order() {
    let (dir, file) = scratch("turbofish", SRC);
    for cmd in ["__mir-run", "__mir-llvm-run"] {
        let mut samples: Vec<Vec<String>> = Vec::new();
        for seed in 1..=6 {
            let out = Command::new(env!("CARGO_BIN_EXE_karac"))
                .arg(cmd)
                .arg(&file)
                .env("KARAC_HASH_SEED", seed.to_string())
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
        for (i, (tag, step)) in [("map:", 1), ("set:", 3)].into_iter().enumerate() {
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
                "{cmd}: the {tag} FxBuildHasher table's order moved between seeds: {samples:?}"
            );
        }
        assert!(
            samples.iter().any(|s| s[2] != samples[0][2]),
            "{cmd}: the default table's order did not move across 6 seeds: {samples:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mir_hasher_qualified_constructor_refuses_a_non_hasher() {
    let (dir, file) = scratch(
        "turbofish-bad",
        "fn main() {\n    let m = Map[i64, i64, i64].new();\n    println(f\"{m.len()}\");\n}\n",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&file)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a non-hasher qualifier ran: {stderr}"
    );
    assert!(
        stderr.contains("'i64' cannot be one: it does not implement 'BuildHasher'"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
