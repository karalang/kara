//! B-2026-10-23-19: a hashed `Map` or `Set` walks its entries in an order
//! that changes from one process to the next and that `KARAC_HASH_SEED`
//! pins (collections.md § Map), and compiled code walks in the order the
//! MIR interpreter walks: iteration, `keys` / `values` / `entries`,
//! Display, `drain_entries`, `clear` and the table's drop. Sorted tables
//! walk in key order.
#![cfg(feature = "llvm")]

mod common;

use std::path::Path;
use std::process::Command;

const SRC: &str = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { print(f"d{self.id} "); } }
fn main() {
    let mut m: Map[String, i64] = Map.new();
    for (i, w) in ["zulu", "alpha", "mike", "bravo", "yankee", "charlie", "xray", "delta", "whiskey", "echo"].iter().enumerate() {
        let _ = m.insert(w.to_string(), i as i64);
    }
    let _ = m.remove(String.from("mike"));
    let mut line = "";
    for (k, v) in m { line = line + f"{k}={v} "; }
    println(line);
    let mut ks = "";
    for k in m.keys() { ks = ks + k + " "; }
    println(ks);
    let mut vs = 0;
    for v in m.values() { vs = vs * 10 + v; }
    println(f"{vs} {m.entries().len()}");
    println(f"{m}");
    let mut s: Set[i64] = Set.new();
    for x in [40, 3, 17, 99, 8, 61, 25] { let _ = s.insert(x); }
    println(f"{s}");
    let mut sm: SortedMap[i64, i64] = SortedMap.new();
    for x in [5, 1, 4, 2, 3] { let _ = sm.insert(x, x * x); }
    println(f"{sm}");
    let mut owned: Map[i64, R] = Map.new();
    for i in 0..8 { let _ = owned.insert(i * 7, R { id: i }); }
    let drained = owned.drain_entries();
    for (k, _) in drained { print(f"k{k} "); }
    println("");
    let mut more: Map[i64, R] = Map.new();
    for i in 0..6 { let _ = more.insert(i * 3, R { id: i + 10 }); }
    more.clear();
    println("");
    let mut last: Map[i64, R] = Map.new();
    for i in 0..6 { let _ = last.insert(i * 5, R { id: i + 20 }); }
}
"#;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn run(cmd: &mut Command, seed: &str) -> String {
    let out = cmd.env("KARAC_HASH_SEED", seed).output().expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8")
}

fn build(src: &Path, exe: &Path) -> bool {
    let out = karac()
        .arg("build")
        .arg(src)
        .arg("-o")
        .arg(exe)
        .output()
        .expect("karac build");
    if out.status.success() {
        return true;
    }
    // Only a link may fail, and only for want of the runtime archive.
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("link"), "karac build: {err}");
    common::link_or_skip(Err(err)).is_some()
}

#[test]
fn mir_hashed_tables_walk_in_the_interpreters_seeded_order() {
    let dir = std::env::temp_dir().join(format!("karac-mir-walk-order-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (src, exe) = (dir.join("order.kara"), dir.join("order"));
    std::fs::write(&src, SRC).unwrap();
    let built = build(&src, &exe);
    let mut seen = Vec::new();
    for seed in ["1", "7", "99", "12345"] {
        let want = run(karac().arg("run").arg("--interp").arg(&src), seed);
        // The sorted table prints in key order whatever the seed.
        assert!(want.contains("{1: 1, 2: 4, 3: 9, 4: 16, 5: 25}"), "{want}");
        if built {
            assert_eq!(run(&mut Command::new(&exe), seed), want, "seed {seed}");
        }
        seen.push(want);
    }
    seen.dedup();
    assert!(seen.len() > 1, "four seeds walked one order:\n{}", seen[0]);
    let _ = std::fs::remove_dir_all(&dir);
}
