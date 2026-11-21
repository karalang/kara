//! B-2026-11-21-60: a `Map`, `SortedMap`, `Set` or `SortedSet` whose keys are
//! references (`Map[ref String, i64]`, valid by design.md's `Vec[ref T]`
//! rule and the 2026-11-08 ruling on a written `Map[ref i64, V]`) passed
//! `karac check`, but its library `into_iter`, `into_keys` and `retain`
//! failed the MIR borrow check inside the library with E0509 "the result
//! borrows _2, which this function owns", or a mutable borrow conflict in
//! `retain`. Each moves the entries out through the native
//! `drain_entries`, whose result was read as a borrow of the collection
//! rather than the entries' own borrows. Kata 1311's Rust solution, whose
//! `HashMap<&String, i32>` `karac fix` writes `Map[ref String, i32]`,
//! found it. A `for` over `m.into_iter()` already ran, by another path.

use std::process::Command;

fn run(tag: &str, body: &str, output: &str) {
    let src = format!(
        "fn main() {{
    let names: Vec[String] = [\"x\", \"y\", \"x\", \"zz\"];
    let mut freq: Map[ref String, i64] = Map.new();
    let mut sorted: SortedMap[ref String, i64] = SortedMap.new();
    let mut set: Set[ref String] = Set.new();
    let mut sset: SortedSet[ref String] = SortedSet.new();
    for s in names.iter() {{
        *freq.entry(s).or_insert(0) += 1;
        *sorted.entry(s).or_insert(0) += 1;
        set.insert(s);
        sset.insert(s);
    }}
{body}
}}
"
    );
    let dir = std::env::temp_dir().join(format!("karac-refkey-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, &src).unwrap();
    let p = f.to_str().unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let check = Command::new(karac).args(["check", p]).output().unwrap();
    assert!(
        check.status.success(),
        "{src}\n{}",
        String::from_utf8_lossy(&check.stdout)
    );
    let out = Command::new(karac).args(["__mir-run", p]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        output,
        "{src}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_map_of_ref_keys_collects_its_entries() {
    run(
        "mapiter",
        "    let mut v: Vec[(ref String, i64)] = freq.into_iter().collect();
    v.sort();
    let w: Vec[(ref String, i64)] = sorted.into_iter().collect();
    println(f\"{v} {w}\");",
        "[(x, 2), (y, 1), (zz, 1)] [(x, 2), (y, 1), (zz, 1)]\n",
    );
}

#[test]
fn a_set_of_refs_collects_its_elements() {
    run(
        "setiter",
        "    let mut v: Vec[ref String] = set.into_iter().collect();
    v.sort();
    let w: Vec[ref String] = sset.into_iter().collect();
    println(f\"{v} {w} {names.len()}\");",
        "[x, y, zz] [x, y, zz] 4\n",
    );
}

#[test]
fn a_map_of_ref_keys_gives_its_keys_and_values() {
    run(
        "keys",
        "    let mut ks: Vec[ref String] = freq.into_keys().collect();
    ks.sort();
    let vs: Vec[i64] = sorted.into_values().collect();
    println(f\"{ks} {vs}\");",
        "[x, y, zz] [2, 1, 1]\n",
    );
}

#[test]
fn ref_key_collections_retain() {
    run(
        "retain",
        "    freq.retain(|k, v| k.len() == 1 and *v == 2);
    sorted.retain(|k, v| k.len() == 1);
    set.retain(|k| k.len() == 2);
    sset.retain(|k| k.len() == 1);
    println(f\"{freq.len()} {sorted.len()} {set.len()} {sset.len()}\");",
        "1 2 1 2\n",
    );
}
