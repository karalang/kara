//! B-2026-11-19-140, from kata 1258: a write of a whole new view through
//! a `mut ref` to a `Str` or a `Slice` was lost in compiled code, the
//! interpreter right. A `mut ref` to a view was laid out as the view's two
//! words, like a `ref`, so `*s = t` through a `mut ref Str` parameter,
//! `v[i] = s` into a `Vec[Str]`, and a `Map` value's `get_mut` each wrote
//! into a copy of the header. A `mut ref` to a `str` or a slice is now a
//! pointer to the view. Each program runs on the interpreter, compiled and
//! under ASan.

mod common;

use std::process::{Command, Stdio};

fn run(sub: &str, tag: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-view-writes-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([sub, "cell.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn lanes(tag: &str, src: &str, want: &str) {
    let (ok, out, err) = run("__mir-run", tag, src);
    assert!(ok, "{tag} interpreted: {err}");
    assert_eq!(out, want, "{tag} interpreted: {err}");
    if !cfg!(feature = "llvm") {
        return;
    }
    for sub in ["__mir-llvm-run", "__mir-llvm-asan"] {
        let (ok, out, err) = run(sub, tag, src);
        if err.contains("Linker failed") && common::link_or_skip(Err(err.to_string())).is_none() {
            eprintln!("skipping {sub}: no runtime archive");
            continue;
        }
        assert!(ok, "{tag} {sub}: {err}");
        assert_eq!(out, want, "{tag} {sub}: {err}");
    }
}

#[test]
fn a_mut_ref_str_parameter_writes_its_argument() {
    lanes(
        "mut-ref-str",
        r#"fn put(a: mut ref Str, b: Str) {
    *a = b;
}

fn shout(a: mut ref Str) -> i64 {
    let n = a.len();
    if a.starts_with("w") and *a == "world" {
        println(f"saw {a} ({n})");
    }
    n
}

fn first(s: ref Str) -> Str {
    s[0..1]
}

fn main() {
    let mut x: Str = "hello";
    put(mut x, "world");
    let n = shout(mut x);
    println(f"{x} {first(x)} {n}");
    let words = ["red".to_string(), "green".to_string()];
    let mut pick: Str = "none";
    for w in words.iter() {
        if w.len() > 3 {
            put(mut pick, w.as_str());
        }
    }
    println(pick);
}
"#,
        "saw world (5)\nworld w 5\ngreen\n",
    );
}

#[test]
fn str_elements_of_a_vec_deque_and_map_take_writes() {
    lanes(
        "str-elements",
        r#"fn put(a: mut ref Str, b: Str) {
    *a = b;
}

fn main() {
    let mut w: Vec[Str] = ["k", "l", "m"];
    w[1] = "q";
    put(mut w[0], "y");
    let r = mut ref w[2];
    *r = "s";
    let names = ["ab".to_string(), "cd".to_string()];
    for i in 0..2 {
        w[i] = names[i].as_str();
    }
    println(f"{w}");
    let mut d: VecDeque[Str] = VecDeque.new();
    d.push_back("p");
    d.push_back("r");
    d[1] = "t";
    println(f"{d[0]} {d[1]}");
    let mut m: Map[i64, Str] = Map.new();
    m.insert(1, "one");
    if let Some(v) = m.get_mut(1) {
        *v = "uno";
    }
    *m.entry(2).or_insert("two") = "dos";
    println(f"{m[1]} {m[2]}");
}
"#,
        "[ab, cd, s]\np t\nuno dos\n",
    );
}

#[test]
fn slice_elements_and_mut_ref_slice_parameters_take_writes() {
    lanes(
        "slices",
        r#"fn take(s: mut ref Slice[i64], t: Slice[i64]) {
    *s = t;
}

fn total(s: ref Slice[i64]) -> i64 {
    s.iter().sum()
}

fn main() {
    let ys: Vec[i64] = [5, 6, 7];
    let mut parts: Vec[Slice[i64]] = [ys[0..1], ys[1..2]];
    parts[0] = ys[0..3];
    let r = mut ref parts[1];
    *r = ys[2..3];
    println(f"{parts[0].len()} {total(parts[1])}");
    take(mut parts[1], ys[0..2]);
    println(f"{parts[1]} {total(parts[1])}");
}
"#,
        "3 7\n[5, 6] 11\n",
    );
}
