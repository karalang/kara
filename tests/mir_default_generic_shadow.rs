//! B-2026-10-21-4: a trait default method whose own generic shares a name
//! with the implementing impl's (`Iterator.max_by_key[K]` copied into
//! `impl[K, V] Iterator for SortedMapIter[K, V]`) keeps the two apart, so
//! the instance is not read at the closure's key type in the map's place.
//! The library names every default method's generics apart from its
//! iterator impls' (`fold[AccV]` beside `IterZip[A, B]`).
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"fn main() {
    let mut s: SortedMap[String, i64] = SortedMap.new();
    s.insert("ann".to_string(), 3);
    s.insert("bob".to_string(), 2);
    s.insert("cat".to_string(), 1);
    let e = s.iter().max_by_key(|(w, n)| *n).unwrap();
    println(f"{e.0} {e.1}");
    let f = s.iter().min_by_key(|(w, n)| *n).unwrap();
    println(f"{f.0} {f.1}");
    let mut t: SortedMap[i64, String] = SortedMap.new();
    t.insert(1, "ann".to_string());
    t.insert(2, "cat".to_string());
    t.insert(3, "bob".to_string());
    let g = t.iter().max_by_key(|(k, v)| v.clone()).unwrap();
    println(f"{g.0} {g.1}");
    let mut m: Map[String, i64] = Map.new();
    m.insert("bob".to_string(), 2);
    m.insert("ann".to_string(), 5);
    m.insert("cat".to_string(), 3);
    let h = m.iter().min_by_key(|(w, n)| (-*n, w.clone())).unwrap();
    println(f"{h.0} {h.1}");
}
"#;

const COLLIDING: &str = r#"fn main() {
    let a: Vec[i64] = vec![1, 2, 3];
    let b: Vec[i64] = vec![4, 5];
    let s = a.iter().chain(b.iter()).fold("x".to_string(), |acc, x| f"{acc}{x}");
    println(s);
    let t: Vec[String] = a.iter().map(|x| f"n{x}").filter(|s| s.len() > 1).map(|s| s.len() * 10).map(|n| f"<{n}>").collect();
    println(f"{t.len()} {t[0]}");
    let u = a.iter().scan(0, |acc, x| Some((acc + *x, f"{acc}"))).scan("s".to_string(), |acc, y| Some((f"{acc}{y}", acc.len()))).fold(0, |p, q| p + q);
    println(u);
    let m = a.iter().map(|x| *x as f64).filter_map(|f| if f > 1.0 { Some(f"{f}") } else { None }).fold(0, |p, q| p + q.len());
    println(m);
    let z = a.iter().zip(b.iter()).fold(0.5, |p, q| p + (*q.0 * *q.1) as f64);
    println(z);
    let mut sm: SortedMap[String, i64] = SortedMap.new();
    sm.insert("ab".to_string(), 1);
    sm.insert("c".to_string(), 2);
    let r = sm.iter().fold(0.0, |p, kv| p + (kv.0.len() as f64) * (*kv.1 as f64));
    println(r);
    let w = sm.iter().map(|kv| kv.0.len()).fold("".to_string(), |acc, n| f"{acc}{n}");
    println(w);
    let fm = sm.iter().find_map(|kv| if *kv.1 == 2 { Some(kv.0.clone()) } else { None });
    println(fm.unwrap());
}
"#;

const COLLIDING_WANT: &str = "x12345\n3 <20>\n6\n6\n14.5\n4.0\n21\nc\n";

const WANT: &str = "ann 3\ncat 1\n2 cat\nann 5\n";

#[test]
fn a_default_method_generic_named_like_the_impls_is_its_own() {
    both_backends("key", SRC, WANT);
}

#[test]
fn every_iterator_default_with_generics_runs_over_colliding_impls() {
    both_backends("all", COLLIDING, COLLIDING_WANT);
}

fn both_backends(tag: &str, src: &str, want: &str) {
    let interp = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(interp.output, want);
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!(
        "karac-mir-default-shadow-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    if linked.is_some() {
        let out = Command::new(&exe).output().expect("run");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8(out.stdout).unwrap(), want);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
