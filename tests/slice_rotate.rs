//! B-2026-11-15-35: `rotate_left` and `rotate_right` were `Vec` methods
//! only, so Rust's `s[k..=r].rotate_left(1)` on a part of a `Vec` was
//! refused. They are `Slice` methods now (library/collections.md), the same
//! three `swap` passes, and a `Slice` method declared `mut ref self` has
//! the `mut Slice[T]` receiver its body changes the elements through.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn run_interp(tag: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-slicerot-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let out = karac(&["run", "--interp", f.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

#[test]
fn a_part_of_a_vec_rotates_in_place() {
    let out = run_interp(
        "ints",
        "fn main() {
    let mut s: Vec[i64] = [1, 2, 3, 4, 5, 6].to_vec();
    s[1..=4].rotate_left(1);
    println(f\"{s}\");
    s[0..6].rotate_right(2);
    println(f\"{s}\");
    s[3..3].rotate_left(0);
    println(f\"{s}\");
    let mut a: Array[i64, 4] = [1, 2, 3, 4];
    a.rotate_left(1);
    println(f\"{a}\");
}
",
    );
    assert_eq!(
        out,
        "[1, 3, 4, 5, 2, 6]\n[2, 6, 1, 3, 4, 5]\n[2, 6, 1, 3, 4, 5]\n[2, 3, 4, 1]\n"
    );
}

#[test]
fn a_part_of_a_vec_of_strings_rotates_in_place() {
    let out = run_interp(
        "strings",
        "fn main() {
    let mut s: Vec[String] = [\"a\", \"b\", \"c\", \"d\", \"e\"].iter().map(|x| x.to_string()).collect();
    s[1..4].rotate_left(2);
    println(f\"{s}\");
    s[0..5].rotate_right(1);
    println(f\"{s}\");
}
",
    );
    assert_eq!(out, "[a, d, b, c, e]\n[e, a, d, b, c]\n");
}

#[test]
fn a_slice_rotation_past_its_length_panics() {
    let out = run_interp(
        "oob",
        "fn main() {
    let mut s: Vec[i64] = [1, 2, 3].to_vec();
    s[0..2].rotate_left(3);
    println(f\"{s}\");
}
",
    );
    assert!(out.contains("rotate_left: mid out of bounds"), "{out}");
}

#[test]
fn a_read_only_view_is_not_rotated() {
    let out = run_interp(
        "view",
        "fn main() {
    let v: Vec[i64] = [1, 2, 3].to_vec();
    let mut r = v.as_slice();
    r.rotate_left(1);
    println(f\"{r}\");
}
",
    );
    assert!(
        out.contains("Slice.rotate_left() requires a mutable slice"),
        "{out}"
    );
    assert!(!out.contains("[2, 3, 1]"), "{out}");
}
