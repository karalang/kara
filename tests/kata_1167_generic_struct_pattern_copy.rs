//! Kata 1167's Rust arm pops a heap of `Reverse[i32]` with `let Reverse {
//! value: a } = heap.pop().unwrap();` and then reads `a` twice (`ans += a +
//! b; heap.push(Reverse { value: a + b });`). The ownership check gave the
//! binding the field's declared type, the parameter `T`, which no bound
//! makes `Copy`, so the first read moved `a`: "value 'a' moved here, used
//! again here". A `match` arm binding the same field was fine. The binding
//! now takes the struct's type argument (B-2026-11-15-79).

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

fn checks_and_runs(name: &str, src: &str, want: &str) {
    let file = write(name, src);
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
    assert_eq!(out, want);
}

#[test]
fn kata_1167_a_generic_struct_pattern_binds_a_copy_field() {
    checks_and_runs(
        "k1167-wrap",
        "struct Wrap[T] {
    value: T,
}

fn main() {
    let w = Wrap { value: 3 };
    let Wrap { value: a } = w;
    let s = a + 1;
    let Wrap { value } = Wrap { value: 4 };
    let t = value + 1;
    println(f\"{a} {s} {value} {t}\");
}
",
        "3 4 4 5\n",
    );
}

#[test]
fn kata_1167_a_reverse_popped_from_a_heap_reads_twice() {
    checks_and_runs(
        "k1167-reverse",
        "import std.cmp.Reverse;

fn main() {
    let mut heap: PriorityQueue[Reverse[i32]] = PriorityQueue.new();
    heap.push(Reverse { value: 2 });
    heap.push(Reverse { value: 5 });
    let Reverse { value: a } = heap.pop().unwrap();
    let Reverse { value: b } = heap.pop().unwrap();
    let ans = a + b;
    heap.push(Reverse { value: a + b });
    println(f\"{a} {b} {ans} {heap.len()}\");
}
",
        "5 2 7 1\n",
    );
}

#[test]
fn kata_1167_a_generic_struct_pattern_still_moves_an_owned_field() {
    // A `String` argument is not `Copy`: binding it moves, and a second
    // use after a move is still refused.
    let src = "struct Wrap[T] {
    value: T,
}

fn eat(s: own String) -> i64 {
    s.len()
}

fn main() {
    let Wrap { value: a } = Wrap { value: \"ab\".to_string() };
    let n = eat(a);
    println(f\"{a} {n}\");
}
";
    let file = write("k1167-owned", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(stderr.contains("moved"), "{stderr}");
}
