//! Kata 2284's iterator arm keys `max_by_key` on a `Map`'s owned pairs:
//! `totals.into_iter().max_by_key(|(name, total)| (total, name))`. The
//! key's `name` is a `ref String` into the item, and `max_by_key` moves
//! that item while it keeps the best key, so the program passed check and
//! failed borrowck inside the library's body. Check now refuses the
//! borrowed part and `karac fix` clones it (B-2026-11-18-84).
//!
//! Probing that, `let m: Vec[ref String] = w.into_iter().collect()`
//! passed check too: each owned item is dropped as it is gathered, and a
//! build wrote past the stack under AddressSanitizer. Owned items for a
//! collection of references are refused now (B-2026-11-18-85).

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn kata_2284_max_by_key_borrowing_its_item_is_cloned() {
    let src = "fn main() {
    let v: Vec[(String, i64)] = [(\"a\".to_string(), 2), (\"b\".to_string(), 2), (\"c\".to_string(), 1)];
    let best = v.into_iter().max_by_key(|(name, total)| (total, name)).unwrap();
    let w: Vec[String] = [\"y\".to_string(), \"x\".to_string()];
    let least = w.into_iter().min_by_key(|s| s).unwrap();
    println(f\"{best.0} {least}\");
}
";
    let file = write("k2284-key", src);
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        stderr.contains("`max_by_key`'s key cannot keep a borrow of the item")
            && stderr.contains("`min_by_key`'s key cannot keep a borrow of the item"),
        "{stderr}"
    );
    let fix = karac(&["fix", &file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        fixed.contains("(total, name.clone())") && fixed.contains("|s| s.clone()"),
        "{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "b x\n");
}

#[test]
fn kata_2284_a_key_over_borrowed_items_or_captures_is_kept() {
    let src = "fn main() {
    let v: Vec[(String, i64)] = [(\"a\".to_string(), 2), (\"b\".to_string(), 2)];
    let best = v.iter().max_by_key(|(name, total)| (total, name)).unwrap();
    let names = [\"q\".to_string(), \"p\".to_string()];
    let at = [0, 1].into_iter().min_by_key(|i| ref names[i]).unwrap();
    println(f\"{best.0} {at}\");
}
";
    let file = write("k2284-key-kept", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "b 1\n");
}

#[test]
fn kata_2284_owned_items_for_references_are_refused() {
    let src = "fn main() {
    let w: Vec[String] = [\"b\".to_string(), \"a\".to_string()];
    let m: Vec[ref String] = w.into_iter().collect();
    println(f\"{m}\");
}
";
    let file = write("k2284-collect", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        !check.status.success()
            && stderr.contains("`Vec[ref String]: FromIterator[String]` is not satisfied: it gathers `ref String`, a reference"),
        "{stderr}"
    );
}

#[test]
fn kata_2284_borrowed_items_for_references_are_kept() {
    let src = "fn main() {
    let w: Vec[String] = [\"b\".to_string(), \"a\".to_string()];
    let m: Vec[ref String] = w.iter().collect();
    let n: Vec[(ref String, i64)] = w.iter().map(|s| (s, s.len())).collect();
    println(f\"{m} {n.len()}\");
}
";
    let file = write("k2284-collect-kept", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "[b, a] 2\n");
}

#[test]
fn kata_2284_borrowed_copy_items_for_references_are_kept_and_owned_ones_refused() {
    let kept = "fn keep(v: Vec[i64], k: i64) -> Vec[ref i64] {
    v.iter().filter(|x| *x > k).collect()
}
fn main() {
    let v = [1, 5, 3, 9];
    let mut s: SortedSet[i64] = SortedSet.new();
    s.insert(4);
    s.insert(6);
    let b: Vec[ref i64] = s.range_inclusive(3, 7).collect();
    println(f\"{keep(v, 2)} {b}\");
}
";
    let file = write("k2284-copy-kept", kept);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let out = karac::mir::lower::run_source(kept)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "[5, 3, 9] [4, 6]\n");
    let owned = "fn main() {
    let w: Vec[i64] = [1, 2];
    let m: Vec[ref i64] = w.into_iter().collect();
    println(f\"{m}\");
}
";
    let file = write("k2284-copy-owned", owned);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        !check.status.success()
            && stderr.contains(
                "`Vec[ref i64]: FromIterator[i64]` is not satisfied: it gathers `ref i64`"
            ),
        "{stderr}"
    );
}
