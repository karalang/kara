//! B-2026-10-20-32: `IntoIterator` is the spec's trait (design.md §
//! `Iterator` and `Iterable`): `type Item; fn into_iter(own self) -> impl
//! Iterator[Item = Self.Item]`. A spec-shaped impl is accepted, a generic
//! `C: IntoIterator` consumer calls `.into_iter()` (each impl's `impl
//! Iterator` is its own type, lowered per instance), the collections and a
//! channel receiver implement it, `extend` takes a user `IntoIterator`, and
//! `for` over one is refused with a fix that writes `.into_iter()`. Each
//! program prints the same on the MIR interpreter and the compiled backend.

mod common;

use std::process::Command;

const USER_IMPL: &str = r#"
struct Bag { items: Vec[i64] }
impl IntoIterator for Bag {
    type Item = i64;
    fn into_iter(own self) -> impl Iterator[Item = i64] { self.items.into_iter() }
}
fn total[C: IntoIterator[Item = i64]](c: own C) -> i64 {
    let mut s = 0;
    for x in c.into_iter() { s += x; }
    s
}
fn main() {
    println(total(Bag { items: vec![3, 4] }));
    println(Bag { items: vec![1, 2, 5] }.into_iter().count());
    let mut v: Vec[i64] = Vec.new();
    v.extend(Bag { items: vec![3, 4] });
    println(v.len());
    let mut d: VecDeque[i64] = VecDeque.new();
    d.extend(Bag { items: vec![9] });
    println(d.len());
}
"#;

const GENERIC_IMPL: &str = r#"
struct Bag[T] { items: Vec[T] }
impl[T] IntoIterator for Bag[T] {
    type Item = T;
    fn into_iter(own self) -> impl Iterator[Item = T] { self.items.into_iter() }
}
fn first[C: IntoIterator](c: own C) -> Option[C.Item] {
    let mut it = c.into_iter();
    it.next()
}
fn evens[T: Copy](v: own Vec[T]) -> impl Iterator[Item = T] { v.into_iter() }
fn main() {
    let b = Bag { items: vec!["a".to_string(), "b".to_string()] };
    println(first(b).unwrap());
    for x in (Bag { items: vec![1, 2] }).into_iter() { println(x); }
    let mut s = 0;
    for x in evens(vec![1, 2, 3]) { s += x; }
    println(s);
}
"#;

const COLLECTIONS: &str = r#"
fn total[C: IntoIterator[Item = i64]](c: own C) -> i64 {
    let mut s = 0;
    for x in c.into_iter() { s += x; }
    s
}
fn count[C: IntoIterator](c: own C) -> i64 { c.into_iter().count() }
fn main() {
    println(total(vec![1, 2, 3]));
    let mut d: VecDeque[i64] = VecDeque.new();
    d.push_back(5);
    println(total(d));
    let mut s: SortedSet[i64] = SortedSet.new();
    s.insert(7);
    s.insert(8);
    println(total(s));
    let mut m: Map[String, i64] = Map.new();
    m.insert("a".to_string(), 1);
    println(count(m));
    let v = vec![4, 5];
    for x in v { println(x); }
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(1);
    {
        let t = tx;
        let _ = t.send(3);
    }
    println(total(rx));
}
"#;

const FOR_OVER_INTO_ITERATOR: &str = r#"
struct Bag { items: Vec[i64] }
impl IntoIterator for Bag {
    type Item = i64;
    fn into_iter(own self) -> impl Iterator[Item = i64] { self.items.into_iter() }
}
fn main() {
    let b = Bag { items: vec![3, 4] };
    for x in b { println(x); }
}
"#;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-into-iterator-{}-{tag}", std::process::id()));
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
fn a_spec_shaped_into_iterator_impl_is_consumed_through_its_bound() {
    runs_on_both("user", USER_IMPL, "7\n3\n2\n1\n");
}

#[test]
fn a_generic_impl_trait_return_lowers_per_instance() {
    runs_on_both("generic", GENERIC_IMPL, "a\n1\n2\n6\n");
}

#[test]
fn the_collections_and_a_receiver_are_into_iterator() {
    runs_on_both("collections", COLLECTIONS, "6\n5\n15\n1\n4\n5\n3\n");
}

#[test]
fn a_for_over_an_into_iterator_is_refused_with_a_fix() {
    let dir = scratch("for");
    let file = dir.join("main.kara");
    std::fs::write(&file, FOR_OVER_INTO_ITERATOR).unwrap();
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(!check.status.success(), "{err}");
    assert!(
        err.contains("type 'Bag' cannot be iterated by a `for` loop: it implements `IntoIterator` but not `Iterator`"),
        "{err}"
    );
    let fix = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("fix")
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&file).unwrap();
    assert!(fixed.contains("for x in b.into_iter() {"), "{fixed}");
    let out = karac::mir::lower::run_source(&fixed).expect("MIR interpreter");
    assert_eq!(out.output, "3\n4\n");
    let _ = std::fs::remove_dir_all(&dir);
}
