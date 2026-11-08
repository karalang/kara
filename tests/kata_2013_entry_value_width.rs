//! Kata 2013's Rust arm: `self.cnt.entry(p).or_insert(0)` on a
//! `Map[(i32, i32), i32]`. The `0` reached MIR as an `i64` (the typed HIR
//! keeps a literal's default, and the lowering builds a literal argument at
//! the parameter's type only where it knows one), so the compiled
//! `entry_or_insert` stored 8 bytes into the 4-byte value of a 12-byte entry:
//! AddressSanitizer's stack-buffer-overflow. The value is
//! now built at the map's value type, fused (`m.entry(k).or_insert(v)`) or
//! not (`e.or_insert(v)`).

use std::path::Path;
use std::process::{Command, Stdio};

fn built_mir(dir: &Path, src: &str) -> (String, String) {
    std::fs::create_dir_all(dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", "cell.kara"])
        .env("KARAC_MIR_DUMP", "built")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("karac-kata-2013-{name}-{}", std::process::id()))
}

#[test]
fn kata_2013_fused_or_insert_builds_its_literal_at_the_value_type() {
    let src = "fn main() {\n    let mut cnt: Map[(i32, i32), i32] = Map.new();\n    let p = (3i32, 10i32);\n    let c = cnt.entry(p).or_insert(0);\n    *c += 1;\n    println(cnt[(3, 10)]);\n}";
    let (out, mir) = built_mir(&scratch("fused"), src);
    assert_eq!(out, "1\n");
    assert!(mir.contains("entry_or_insert("), "{mir}");
    assert!(mir.contains("const 0_i32)"), "{mir}");
    assert!(!mir.contains("const 0_i64)"), "{mir}");
}

#[test]
fn kata_2013_entry_or_insert_builds_its_literal_at_the_value_type() {
    let src = "fn main() {\n    let mut cnt: Map[i32, u8] = Map.new();\n    let e = cnt.entry(3i32);\n    let c = e.or_insert(7);\n    *c += 1;\n    println(cnt[3]);\n}";
    let (out, mir) = built_mir(&scratch("unfused"), src);
    assert_eq!(out, "8\n");
    assert!(mir.contains(".or_insert(move _"), "{mir}");
    assert!(mir.contains("const 7_u8)"), "{mir}");
}
