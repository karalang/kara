//! B-2026-11-27-57: a struct pattern in a `for` head, `for P { x, y } in ps`,
//! was read as Go's `for cond {`, the `{` of the pattern taken for the
//! body's, and refused with advice to write `while`. design.md § Struct
//! literals in conditions parenthesizes a struct literal in a `for`'s
//! collection, not its pattern, which an `in` follows. Kata 1801's Rust
//! solution met it through `karac fix`: `for Reverse((_, a)) in sell`,
//! rewritten `for Reverse { value: (_, a) } in sell`, was refused as an edit
//! that adds parse errors.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("karac-k1801-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn a_struct_pattern_in_a_for_head_is_a_pattern() {
    let path = write(
        "pattern",
        "import std.cmp.Reverse;
struct P { x: i64, y: i64 }
fn main() {
    let ps: Vec[P] = [P { x: 1, y: 2 }, P { x: 3, y: 4 }];
    for P { x, y } in ps.iter() {
        println(f\"{x} {y}\");
    }
    let mut q: PriorityQueue[Reverse[(i64, i64)]] = PriorityQueue.max_first();
    q.push(Reverse { value: (5, 6) });
    for Reverse { value: (a, b) } in q {
        println(f\"{a} {b}\");
    }
    let mut n = 0;
    for n2 in [1, 2] {
        n += n2;
    }
    println(f\"{n}\");
}
",
    );
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "1 2\n3 4\n5 6\n3\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn go_s_conditional_for_is_still_mended() {
    let path = write(
        "gofor",
        "struct P { x: i64 }
fn main() {
    let mut i = 0;
    for i < 3 {
        i += 1;
    }
    println(f\"{i}\");
}
",
    );
    let check = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    let err = String::from_utf8_lossy(&check.stderr);
    assert!(!check.status.success());
    assert!(err.contains("is Go's conditional loop"), "{err}");
}

#[test]
fn karac_fix_rewrites_a_rust_tuple_struct_pattern_in_a_for_head() {
    let path = write(
        "fix",
        "import std.cmp.Reverse;
fn main() {
    let mut q: PriorityQueue[Reverse[(i64, i64)]] = PriorityQueue.max_first();
    q.push(Reverse { value: (1, 2) });
    q.push(Reverse { value: (3, 4) });
    let mut total = 0;
    for Reverse((_, a)) in q {
        total += a;
    }
    println(f\"{total}\");
}
",
    );
    let file = path.to_str().unwrap();
    let fix = karac(&["fix", file]);
    let text = std::fs::read_to_string(&path).unwrap();
    let check = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(
        text.contains("for Reverse { value: (_, a) } in q {"),
        "{}\n{text}",
        String::from_utf8_lossy(&fix.stderr)
    );
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "6\n");
}
