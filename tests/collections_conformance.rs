//! Divergences found by the collections conformance pass over
//! docs/library/collections.md, each program run on the interpreter and,
//! under `--features llvm`, compiled.
//!
//! - `Vec.filled`, `resize` and `fill` copied an element with a user
//!   `Clone` field by field unless the program wrote `.clone()` somewhere,
//!   so the user body never ran and a `Drop` type dropped the same value
//!   twice; compiled `resize` and `fill` also cloned into every slot where
//!   the interpreter moves the value into the last one.
//! - `windows(0)` / `chunks(0)` panicked as "explicit panic".
//! - A float `Map` key's diagnostic now names `F64`, as design.md § Floats
//!   says it does.

mod common;

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-coll-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    file
}

/// Exit code, stdout and stderr of `src` on each lane.
fn lanes(name: &str, src: &str) -> Vec<(&'static str, Option<i32>, String, String)> {
    let file = program(name, src);
    let mut cmds = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        cmds.push("__mir-llvm-run");
    }
    let mut out = Vec::new();
    for cmd in cmds {
        let o = Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg(cmd)
            .arg(&file)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&o.stderr).to_string();
        if cmd == "__mir-llvm-run"
            && stderr.contains("undefined reference")
            && common::link_or_skip(Err(stderr.clone())).is_none()
        {
            continue;
        }
        out.push((
            cmd,
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).to_string(),
            stderr,
        ));
    }
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
    out
}

#[test]
fn coll_filled_resize_fill_call_a_user_clone_and_move_into_the_last_slot() {
    let src = r#"struct D { n: i64, s: String }
impl Drop for D { fn drop(mut ref self) { println(f"drop {self.n} {self.s}"); } }
impl Clone for D { fn clone(self) -> D { D { n: self.n + 1000, s: f"{self.s}c" } } }
struct C { n: i64 }
impl Clone for C { fn clone(self) -> C { C { n: self.n + 1000 } } }
fn main() {
    let f = Vec.filled(2, D { n: 1, s: "a".to_string() });
    println(f"filled {f[0].n} {f[1].n}");
    let mut v: Vec[D] = [];
    v.resize(2, D { n: 2, s: "b".to_string() });
    println(f"resize {v[0].n} {v[1].n}");
    v.resize(2, D { n: 4, s: "u".to_string() });
    let mut w: Vec[D] = [D { n: 0, s: "z".to_string() }, D { n: 0, s: "z".to_string() }];
    w.fill(D { n: 3, s: "c".to_string() });
    println(f"fill {w[0].n} {w[1].n}");
    let mut e: Vec[D] = [];
    e.fill(D { n: 5, s: "v".to_string() });
    let mut c: Vec[C] = [];
    c.resize(3, C { n: 1 });
    let mut cf: Vec[C] = [C { n: 0 }, C { n: 0 }];
    cf.fill(C { n: 7 });
    println(f"copyless {c[0].n} {c[1].n} {c[2].n} {cf[0].n} {cf[1].n}");
    println("end");
}
"#;
    let want = "filled 1001 1\nresize 1002 2\ndrop 4 u\ndrop 0 z\ndrop 0 z\nfill 1003 3\ndrop 5 v\n\
copyless 1001 1001 1 1007 7\nend\ndrop 1003 cc\ndrop 3 c\ndrop 1002 bc\ndrop 2 b\ndrop 1001 ac\ndrop 1 a\n";
    for (cmd, code, stdout, stderr) in lanes("clone", src) {
        assert_eq!(code, Some(0), "{cmd}: {stderr}");
        assert_eq!(stdout, want, "{cmd}");
    }
}

#[test]
fn coll_windows_and_chunks_of_a_non_positive_size_say_so() {
    for (m, n) in [("windows", "0"), ("chunks", "-1")] {
        let src = format!(
            "fn main() {{\n    let v: Vec[i64] = [1, 2, 3];\n    println(v.{m}({n}).len());\n}}\n"
        );
        for (cmd, code, _, stderr) in lanes(m, &src) {
            assert_eq!(code, Some(101), "{cmd}: {stderr}");
            assert!(
                stderr.contains(&format!("runtime error: {m}: size must be positive")),
                "{cmd}: {stderr}"
            );
        }
    }
}

#[test]
fn coll_float_key_diagnostic_names_the_total_order_float() {
    let file = program(
        "fkey",
        "fn main() {\n    let m: Map[f64, i64] = Map.new();\n    let s: Set[f32] = Set.new();\n}\n",
    );
    let o = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
    assert!(!o.status.success(), "{all}");
    assert!(all.contains("Use `F64`, the total-order float"), "{all}");
    assert!(all.contains("Use `F32`, the total-order float"), "{all}");
}
