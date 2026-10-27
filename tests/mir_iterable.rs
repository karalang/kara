//! B-2026-10-27-11: `Iterable` (design.md § `Iterator` and `Iterable`), the
//! lang-item trait `for` borrows through. `for x in c` over a type with an
//! `Iterable` impl calls `c.iter()`, which borrows `c`, and binds each item
//! of the iterator it returns: the impl's `Item`, a `ref` into `c` when the
//! impl says so. A generic `C: Iterable[Item = T]` binds `T`. A `for` over a
//! type that is neither an `Iterator` nor `Iterable` names both. Each program
//! checks and prints the same on the MIR interpreter and the compiled backend.

mod common;

use std::process::Command;

const ROW: &str = r#"
struct Bag { items: Vec[i64] }
impl Iterable for Bag {
    type Item = ref i64;
    fn iter(self) -> impl Iterator[Item = ref i64] { self.items.iter() }
}
fn main() { let b = Bag { items: vec![3, 4] }; for x in b { println(x); } println(b.items.len()); }
"#;

const SHAPES: &str = r#"
struct Stack[T] { items: Vec[T] }
impl[T] Iterable for Stack[T] {
    type Item = ref T;
    fn iter(self) -> impl Iterator[Item = ref T] { self.items.iter() }
}
struct Countdown { n: i64 }
struct Down { k: i64 }
impl Iterator for Down {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] {
        if self.k == 0 { return None; }
        self.k -= 1;
        Some(self.k + 1)
    }
}
impl Iterable for Countdown {
    type Item = i64;
    fn iter(self) -> impl Iterator[Item = i64] { Down { k: self.n } }
}
fn longest(s: Stack[String]) -> i64 {
    let mut best = 0;
    for w in s {
        if w.len() == 0 { continue; }
        if w.len() > best { best = w.len(); }
    }
    best
}
fn total[C: Iterable[Item = i64]](c: C) -> i64 {
    let mut t = 0;
    for x in c { t += x; }
    t
}
fn main() {
    let s = Stack { items: vec!["a".to_string(), "".to_string(), "abc".to_string()] };
    println(longest(s));
    for w in s { if w == "abc" { break; } println(w); }
    let c = Countdown { n: 4 };
    for k in c { print(f"{k} "); }
    println("");
    println(total(c));
    let mut n = 0;
    for x in s.iter() { n += x.len(); }
    println(n);
    println(s.items.len());
}
"#;

const MOVE_OUT: &str = r#"
struct Stack[T] { items: Vec[T] }
impl[T] Iterable for Stack[T] {
    type Item = ref T;
    fn iter(self) -> impl Iterator[Item = ref T] { self.items.iter() }
}
fn main() {
    let s = Stack { items: vec!["a".to_string()] };
    let mut out: Vec[String] = Vec.new();
    for w in s { out.push(w); }
    println(out.len());
}
"#;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-iterable-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `karac check` on `src`: whether it passes, and its stderr.
fn check(tag: &str, src: &str) -> (bool, String) {
    let dir = scratch(tag);
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// `src` checks, and prints `want` on the MIR interpreter and on the
/// compiled backend when this build has one.
fn checks_and_runs(tag: &str, src: &str, want: &str) {
    let (ok, err) = check(tag, src);
    assert!(ok, "check: {err}");
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
fn a_for_over_a_user_iterable_borrows_it_through_iter() {
    checks_and_runs("row", ROW, "3\n4\n2\n");
}

#[test]
fn an_iterable_generic_type_a_bound_and_its_own_iterator_each_loop() {
    checks_and_runs("shapes", SHAPES, "3\na\n\n4 3 2 1 \n10\n4\n3\n");
}

#[test]
fn an_iterable_item_that_borrows_cannot_be_moved_out() {
    let (ok, err) = check("move-out", MOVE_OUT);
    assert!(!ok);
    assert!(
        err.contains("expected 'String', found 'ref String': a borrowed value cannot be moved out of what it borrows"),
        "{err}"
    );
}

#[test]
fn a_for_over_a_type_that_is_not_iterable_names_iterable() {
    let (ok, err) = check(
        "not-iterable",
        "struct Point { x: i64 }\nfn main() { let p = Point { x: 1 }; for q in p { println(q.x); } }\n",
    );
    assert!(!ok);
    assert!(
        err.contains("type 'Point' cannot be iterated by a `for` loop: it implements neither `Iterator` nor `Iterable`"),
        "{err}"
    );
}
