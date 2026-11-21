//! Kata 2343's naive arm was first written `a.byte_at(p)`, after
//! `char_at`; the refusal named neither the byte read `a.bytes()[p]` nor
//! what `char_at` is, and offered no fix (B-2026-11-21-17). Kata 2349's
//! heap arm stores `self.heaps[number] = PriorityQueue.new()`: a generic
//! constructor assigned, not let-bound, to a typed place was refused
//! "cannot infer type parameter 'T'" though the place fixes it
//! (B-2026-11-21-18).

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn kata_2343_byte_at_is_named_and_fixed_to_bytes_index() {
    let src = "fn less(a: Str, b: Str) -> bool {
    let mut p = 0;
    while p < a.len() {
        if a.byte_at(p) != b.byte_at(p) {
            return a.byte_at(p) < b.byte_at(p);
        }
        p += 1;
    }
    false
}

fn main() {
    println(f\"{less(\"102\", \"110\")} {less(\"9\", \"1\")}\");
}
";
    let file = write("k2343-byte-at", src);
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        stderr.contains("no method 'byte_at' on type 'Str': the byte at an index is `.bytes()[i]`"),
        "{stderr}"
    );
    let fix = karac(&["fix", &file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        fixed.contains("if a.bytes()[p] != b.bytes()[p] {")
            && fixed.contains("return a.bytes()[p] < b.bytes()[p];"),
        "{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "true false\n");
}

#[test]
fn kata_2349_generic_new_assigned_to_a_typed_place() {
    let src = "struct Bag[T] {
    xs: Vec[T],
}

impl[T] Bag[T] {
    fn new() -> Bag[T] {
        Bag { xs: Vec.new() }
    }
}

fn main() {
    let mut b: Bag[i64] = Bag.new();
    b.xs.push(4);
    b = Bag.new();
    let mut m: Map[i64, Bag[i64]] = Map.new();
    m[1] = Bag.new();
    let mut heaps: Map[i64, PriorityQueue[i64]] = Map.new();
    heaps[7] = PriorityQueue.new();
    heaps[7].push(3);
    heaps[7].push(9);
    let mut q: PriorityQueue[i64] = PriorityQueue.new();
    q = PriorityQueue.new();
    q.push(2);
    println(f\"{b.xs.len()} {m.len()} {heaps[7].pop().unwrap()} {q.len()}\");
}
";
    let file = write("k2349-assign-new", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "0 1 3 1\n");
}

#[test]
fn kata_2349_a_constructor_of_the_wrong_type_is_still_refused() {
    let src = "struct Bag[T] {
    xs: Vec[T],
}

impl[T] Bag[T] {
    fn new() -> Bag[T] {
        Bag { xs: Vec.new() }
    }
}

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v = Bag.new();
    println(f\"{v.len()}\");
}
";
    let file = write("k2349-assign-wrong", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(!check.status.success());
}
