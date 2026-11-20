//! B-2026-11-21-3: the library's table methods written in Kāra source
//! (`from`, `extend`, `retain`, `get_mut`, `collect`'s `from_iter`) ran
//! over `Map[K, V]` / `Set[T]` with the default hasher whatever the table
//! named, so a `Map[K, V, H]` with a user `BuildHasher` filled by `extend`
//! or `collect` hashed its keys one way and looked them up another: `get`
//! missed keys the table held, and `karac run` and `karac build` disagreed.
//! Each is now written over any hasher, and a table that names none binds
//! the hasher to the default. The digests the user hasher prints are a
//! Python mirror's (start 7, `n = n * 31 + byte` over each key's eight
//! little-endian bytes); both backends must print them.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const HASHER: &str = r#"import std.hash.{Hasher, BuildHasher};
struct Rec { n: u64 }
impl Hasher for Rec {
    fn write(mut ref self, bytes: ref Slice[u8]) {
        for b in bytes.iter() { self.n = self.n.wrapping_mul(31).wrapping_add(*b as u64); }
    }
    fn finish(ref self) -> u64 { self.n }
}
struct RecBuild {}
impl BuildHasher for RecBuild {
    type Hasher = Rec;
    fn build(ref self) -> Rec { Rec { n: 7 } }
}
"#;

fn run_both(name: &str, body: &str, want: &str) {
    let dir = std::env::temp_dir().join(format!("karac-hashlib-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, format!("{HASHER}{body}")).unwrap();
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
fn user_hasher_table_keeps_keys_through_library_methods() {
    run_both(
        "methods",
        r#"fn main() {
    let mut m: Map[i64, i64, RecBuild] = Map.new();
    m.extend([(1, 10), (2, 20)].to_vec());
    m.retain(|k, v| k != 2);
    match m.get_mut(1) { Some(r) => { *r += 1; }, None => {} }
    let f: Map[i64, i64, RecBuild] = Map.from([(4, 40)].to_vec());
    let mut s: Set[i64, RecBuild] = Set.from([1, 2, 3].to_vec());
    s.retain(|x| x != 1);
    s.extend([9].to_vec());
    println(f"{m.get(1) ?? -1} {m.contains_key(2)} {m.len()} {f.get(4) ?? -1} {s.contains(1)} {s.contains(2)} {s.contains(9)} {s.len()}");
}
"#,
        "11 false 1 40 false true true 3\n",
    );
}

#[test]
fn collect_into_user_hasher_table_runs_its_hasher() {
    run_both(
        "collect",
        r#"struct Tap {}
impl BuildHasher for Tap {
    type Hasher = Rec;
    fn build(ref self) -> Rec { println("build"); Rec { n: 7 } }
}
fn main() {
    let m: Map[i64, i64, RecBuild] = (0..2).map(|i| (i, i * 10)).collect();
    let s: Set[i64, RecBuild] = (0..3).collect();
    let t: Set[i64, Tap] = [5].to_vec().into_iter().collect();
    // The same collect into a default table is its own instance: the two
    // shared one compiled body, and `c[2]` panicked "key not found".
    let c: Map[i64, i64] = (0..3).map(|i| (i, i)).collect();
    println(f"{m.get(1) ?? -1} {m.get(0) ?? -1} {m.len()} {s.contains(2)} {s.len()} {t.contains(5)} {c[2]}");
}
"#,
        "build\nbuild\n10 0 2 true 3 true 2\n",
    );
}

#[test]
fn default_and_fx_tables_reach_the_same_library_methods() {
    run_both(
        "default",
        r#"fn main() {
    let d: Map[i64, i64] = Map.from([(1, 2)].to_vec());
    let q = Map.from([(3, 4)].to_vec());
    let x: Map[i64, i64, FxBuildHasher] = Map.from([(5, 6)].to_vec());
    let mut e: Map[i64, i64] = Map.new();
    e.extend([(3, 4)].to_vec());
    e.retain(|k, v| true);
    match e.get_mut(3) { Some(r) => { *r += 1; }, None => {} }
    let c: Map[i64, i64] = (0..3).map(|i| (i, i)).collect();
    let mut t: Set[i64] = Set.from([1, 2].to_vec());
    t.retain(|x| x != 1);
    t.extend([7].to_vec());
    let u: Set[i64, FxBuildHasher] = (0..4).collect();
    println(f"{d[1]} {q[3]} {x[5]} {e[3]} {c.len()} {c[2]} {t.contains(1)} {t.contains(7)} {u.len()}");
}
"#,
        "2 4 6 5 3 2 false true 4\n",
    );
}
