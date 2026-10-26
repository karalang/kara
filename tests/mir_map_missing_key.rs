//! Indexing a `Map` with a missing key panics (library/collections.md),
//! and says why: `key not found`, on the interpreter and built alike, where
//! it used to read `explicit panic`, as if the program had called `panic`
//! (B-2026-10-25-1).
#![cfg(feature = "llvm")]

use std::process::Command;

fn stops_with(backend: &str, name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "karac-mir-map-missing-key-{backend}-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(backend)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if stderr.contains("runtime archive") && stderr.contains("not found") {
        panic!("{backend}: no runtime archive\n{stderr}");
    }
    assert_eq!(out.status.code(), Some(101), "{backend} {name}: {stderr}");
    stderr
}

#[test]
fn mir_map_index_of_a_missing_key_says_key_not_found() {
    for (name, src) in [
        (
            "read",
            "fn main() {\n    let mut m: Map[i64, i64] = Map.new();\n    m.insert(1, 5);\n    let x = m[2];\n    println(f\"{x}\");\n}\n",
        ),
        (
            "compound",
            "fn main() {\n    let mut m: Map[String, i64] = Map.new();\n    m.insert(\"a\", 1);\n    m[\"b\"] -= 1;\n    println(m.len());\n}\n",
        ),
        (
            "sorted",
            "fn main() {\n    let mut m: SortedMap[i64, i64] = SortedMap.new();\n    m.insert(1, 5);\n    println(m[3]);\n}\n",
        ),
    ] {
        for backend in ["__mir-run", "__mir-llvm-run"] {
            let stderr = stops_with(backend, name, src);
            assert!(
                stderr.contains("key not found") && !stderr.contains("explicit panic"),
                "{backend} {name}: {stderr}"
            );
        }
    }
}
