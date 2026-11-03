//! The interpreter's decoded steps for an iterator adaptor's bodies
//! (B-2026-11-03-9): a field reached through a reference, an enum built,
//! read and copied, a reference argument, a call through an erased closure
//! and `Vec.get`. Each falls back to the general step off its happy path,
//! so the answers are those of the general steps; this program reaches
//! each of them, and its expected output is worked out by hand.

use std::process::{Command, Stdio};

const SRC: &str = r#"struct P { x: i64, y: i64 }
enum Shape { Dot(i64), Pair(i64, i64), Empty }
fn main() {
    let v: Vec[i64] = [5, 1, 9, 3, 7];
    let lo = 3;
    println(v.iter().filter(|x| *x >= lo).count());
    println(v.iter().map(|x| *x * 2).max().unwrap());
    println(v.iter().map(|x| *x - 10).min().unwrap());
    let s: i64 = v.iter().filter(|x| *x % 3 == 0).map(|x| *x + lo).sum();
    println(s);
    match v.get(9) { Some(x) => println(*x), None => println("none") }
    match v.get(2) { Some(x) => println(*x), None => println("none") }
    let mut ps: Vec[P] = Vec.new();
    for i in 0..4 { ps.push(P { x: i, y: i * i }); }
    println(ps.iter().map(|p| p.x + p.y).max().unwrap());
    let names: Vec[String] = ["b".to_string(), "abc".to_string(), "zz".to_string()];
    println(names.iter().filter(|n| n.len() > 1).count());
    let longest = names.iter().map(|n| n.len()).max();
    let again = longest;
    println(f"{longest.unwrap()} {again.unwrap()}");
    let shapes = [Shape.Dot(4), Shape.Pair(1, 2), Shape.Empty];
    let mut t = 0;
    for sh in shapes.iter() {
        t += match sh { Shape.Dot(a) => a, Shape.Pair(a, b) => a * 10 + b, Shape.Empty => 100 };
    }
    println(t);
    let mut it = v.iter();
    let first = it.next();
    let copy = first;
    println(f"{*first.unwrap()} {*copy.unwrap()}");
    let mut hits = 0;
    let found = v.iter().any(|x| { hits += 1; *x == 9 });
    println(f"{found} {hits}");
}
"#;

#[test]
fn decoded_adaptor_steps_give_the_general_answers() {
    let path =
        std::env::temp_dir().join(format!("karac-adaptor-steps-{}.kara", std::process::id()));
    std::fs::write(&path, SRC).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "4\n18\n-9\n18\nnone\n9\n12\n2\n3 3\n116\n5 5\ntrue 3\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
