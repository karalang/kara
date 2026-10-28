//! `entry(k).or_insert(v)` and `or_insert_with` on a new key insert the
//! entry with the hash the probe already computed and keep the place the
//! insert hands back, rather than hashing the key again and searching
//! for it (B-2026-10-02-39). Words, `String` keys and a growing value,
//! and a sorted map (searched again, as it has no hash), must each write
//! through the returned reference. The expected output is a Python
//! mirror's.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"fn main() {
    let mut m: Map[i64, i64] = Map.new();
    let mut i = 0;
    while i < 5000 {
        *m.entry((i * 7) % 1031).or_insert(0) += i;
        i += 1;
    }
    let mut t = 0;
    for (k, v) in m.iter() { t = t + k * 3 + v; }
    println(f"{m.len()} {t} {m.get(7)}");
    let mut w: Map[String, i64] = Map.new();
    for s in ["a", "bb", "a", "ccc", "bb", "a", "dd"] {
        *w.entry(s.to_string()).or_insert(10) += 1;
    }
    println(f"{w.get("a".to_string())} {w.get("bb".to_string())} {w.get("dd".to_string())} {w.len()}");
    let mut sm: SortedMap[i64, i64] = SortedMap.new();
    for x in [5, 1, 9, 5, 3, 1, 5] {
        *sm.entry(x).or_insert(100) += x;
    }
    let mut parts: Vec[String] = Vec.new();
    for (k, v) in sm.iter() { parts.push(f"{k}:{v}"); }
    let mut line = String.new();
    for p in parts { line.push_str(p.as_str()); line.push_str(" "); }
    println(line);
    let mut lazy: Map[i64, Vec[i64]] = Map.new();
    let mut j = 0;
    while j < 40 {
        lazy.entry(j % 6).or_insert_with(|| Vec.new()).push(j);
        j += 1;
    }
    let mut lt = 0;
    for (k, v) in lazy.iter() { lt = lt + k * 1000 + v.len() * 10 + v[v.len() - 1]; }
    println(f"{lazy.len()} {lt}");
}
"#;

const WANT: &str =
    "1031 14090395 Some(10315)\nSome(13) Some(12) Some(11) 4\n1:102 3:103 5:115 9:109 \n6 15619\n";

fn compiled(src: &str) -> Option<String> {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir =
        std::env::temp_dir().join(format!("karac-mir-map-entry-insert-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).unwrap())
}

#[test]
fn mir_map_entry_inserts_with_the_probes_hash() {
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, WANT);
    if let Some(out) = compiled(SRC) {
        assert_eq!(out, WANT);
    }
}
