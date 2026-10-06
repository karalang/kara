//! B-2026-10-06-65: compiled `SortedMap` / `SortedSet` keep an ordered key index

use super::*;

/// B-2026-10-06-65: the ordered queries now read a key index the runtime
/// keeps current, instead of sorting every key per call. Every path that adds
/// or removes a key has to keep it current, including codegen's inline
/// integer-key insert, which bypasses the runtime and so must divert while the
/// index exists. Each step below queries right after one kind of mutation:
/// an insert through the inline path, a remove, an update of an existing key,
/// an `entry` insert, a `clear`, `String` keys, a `SortedSet`, and a churn
/// loop checked against a brute-force scan. A stale index answers one of them
/// with a key that is gone or misses one that arrived.
#[test]
fn e2e_sorted_map_ordered_index_tracks_every_mutation() {
    let Some(out) = run_program(
        r#"fn show(m: ref SortedMap[i64, i64]) -> String {
    let mut out = "";
    for (k, v) in m {
        out = out + f"{k}={v} ";
    }
    return out;
}

fn floor_of(m: ref SortedMap[i64, i64], k: i64) -> String {
    match m.floor(k) {
        Some((a, b)) => return f"{a}={b}",
        None => return "none",
    }
}

fn main() {
    let mut m: SortedMap[i64, i64] = SortedMap.new();
    m.insert(5, 50);
    m.insert(1, 10);
    m.insert(9, 90);
    println(f"floor 6: {floor_of(m, 6)}");
    m.insert(7, 70);
    println(f"floor 8 after insert 7: {floor_of(m, 8)}");
    m.remove(7);
    println(f"floor 8 after remove 7: {floor_of(m, 8)}");
    m.insert(5, 55);
    println(f"floor 5 after update: {floor_of(m, 5)}");
    *m.entry(3).or_insert(30) += 1;
    println(f"floor 4 after entry 3: {floor_of(m, 4)}");
    println(f"walk: {show(m)}");
    m.clear();
    println(f"floor 100 after clear: {floor_of(m, 100)}");
    m.insert(2, 20);
    match m.max() {
        Some((k, v)) => println(f"max {k}={v}"),
        None => println("max none"),
    }

    let mut s: SortedMap[String, i64] = SortedMap.new();
    s.insert("pear", 1);
    s.insert("apple", 2);
    s.insert("fig", 3);
    match s.ceiling("b") {
        Some((k, v)) => println(f"ceiling b: {k}={v}"),
        None => println("ceiling b: none"),
    }
    s.remove("fig");
    match s.ceiling("b") {
        Some((k, v)) => println(f"ceiling b after remove fig: {k}={v}"),
        None => println("none"),
    }
    s.insert("banana", 4);
    match s.ceiling("b") {
        Some((k, v)) => println(f"ceiling b after insert banana: {k}={v}"),
        None => println("none"),
    }
    let mut keys = "";
    for (k, v) in s {
        keys = keys + f"{k}:{v} ";
    }
    println(f"string walk: {keys}");

    let mut set: SortedSet[i64] = SortedSet.new();
    set.insert(4);
    set.insert(2);
    set.insert(8);
    match set.min() {
        Some(x) => println(f"set min {x}"),
        None => println("set min none"),
    }
    set.insert(1);
    match set.min() {
        Some(x) => println(f"set min after insert 1: {x}"),
        None => println("none"),
    }
    set.remove(1);
    match set.min() {
        Some(x) => println(f"set min after remove 1: {x}"),
        None => println("none"),
    }
    let mut walk = "";
    for x in set {
        walk = walk + f"{x} ";
    }
    println(f"set walk: {walk}");

    // Interleaved churn against a brute-force oracle over a presence table.
    let mut churn: SortedMap[i64, i64] = SortedMap.new();
    let mut present: Vec[bool] = Vec.filled(64, false);
    let mut state: i64 = 7;
    let mut agree = 0;
    for step in 0..600 {
        state = (state * 1103515245 + 12345) % 2147483648;
        let k = (state >> 8) % 64;
        if (state >> 20) % 3 == 0 {
            churn.remove(k);
            present[k] = false;
        } else {
            churn.insert(k, step);
            present[k] = true;
        }
        let q = (state >> 12) % 64;
        let mut want = -1;
        for i in 0..q + 1 {
            if present[i] {
                want = i;
            }
        }
        let got = match churn.floor(q) {
            Some((a, _)) => a,
            None => -1,
        };
        let mut want_c = -1;
        let mut i = 63;
        while i >= q {
            if present[i] {
                want_c = i;
            }
            i -= 1;
        }
        let got_c = match churn.ceiling(q) {
            Some((a, _)) => a,
            None => -1,
        };
        if got == want and got_c == want_c {
            agree += 1;
        }
    }
    println(f"churn agreed {agree} of 600, {churn.len()} keys left");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        "floor 6: 5=50\nfloor 8 after insert 7: 7=70\nfloor 8 after remove 7: 5=50\nfloor 5 after update: 5=55\nfloor 4 after entry 3: 3=31\nwalk: 1=10 3=31 5=55 9=90 \nfloor 100 after clear: none\nmax 2=20\nceiling b: fig=3\nceiling b after remove fig: pear=1\nceiling b after insert banana: banana=4\nstring walk: apple:2 banana:4 pear:1 \nset min 2\nset min after insert 1: 1\nset min after remove 1: 2\nset walk: 2 4 8 \nchurn agreed 600 of 600, 50 keys left\n"
    );
}
