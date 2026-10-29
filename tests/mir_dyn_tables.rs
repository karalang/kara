//! A `Map` or `SortedMap` of `dyn` values (design.md § `dyn Trait`): a
//! reference to a stored value is the value's two-word pair, from `m[k]`,
//! `ref m[k]`, `mut ref m[k]`, `get` and `values` alike. Compiled, the
//! index once wrote the slot's address into the pair, so a present key read
//! as missing (B-2026-10-29-30). Each program runs the same on the
//! interpreter and compiled at -O0 and -O2.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// What the program prints compiled at `opt`, and whether it exited
/// cleanly; `None` when there is no runtime archive to link.
fn build_and_run(src: &str, opt: u8) -> Option<(bool, String)> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("karac-dyn-tables-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, opt).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    let out = linked.map(|()| Command::new(&exe).output().expect("run"));
    let _ = std::fs::remove_dir_all(&dir);
    let out = out?;
    Some((
        out.status.success(),
        String::from_utf8(out.stdout).expect("utf-8"),
    ))
}

/// The interpreter prints `want`, and so does the program compiled.
fn runs(src: &str, want: &str) {
    let got = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(got, want);
    for opt in [0, 2] {
        if let Some((ok, out)) = build_and_run(src, opt) {
            assert!(ok, "at -O{opt}: {out}");
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[test]
fn mir_dyn_map_values_lend_their_pair() {
    runs(
        r#"trait Show {
    fn show(ref self) -> String;
    fn bump(mut ref self);
}
struct A { n: i64 }
impl Show for A {
    fn show(ref self) -> String { f"a {self.n}" }
    fn bump(mut ref self) { self.n += 10; }
}
struct B {}
impl Show for B {
    fn show(ref self) -> String { "b" }
    fn bump(mut ref self) {}
}
fn main() {
    let mut m: Map[String, dyn Show] = Map.new();
    m.insert("x", A { n: 5 });
    m.insert("y", B {});
    let r = ref m["x"];
    println(r.show());
    println(m["y"].show());
    m["x"].bump();
    let w = mut ref m["x"];
    w.bump();
    println(m["x"].show());
    match m.get("y") {
        Some(s) => println(s.show()),
        None => println("none"),
    }
    let mut vals: Vec[String] = Vec.new();
    for v in m.values() { vals.push(v.show()); }
    vals.sort();
    println(vals.join(","));
    let mut s: SortedMap[i64, dyn Show] = SortedMap.new();
    s.insert(2, A { n: 2 });
    s.insert(1, B {});
    println(f"{s[2].show()} {s[1].show()}");
    for v in s.values() { println(v.show()); }
}
"#,
        "a 5
b
a 25
b
a 25,b
a 2 b
b
a 2
",
    );
}
