//! A `par for` whose body's value is a tuple, written in place or returned
//! by a call, was refused with the builder's internal "an expression with no
//! recorded type is not lowered yet" (B-2026-11-13-4). A tuple-slot read
//! added for B-2026-11-11-64 asked the task body's block for its type and
//! fell back to the slot's when there was none, but the failed lookup had
//! already recorded its error. The fallback is quiet now.

use std::process::{Command, Stdio};

#[test]
fn par_for_with_tuple_bodies_collects_them() {
    let src = "fn pair(i: i64) -> (i64, i64) { (i, i * 2) }\n\
               fn main() {\n    \
               let r = par for i in 0..4 { (i, i * 2) };\n    println(r);\n    \
               let q = par for i in 0..3 { pair(i) };\n    println(q);\n    \
               let s = par for i in 0..2 { (i, \"x\".to_string()) };\n    println(s);\n    \
               let t: Vec[(i64, i64)] = par for i in 0..2 { (i, i) };\n    println(t);\n}\n";
    let path =
        std::env::temp_dir().join(format!("karac-par-for-tuple-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(lane)
            .arg(&path)
            .stdin(Stdio::null())
            .output()
            .expect("karac");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "[(0, 0), (1, 2), (2, 4), (3, 6)]\n[(0, 0), (1, 2), (2, 4)]\n[(0, x), (1, x)]\n[(0, 0), (1, 1)]\n",
            "{lane:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_file(&path);
}
