//! Natives over collections whose element is a reference (B-2026-11-02-38):
//! `Vec[ref String].fill(r)` read through the reference and copied the
//! `String`'s first word as the element, and a set's `entry_at` and a map's
//! index gave the address of the slot holding the reference where the MIR
//! reads the reference itself (a borrow of a borrow is the borrow). Each
//! program is built and run, and must print what the interpreter prints.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

fn check(tag: &str, src: &str, expected: &str) {
    let dir = std::env::temp_dir().join(format!("karac-ref-elems-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let interp = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        expected,
        "{interp:?}"
    );
    let exe = dir.join("main.out");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let ok = if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    };
    if common::link_or_skip(ok).is_none() {
        return;
    }
    let ran = Command::new(&exe).output().unwrap();
    assert!(ran.status.success(), "{ran:?}");
    assert_eq!(String::from_utf8_lossy(&ran.stdout), expected);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `Vec[ref String]` filled with a reference holds that reference (it
/// read through it before, and copied the `String`'s buffer pointer).
#[test]
fn mir_ref_elements_fill() {
    let src = r#"fn main() {
    let s = "a".to_string();
    let r = ref s;
    let mut v: Vec[ref String] = Vec.new();
    v.push(r);
    v.fill(r);
    println(v[0]);
}
"#;
    check("fill", src, "a\n");
}

/// Iterating a `SortedSet[ref String]` gives the references it holds.
#[test]
fn mir_ref_elements_sorted_set_iter() {
    let src = r#"fn main() {
    let s = "a".to_string();
    let mut t: SortedSet[ref String] = SortedSet.new();
    let r = ref s;
    t.insert(r);
    println(t.len());
    for x in t {
        println(x);
    }
}
"#;
    check("sorted_set_iter", src, "1\na\n");
}

/// A `Set`, `Map`, `SortedMap` and `VecDeque` of references, and
/// `Vec.filled` with one.
#[test]
fn mir_ref_elements_collections() {
    let src = r#"fn main() {
    let s = "a".to_string();
    let u = "bc".to_string();
    let r = ref s;
    let q = ref u;
    let mut hs: Set[ref String] = Set.new();
    hs.insert(r);
    for x in hs {
        println(x);
    }
    println(hs.contains(r));
    let mut m: Map[i64, ref String] = Map.new();
    m.insert(1, r);
    m.insert(2, q);
    println(m[2]);
    match m.get(1) {
        Some(v) => println(v),
        None => println("none"),
    }
    let mut sm: SortedMap[i64, ref String] = SortedMap.new();
    sm.insert(5, q);
    sm.insert(3, r);
    for (k, v) in sm {
        println(f"{k} {v}");
    }
    let mut ks: SortedMap[ref String, i64] = SortedMap.new();
    ks.insert(q, 7);
    ks.insert(r, 9);
    for (k, v) in ks {
        println(f"{k} {v}");
    }
    let mut d: VecDeque[ref String] = VecDeque.new();
    d.push_back(q);
    d.push_front(r);
    for x in d {
        println(x);
    }
    let v: Vec[ref String] = Vec.filled(2, q);
    println(f"{v[1]} {v.len()}");
}
"#;
    check(
        "collections",
        src,
        "a\ntrue\nbc\na\n3 a\n5 bc\na 9\nbc 7\na\nbc\nbc 2\n",
    );
}

/// A map's `values`, `keys`, `entry(..).or_insert` and index, with
/// reference values.
#[test]
fn mir_ref_elements_map_views() {
    let src = r#"fn main() {
    let s = "a".to_string();
    let u = "bc".to_string();
    let r = ref s;
    let q = ref u;
    let mut m: Map[i64, ref String] = Map.new();
    m.insert(1, r);
    m.insert(2, q);
    let mut total = 0;
    for v in m.values() {
        total = total + v.len();
    }
    println(total);
    let mut ks: Vec[i64] = Vec.new();
    for k in m.keys() {
        ks.push(k);
    }
    ks.sort();
    println(f"{ks[0]} {ks[1]}");
    m.entry(3).or_insert(q);
    println(m[3]);
    let mut sm: SortedMap[i64, ref String] = SortedMap.new();
    sm.insert(4, r);
    println(sm[4]);
    for v in sm.values() {
        println(v);
    }
}
"#;
    check("map_views", src, "3\n1 2\nbc\na\na\n");
}
