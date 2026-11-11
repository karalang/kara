//! Kata 1122 sorts by a key that is a match of two tuples:
//! `match place.get(x) { Some(i) => (0, i), None => (1, x as i64) }`,
//! where `i` is a `ref i64`. A `ref` to a `Copy` value beside the value
//! joins at the value (core semantics §5.10), and so does each element of
//! two tuples: the arms are an `(i64, i64)`. A tuple literal is built at
//! the joined types; a tuple made elsewhere is read element by element.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kata_1122_tuple_ref_join_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Whether the program checks, and what it prints under the MIR
/// interpreter (or what it reports).
fn run(name: &str, src: &str) -> (bool, String) {
    let dir = scratch(name);
    std::fs::write(dir.join("m.kara"), src).unwrap();
    let (checked, check_out, err) = karac(&dir, &["check", "m.kara"]);
    let (_, out, run_err) = karac(&dir, &["__mir-run", "m.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    if checked {
        (true, format!("{out}{run_err}"))
    } else {
        (false, format!("{check_out}{err}"))
    }
}

#[test]
fn tuple_arms_of_a_ref_and_a_value_join_at_the_value() {
    let src = "fn key(place: Map[i32, i64], x: i32) -> (i64, i64) {
    match place.get(x) {
        Some(i) => (0, i),
        None => (1, x as i64),
    }
}

fn main() {
    let mut m: Map[i32, i64] = Map.new();
    m[3] = 7;
    let x: i64 = 5;
    let b = match m.get(4) {
        Some(i) => (0, i),
        None => (1, x),
    };
    let c = match m.get(3) {
        None => (1, x),
        Some(i) => (0, i),
    };
    let d = if m.contains_key(3) { (2, m.get(3).unwrap()) } else { (3, x) };
    m[3] = 8;
    let mut v = [5, 3, 9, 1];
    let mut place: Map[i32, i64] = Map.new();
    place[9] = 0;
    place[5] = 1;
    v.sort_by_key(|y| match place.get(y) {
        Some(i) => (0, i),
        None => (1, y as i64),
    });
    println(f\"{b.0} {b.1} {c.0} {c.1} {d.0} {d.1} {v} {key(place, 5).1}\");
}
";
    let (checked, out) = run("literal", src);
    assert!(checked, "{out}");
    assert_eq!(out, "1 5 0 7 2 7 [9, 5, 1, 3] 1\n");
}

#[test]
fn a_tuple_arm_made_elsewhere_reads_its_referenced_element() {
    let src = "fn main() {
    let mut m: Map[i32, i64] = Map.new();
    m[3] = 7;
    let x: i64 = 5;
    let b = match m.get(3) {
        Some(i) => {
            let t = (0, i);
            t
        }
        None => (1, x),
    };
    m[3] = 8;
    println(f\"{b.0} {b.1}\");
}
";
    let (checked, out) = run("made", src);
    assert!(checked, "{out}");
    assert_eq!(out, "0 7\n");
}

#[test]
fn a_tuple_of_a_ref_string_beside_a_string_is_still_refused() {
    let src = "fn main() {
    let mut m: Map[i32, String] = Map.new();
    m[3] = \"a\";
    let s = \"b\".to_string();
    let b = match m.get(3) {
        Some(i) => (0, i),
        None => (1, s),
    };
    println(f\"{b.0} {b.1}\");
}
";
    let (checked, out) = run("string", src);
    assert!(!checked, "{out}");
    assert!(out.contains("match arms have incompatible types"), "{out}");
}
