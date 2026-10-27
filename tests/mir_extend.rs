//! B-2026-10-15-10: `std.iter.Extend` (design.md §9). Every `Iterator` is
//! `IntoIterator` of itself through the library's blanket `impl[I: Iterator]
//! IntoIterator for I`, so a range or an iterator chain goes wherever an
//! `IntoIterator` does. `trait Extend[T] { fn extend[I: IntoIterator[Item =
//! T]](mut ref self, items: own I); }` is implemented by `Vec`, `VecDeque`,
//! `Set`, `SortedSet` (their element), `Map`, `SortedMap` (`(K, V)`, a later
//! pair replacing an earlier value) and `String` (`char` and `String`), and
//! a generic `C: Extend[T]` reaches each. Each program prints the same on the
//! MIR interpreter and the compiled backend.

mod common;

use std::process::Command;

const BLANKET: &str = r#"
fn total[C: IntoIterator[Item = i64]](c: own C) -> i64 {
    let mut s = 0;
    for x in c.into_iter() { s += x; }
    s
}
fn main() {
    println(total(vec![1, 2, 3]));
    println(total(vec![1, 2, 3].into_iter()));
    println(total((1..4).map(|x| x * 2)));
    println(total(1..5));
    let v = vec![5, 6];
    println(total(v.iter().map(|x| *x)));
}
"#;

const EXTEND_EACH: &str = r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.extend(1..4);
    println(v.len());
    let mut s: Set[i64] = Set.new();
    s.extend(vec![1, 2, 2]);
    println(s.len());
    let mut ss: SortedSet[i64] = SortedSet.new();
    ss.extend((1..4).map(|x| x * 10));
    println(ss.len());
    let mut m: Map[i64, i64] = Map.new();
    m.extend(vec![(1, 2), (1, 3)]);
    println(m[1]);
    let mut sm: SortedMap[String, i64] = SortedMap.new();
    sm.extend(vec![("a".to_string(), 1), ("b".to_string(), 2)]);
    println(sm.len());
    let mut t = "x".to_string();
    t.extend("yz".chars());
    t.extend(vec!["!".to_string()]);
    println(t);
}
"#;

const THROUGH_A_BOUND: &str = r#"
import std.iter.Extend;
fn fill[C: Extend[i64]](c: mut ref C) {
    c.extend(1..4);
    c.extend(vec![10]);
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    fill(mut v);
    println(v.len());
    let mut d: VecDeque[i64] = VecDeque.new();
    fill(mut d);
    println(d.len());
    let mut s: Set[i64] = Set.new();
    fill(mut s);
    s.extend(vec![1, 2]);
    println(s.len());
    let mut ss: SortedSet[i64] = SortedSet.new();
    fill(mut ss);
    println(ss.len());
}
"#;

const WRONG_ITEM: &str = r#"
fn main() {
    let mut s: Set[i64] = Set.new();
    s.extend(vec!["a".to_string()]);
    println(s.len());
}
"#;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-extend-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `src`'s output on the MIR interpreter, and on the compiled backend when
/// this build has one, after asserting the two agree with `want`.
fn runs_on_both(tag: &str, src: &str, want: &str) {
    let out = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(out.output, want, "interpreter");
    if !cfg!(feature = "llvm") {
        return;
    }
    let dir = scratch(tag);
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let exe = dir.join("main");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", file.to_str().unwrap(), "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err)
    });
    if linked.is_some() {
        let ran = Command::new(&exe).output().unwrap();
        assert!(
            ran.status.success(),
            "{}",
            String::from_utf8_lossy(&ran.stderr)
        );
        assert_eq!(String::from_utf8(ran.stdout).unwrap(), want, "compiled");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_iterator_or_a_range_is_into_iterator_of_itself() {
    runs_on_both("blanket", BLANKET, "6\n6\n12\n10\n11\n");
}

#[test]
fn each_collection_and_string_extends_through_its_extend_impl() {
    runs_on_both("each", EXTEND_EACH, "3\n2\n3\n3\n2\nxyz!\n");
}

#[test]
fn a_generic_extend_bound_reaches_each_collection() {
    runs_on_both("bound", THROUGH_A_BOUND, "4\n4\n4\n4\n");
}

#[test]
fn an_extend_with_other_items_is_refused() {
    let dir = scratch("wrong");
    let file = dir.join("main.kara");
    std::fs::write(&file, WRONG_ITEM).unwrap();
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(!check.status.success(), "{err}");
    assert!(
        err.contains("associated-type bound `I.Item = i64` is not satisfied; `Vec[String]` has `Item = String`"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
