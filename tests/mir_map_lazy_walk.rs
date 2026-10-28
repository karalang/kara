//! A hashed table with holes maps an index to its slot through a Fenwick
//! tree that is built only when a walk asks for it, then kept current by
//! the inserts and removes after it, and rebuilt when the table grows or
//! compacts (B-2026-10-24-6). Walks between removes, a walk after growth
//! with holes, and two tasks walking one table at once (the first builds
//! the tree while the other waits) must each see every entry once. The
//! expected output is a Python mirror's (dict and set over the same ops).
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"fn total(m: ref Map[i64, i64]) -> i64 {
    let mut t = 0;
    let mut n = 0;
    for (k, v) in m.iter() {
        t = t + k * 1000 + v;
        n = n + 1;
    }
    t * 1000 + n
}

fn main() {
    let mut m: Map[i64, i64] = Map.new();
    let mut i = 0;
    let mut acc = 0;
    while i < 2000 {
        m.insert((i * 13) % 211, i);
        if i % 2 == 0 { m.remove((i * 7) % 211); }
        if i % 50 == 0 { acc = acc + total(m); }
        i += 1;
    }
    let (x, y) = par { total(m), total(m) };
    println(f"{m.len()} {total(m)} {acc} {x == y}");
    let mut s: Set[i64] = Set.new();
    for x in 0..100 { s.insert(x); }
    for x in 0..100 { if x % 3 == 0 { s.remove(x); } }
    let mut a = 0;
    for x in s.iter() { a = a + x; }
    for x in 100..400 {
        s.insert(x);
        if x % 5 == 0 { s.remove(x - 50); }
    }
    let mut b = 0;
    let mut c = 0;
    for x in s.iter() {
        b = b + x;
        c += 1;
    }
    println(f"{a} {b} {c} {s.len()}");
}
"#;

const WANT: &str = "159 16716025159 626199663925 true\n3267 66492 309 309\n";

fn compiled(src: &str) -> Option<String> {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-mir-map-lazy-walk-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).unwrap())
}

#[test]
fn mir_map_walk_index_is_built_lazily_through_holes() {
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, WANT);
    if let Some(out) = compiled(SRC) {
        assert_eq!(out, WANT);
    }
}
