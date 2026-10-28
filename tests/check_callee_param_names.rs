//! A generic library method's own parameter is its own, whatever the caller
//! calls its parameters (B-2026-10-27-34). Inside `fn run_all[T]`, the
//! closure given to `TaskGroup.spawn[T]` was checked against `spawn`'s `T`
//! named back, which the rigid-parameter check read as the caller's `T`; the
//! same program with the caller's parameter renamed `U` was accepted.

use std::path::PathBuf;
use std::process::{Command, Output};

fn write(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-callee-param-names-{}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Each caller names its parameter as the library method it calls names its
/// own: `TaskGroup.spawn[T]`, `IsolatingGroup.spawn[T]`, `Iterator.map[OutV]`.
const SRC: &str = "import std.sync.{TaskGroup};
shared struct Cell { n: i64 }
fn run_all[T](x: own T) -> i64 {
    let g = TaskGroup.new();
    let h = g.spawn(|| { let y = x; 1 });
    h.join()
}
fn isolated[T](x: own T) -> i64 {
    let g = TaskGroup.isolating();
    let h = g.spawn(|| { let y = x; 4 });
    match h.join() {
        Ok(v) => v,
        Err(_) => 0,
    }
}
fn mapped[OutV](x: ref OutV, v: ref Vec[i64]) -> i64 {
    let w: Vec[i64] = v.iter().map(|a| a + 1).collect();
    w.iter().fold(0, |s, a| s + a)
}
fn main() {
    println(run_all(Cell { n: 1 }));
    println(isolated(Cell { n: 2 }));
    let v: Vec[i64] = [1, 2, 3].to_vec();
    println(mapped(7, v));
}
";

#[test]
fn a_library_generic_keeps_its_parameter_past_a_caller_of_the_same_name() {
    let path = write("spawn", SRC);
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{}", text(&out.stderr));
    for lane in [&["run"][..], &["run", "--interp"]] {
        let out = karac(lane, &path);
        assert!(out.status.success(), "{lane:?}: {}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "1\n4\n9\n", "{lane:?}");
    }
}

/// The caller's own parameter is still rigid: a concrete value is not a `T`.
#[test]
fn the_callers_parameter_stays_rigid_in_its_own_closure() {
    let path = write(
        "rigid",
        "import std.sync.{TaskGroup};
fn run_all[T](x: own T) -> T {
    let g = TaskGroup.new();
    let h = g.spawn(|| { let y = x; 1 });
    h.join()
}
fn main() {
    println(run_all(3));
}
",
    );
    let out = karac(&["check"], &path);
    let err = text(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("expected 'T', found 'i64'"), "{err}");
}

/// The enclosing item's own parameters a receiver brings into the call
/// (`VecIntoIter[(K, V)]` inside `impl[K, V]`) are its own, named back as
/// they were: 16a2058c4a left them unnamed too, and `keys` below was refused
/// with "cannot infer type parameter 'OutV'" (corpus/apps/cli/mini_collections).
#[test]
fn the_callers_parameters_a_receiver_brings_stay_the_callers() {
    let path = write(
        "receiver",
        "struct Dict[K, V] { ks: Vec[K], vs: Vec[V] }
impl[K: Clone, V: Clone] Dict[K, V] {
    fn items(ref self) -> Vec[(K, V)] {
        let mut out: Vec[(K, V)] = Vec.new();
        let mut i = 0;
        while i < self.ks.len() {
            out.push((self.ks[i].clone(), self.vs[i].clone()));
            i = i + 1;
        }
        out
    }
    fn keys(ref self) -> Vec[K] {
        self.items().into_iter().map(|(k, _)| k).collect()
    }
}
fn main() {
    let d = Dict { ks: [1, 2].to_vec(), vs: [3, 4].to_vec() };
    println(d.keys().len());
}
",
    );
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = karac(&["run", "--interp"], &path);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "2\n");
}
