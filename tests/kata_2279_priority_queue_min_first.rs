//! Kata 2279's heap arm was first written `PriorityQueue.min_first()`,
//! the mirror of the documented `PriorityQueue.max_first()`, and was
//! refused "no associated function 'min_first'" with no word of what it
//! means. `PriorityQueue.new()` is already smallest first
//! (library/collections.md), so the refusal now says so and `karac fix`
//! writes it; `min_first_from(v)` likewise becomes `from(v)` (B-2026-11-18-83).

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
fn kata_2279_min_first_is_named_and_fixed_to_new() {
    let src = "fn main() {
    let mut heap: PriorityQueue[i64] = PriorityQueue.min_first();
    heap.push(3);
    heap.push(1);
    heap.push(2);
    let mut built: PriorityQueue[i64] = PriorityQueue.min_first_from([9, 7, 8].to_vec());
    println(f\"{heap.pop().unwrap()} {built.pop().unwrap()}\");
}
";
    let file = write("k2279-min-first", src);
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        stderr.contains("a queue that pops the smallest first is `PriorityQueue.new()`")
            && stderr.contains("`PriorityQueue.from` is already smallest first"),
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
        fixed.contains("PriorityQueue.new();") && fixed.contains("PriorityQueue.from([9, 7, 8]"),
        "{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "1 7\n");
}

#[test]
fn kata_2279_an_unknown_queue_function_stays_plain() {
    let src = "fn main() {
    let heap: PriorityQueue[i64] = PriorityQueue.smallest();
    println(heap.len());
}
";
    let file = write("k2279-unknown", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        stderr.contains("no associated function 'smallest' on type 'PriorityQueue'")
            && !stderr.contains("already smallest first"),
        "{stderr}"
    );
}
