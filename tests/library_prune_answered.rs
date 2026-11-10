//! The library prune keeps a library trait's default named in
//! `ANSWERED_WHEN_SPELLED` (`Iterator.collect`, `Iterator.min`, ...) only
//! when a program or a kept library body spells the name
//! (B-2026-11-10-38). These programs reach such a method another way, and
//! must keep running.

use std::process::Command;

fn mir_run(tag: &str, src: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("karac-prune-answered-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", "main.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

/// `Vec.from_iter(it)` is `it.collect()` (the typechecker types it as one
/// off the MIR pipeline), but the program never spells `collect`: on the
/// MIR pipeline it is the library's own `FromIterator` impl.
#[test]
fn vec_from_iter_runs_without_collect_spelled() {
    let src = "fn main() {\n    let v = Vec.from_iter([1, 2, 3].into_iter().map(|x| x * 2));\n    println(f\"{v.len()} {v[2]}\");\n}\n";
    assert_eq!(mir_run("from-iter", src), "3 6\n");
}

/// The inherent methods of an answered name stay kept: `Vec.last`, `i64.max`
/// and `Vec.is_empty` beside an iterator's `count`, all spelled once.
#[test]
fn answered_names_reach_inherent_and_iterator_methods_alike() {
    let src = "fn main() {\n    let v = [4, 9, 2];\n    let n = v.iter().count();\n    let m = v.iter().max().unwrap();\n    println(f\"{n} {m} {v.last().unwrap()} {3.max(7)} {v.is_empty()}\");\n}\n";
    assert_eq!(mir_run("inherent", src), "3 9 2 7 false\n");
}
