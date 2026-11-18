//! B-2026-11-18-50 (kata 1619): a generic struct literal or enum variant put
//! where its type is already fixed kept a borrowed number as `ref i64`.
//! With `x: ref i32` from `for x in arr.iter()`, `Reverse { value: x }` into
//! a `PriorityQueue[Reverse[i32]]` was refused ("field 'value' has type
//! 'ref i32', but the type parameter was already bound to 'i32' by an
//! earlier field"), and so were `Pair { a: x }` and `Box2.B(x)` pushed into
//! a `Vec[Pair[i64]]` / `Vec[Box2[i64]]`, while a non-generic struct, a
//! tuple and `Some(x)` copied the number in. With no fixed type the literal
//! still views its value, as `Some(x)` alone does.

use std::process::{Command, Stdio};

const SRC: &str = "import std.cmp.Reverse;

struct Pair[T] {
    a: T,
}

enum Box2[T] {
    B(T),
    N { v: T },
}

fn main() {
    let v: Vec[i64] = [3, 1, 2];
    let mut largest: PriorityQueue[Reverse[i64]] = PriorityQueue.max_first();
    let mut rs: Vec[Reverse[i64]] = Vec.new();
    let mut ps: Vec[Pair[i64]] = Vec.new();
    let mut bs: Vec[Box2[i64]] = Vec.new();
    for x in v.iter() {
        largest.push(Reverse { value: x });
        rs.push(Reverse { value: x });
        ps.push(Pair { a: x });
        let q: Pair[i64] = Pair { a: x * 10 };
        ps.push(q);
        bs.push(Box2.B(x));
        bs.push(Box2.N { v: x });
        let n: Box2[i64] = Box2.N { v: x };
        bs.push(n);
        let view = Pair { a: x };
        print(f\"{view.a} \");
    }
    println(\"\");
    let mut total = 0;
    for b in bs.iter() {
        match b {
            Box2.B(x) => total += x,
            Box2.N { v } => total += v * 100,
        }
    }
    let smallest = largest.pop().unwrap().value;
    let pairs: i64 = ps.iter().map(|p| p.a).sum();
    println(f\"{smallest} {rs.len()} {pairs} {total}\");
}
";

const OUT: &str = "3 1 2 \n1 3 66 1206\n";

fn run(lane: &str) -> (String, String) {
    let dir =
        std::env::temp_dir().join(format!("karac-generic-copy-{lane}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("g.kara"), SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([lane, "g.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn generic_literal_copies_borrowed_number_in_the_interpreter() {
    let (out, err) = run("__mir-run");
    assert_eq!(out, OUT, "{err}");
}

#[test]
fn generic_literal_copies_borrowed_number_checks_clean() {
    let (_, err) = run("check");
    assert!(!err.contains("error"), "{err}");
}
