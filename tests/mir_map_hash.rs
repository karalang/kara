//! A hashed table's key is hashed by compiled code, not the runtime
//! (`karac_mir_map_find_hv` / `_insert_hv`): a scalar key's hash is a
//! pure call, so a `get` and the `insert` after it hash it once (kata
//! 219). The tables still agree with the interpreter, removals included.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"fn main() {
    let mut last: Map[i64, i64] = Map.new();
    let mut hits = 0;
    let mut i = 0;
    while i < 200 {
        let x = (i * 7) % 31;
        match last.get(x) {
            Some(j) => {
                if i - j <= 40 { hits = hits + 1; }
            }
            None => {}
        }
        let _ = last.insert(x, i);
        i = i + 1;
    }
    let _ = last.remove(3);
    let _ = last.insert(3, 1000);
    println(f"{hits} {last.len()} {last.get(3)} {last.contains_key(4)} {last.contains_key(99)}");
    let mut words: Map[String, i64] = Map.new();
    for w in ["a", "bb", "a", "ccc", "bb", "a"] {
        let n = match words.get(w.to_string()) { Some(c) => c + 1, None => 1 };
        let _ = words.insert(w.to_string(), n);
    }
    println(f"{words.get(String.from("a"))} {words.get(String.from("bb"))} {words.len()}");
    let mut s: Set[i64] = Set.new();
    let _ = s.insert(5);
    let _ = s.insert(5);
    let _ = s.insert(6);
    let mut sm: SortedMap[i64, i64] = SortedMap.new();
    let _ = sm.insert(2, 20);
    let _ = sm.insert(1, 10);
    let _ = sm.insert(2, 21);
    println(f"{s.len()} {s.contains(6)} {sm.get(2)} {sm.len()}");
}
"#;

const WANT: &str = "169 31 Some(1000) true false\nSome(3) Some(2) 3\n2 true Some(21) 2\n";

fn ir(src: &str) -> String {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    karac::codegen::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR")
}

fn compiled(ir: &str) -> Option<String> {
    let dir = std::env::temp_dir().join(format!("karac-mir-map-hash-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::codegen::mir_aot::compile_object(ir, &obj, 2).expect("object");
    common::link_or_skip(karac::codegen::link_executable(
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
fn mir_map_hashes_a_key_in_compiled_code() {
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, WANT);
    let ir = ir(SRC);
    assert!(ir.contains("@karac_mir_map_find_hv("), "{ir}");
    assert!(ir.contains("@karac_mir_map_insert_hv("), "{ir}");
    assert!(
        ir.contains("memory(none)"),
        "the word hash is not pure: {ir}"
    );
    if let Some(out) = compiled(&ir) {
        assert_eq!(out, WANT);
    }
}
