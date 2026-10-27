//! A table's hasher is part of its type (collections.md § Hashing, ruled
//! for B-2026-10-25-31): a `Map[K, V, FxBuildHasher]` / `Set[T,
//! FxBuildHasher]` walks in a fixed order across hash seeds however it was
//! built: by a literal, by `Map.new()` pushed into a `Vec` of them or stored
//! in a struct field, or returned from a function whose own `Map.new()`
//! names no hasher. A function generic over the hasher takes tables of
//! either. The default table is the control: its order must move, or the
//! run proves nothing. A table of one hasher is not a table of another.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"struct Index {
    by_id: Map[i64, i64, FxBuildHasher],
}

fn build() -> Map[i64, i64, FxBuildHasher] {
    let mut m = Map.new();
    for i in 0..12 {
        m.insert(i * 5, i);
    }
    m
}

fn keys[H](m: ref Map[i64, i64, H]) -> String {
    let mut out = "";
    for (k, _) in m {
        out = f"{out} {k}";
    }
    out
}

fn set_keys[H](s: ref Set[i64, H]) -> String {
    let mut out = "";
    for k in s {
        out = f"{out} {k}";
    }
    out
}

fn main() {
    let lit: Map[i64, i64, FxBuildHasher] = [1: 1, 2: 2, 3: 3, 4: 4, 5: 5, 6: 6, 7: 7, 8: 8, 9: 9];
    let set: Set[i64, FxBuildHasher] = Set[1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let mut ix = Index { by_id: Map.new() };
    let mut v: Vec[Map[i64, i64, FxBuildHasher]] = Vec.new();
    v.push(Map.new());
    let mut plain = Map.new();
    for i in 0..12 {
        ix.by_id.insert(i * 3, i);
        v[0].insert(i * 7, i);
        plain.insert(i, i);
    }
    println(f"lit:{keys(lit)}");
    println(f"set:{set_keys(set)}");
    println(f"field:{keys(ix.by_id)}");
    println(f"ret:{keys(build())}");
    println(f"vec:{keys(v[0])}");
    println(f"plain:{keys(plain)}");
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
fn mir_hasher_in_type_tables_walk_in_a_fixed_order() {
    let (dir, file) = scratch("in-type", SRC);
    let fixed = [
        ("lit:", (1..=9).collect::<Vec<i64>>()),
        ("set:", (1..=10).collect()),
        ("field:", (0..12).map(|i| i * 3).collect()),
        ("ret:", (0..12).map(|i| i * 5).collect()),
        ("vec:", (0..12).map(|i| i * 7).collect()),
    ];
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
            assert_eq!(lines.len(), 6, "{cmd}: {lines:?}");
            samples.push(lines);
        }
        if samples.is_empty() {
            continue;
        }
        for (i, (tag, want)) in fixed.iter().enumerate() {
            assert!(samples[0][i].starts_with(tag), "{cmd}: {:?}", samples[0]);
            let mut keys: Vec<i64> = samples[0][i]
                .trim_start_matches(tag)
                .split_whitespace()
                .map(|k| k.parse().unwrap())
                .collect();
            keys.sort_unstable();
            assert_eq!(&keys, want, "{cmd}: {tag}");
            assert!(
                samples.iter().all(|s| s[i] == samples[0][i]),
                "{cmd}: the {tag} FxBuildHasher table's order moved between seeds: {samples:?}"
            );
        }
        assert!(
            samples.iter().any(|s| s[5] != samples[0][5]),
            "{cmd}: the default table's order did not move across 6 seeds: {samples:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mir_hasher_in_type_tables_of_two_hashers_differ() {
    let (dir, file) = scratch(
        "in-type-differ",
        "fn total(m: ref Map[i64, i64]) -> i64 {\n    m.len()\n}\n\nfn main() {\n    \
         let m: Map[i64, i64, FxBuildHasher] = Map.new();\n    println(f\"{total(m)}\");\n}\n",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&file)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "an Fx table passed as a default one: {stderr}"
    );
    assert!(
        stderr.contains("expected 'ref Map[i64, i64]', found 'Map[i64, i64, FxBuildHasher]'"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
