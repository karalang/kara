//! B-2026-11-08-12 — a destructuring `let` over a value read through a range
//! slice hands each name its own part: a name whose part is deeply owned
//! (`at: i64` in `let (at, _) = v[i..j].iter().enumerate().min_by_key(..)
//! .unwrap()`) copied it out and holds no borrow of `v`, so `v.remove(at)`
//! checks. A name bound to the borrowed part (`x: ref i64`) still holds the
//! borrow to its last use.

use std::process::Command;

fn check(tag: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-slice-destr-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn an_owned_part_of_a_slice_read_holds_no_borrow() {
    for (tag, src) in [
        (
            "min-by-key",
            "fn main() {\n    let mut v: Vec[i64] = [3, 1, 2];\n    let (at, _) = v[0..3].iter().enumerate().min_by_key(|(_, x)| *x).unwrap();\n    v.remove(at);\n    println(f\"{v.len()}\");\n}\n",
        ),
        (
            "next",
            "fn main() {\n    let mut v: Vec[i64] = [3, 1, 2];\n    let (at, _) = v[0..3].iter().enumerate().next().unwrap();\n    v.remove(at);\n    println(f\"{v.len()}\");\n}\n",
        ),
        (
            "borrowed-part-dead",
            "fn main() {\n    let mut v: Vec[String] = [\"a\".to_string(), \"b\".to_string()];\n    let (at, s) = v[0..2].iter().enumerate().next().unwrap();\n    let n = s.len();\n    v.remove(at);\n    println(f\"{n} {v.len()}\");\n}\n",
        ),
    ] {
        let (ok, out) = check(tag, src);
        assert!(ok, "{tag}: should check:\n{out}");
    }
}

#[test]
fn a_borrowed_part_used_after_the_write_still_conflicts() {
    let (ok, out) = check(
        "borrowed-part-live",
        "fn main() {\n    let mut v: Vec[i64] = [3, 1, 2];\n    let (at, x) = v[0..3].iter().enumerate().next().unwrap();\n    v.remove(at);\n    println(f\"{*x}\");\n}\n",
    );
    assert!(
        !ok && out.contains(
            "`v` cannot be borrowed as `mut ref T` because it is also borrowed as `Slice[T]`"
        ),
        "the live `x` keeps `v` borrowed:\n{out}"
    );
}
