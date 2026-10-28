//! B-2026-10-27-28: the six collections are `Iterable` (design.md §
//! `Iterator` and `Iterable`, collections.md § Iteration), so a generic
//! `C: Iterable[Item = ref T]` takes a `Vec`, `VecDeque`, `Set` or
//! `SortedSet`, and `C: Iterable[Item = (ref K, ref V)]` a `Map` or
//! `SortedMap`. A collection's `Iterable` `Item` (`ref T`) stands beside its
//! `IntoIterator` one (`T`) without replacing it. An iterator is not
//! `Iterable`. Each program checks and prints the same on the MIR
//! interpreter and the compiled backend.

mod common;

use std::process::Command;

const EACH: &str = r#"
fn total[C: Iterable[Item = ref i64]](c: C) -> i64 {
    let mut t = 0;
    for x in c { t += *x; }
    t
}
fn count[C: Iterable](c: C) -> i64 {
    let mut n = 0;
    for _ in c { n += 1; }
    n
}
fn keys[C: Iterable[Item = (ref String, ref i64)]](c: C) -> i64 {
    let mut t = 0;
    for (k, v) in c { t += k.len() + *v; }
    t
}
fn main() {
    let v = vec![1, 2, 3];
    println(total(v));
    let mut d: VecDeque[i64] = VecDeque.new(); d.push_back(4); d.push_back(5);
    println(total(d));
    let s: Set[i64] = Set.from(vec![7, 8]);
    println(total(s));
    let mut ss: SortedSet[i64] = SortedSet.new(); ss.insert(1); ss.insert(9);
    println(total(ss));
    let mut m: Map[String, i64] = Map.new(); m.insert("ab".to_string(), 1);
    println(keys(m));
    let mut sm: SortedMap[String, i64] = SortedMap.new(); sm.insert("abc".to_string(), 2);
    println(keys(sm));
    println(count(v) + count(m));
    for x in v { println(x); }
    let w: Vec[i64] = v.into_iter().map(|x| x * 2).collect();
    println(w.len());
}
"#;

const RANGE: &str = r#"
fn count[C: Iterable](c: C) -> i64 {
    let mut n = 0;
    for _ in c { n += 1; }
    n
}
fn main() { println(count(1..4)); }
"#;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-iterable-coll-{}-{tag}", std::process::id()));
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
fn each_collection_reaches_a_generic_iterable_bound() {
    checks_and_runs("each", EACH, "6\n9\n15\n10\n3\n5\n4\n1\n2\n3\n3\n");
}

#[test]
fn an_iterator_is_not_iterable() {
    let (ok, err) = check("range", RANGE);
    assert!(!ok);
    assert!(
        err.contains("trait bound `C: Iterable` is not satisfied; `Range[i64]` does not implement `Iterable`"),
        "{err}"
    );
}
