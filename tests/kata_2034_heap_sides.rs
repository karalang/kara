//! Kata 2034 (Stock Price Fluctuation): LeetCode's Rust template keeps a
//! plain `BinaryHeap<(i32, i32)>` for the maximum beside a
//! `BinaryHeap<Reverse<(i32, i32)>>` for the minimum. Kāra's
//! `PriorityQueue` pops the smallest first, so `karac fix` writes
//! `PriorityQueue.new()` for a heap whose items are wrapped in `Reverse` and
//! `PriorityQueue.max_first()` for one of bare items. It used to decide for
//! the whole program: one `Reverse` anywhere made every heap smallest-first,
//! and the maximum heap silently became a second minimum heap, so the fixed
//! program checked clean and printed wrong answers. The side is now told
//! from the heap each constructor builds (a struct field, a `let`, an
//! assignment); when one constructor cannot be told, no heap and no
//! `Reverse` in the program gets a fix.

use std::path::Path;
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

/// Every check's diagnostics, the text `karac fix` leaves after as many
/// rounds as change it, whether that text checks clean, and what it prints
/// under the MIR interpreter.
fn mend(tag: &str, src: &str) -> (String, String, bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2034-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let mut first = String::new();
    for _ in 0..8 {
        first += &karac(&dir, &["check", "cell.kara"]).2;
        let before = std::fs::read_to_string(&file).unwrap();
        karac(&dir, &["fix", "cell.kara"]);
        if std::fs::read_to_string(&file).unwrap() == before {
            break;
        }
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (clean, _, _) = karac(&dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (first, text, clean, out)
}

/// The kata's shape: two struct fields, one plain and one `Reverse`, built
/// in a struct literal.
#[test]
fn kata_2034_plain_heap_field_beside_a_reverse_one_stays_largest_first() {
    let src = "use std::cmp::Reverse;\nuse std::collections::BinaryHeap;\n\nstruct Prices {\n    high: BinaryHeap<i64>,\n    low: BinaryHeap<Reverse<i64>>,\n}\n\nimpl Prices {\n    fn new() -> Self {\n        Prices { high: BinaryHeap::new(), low: BinaryHeap::new() }\n    }\n\n    fn add(&mut self, p: i64) {\n        self.high.push(p);\n        self.low.push(Reverse(p));\n    }\n}\n\nfn main() {\n    let mut s = Prices::new();\n    for p in [5, 9, 1, 7] {\n        s.add(p);\n    }\n    let hi = *s.high.peek().unwrap();\n    let Reverse(lo) = *s.low.peek().unwrap();\n    println(f\"{hi} {lo}\");\n}";
    let (_, text, clean, out) = mend("fields", src);
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("high: PriorityQueue.max_first()")
            && text.contains("low: PriorityQueue.new()"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "9 1", "{text}");
}

/// Two `let`s, told apart by what is pushed onto each.
#[test]
fn kata_2034_plain_heap_let_beside_a_reverse_one_stays_largest_first() {
    let src = "use std::cmp::Reverse;\nuse std::collections::BinaryHeap;\n\nfn main() {\n    let mut most = BinaryHeap::new();\n    let mut least = BinaryHeap::new();\n    for p in [5, 9, 1, 7] {\n        most.push(p);\n        least.push(Reverse(p));\n    }\n    let a = most.pop().unwrap();\n    let Reverse(b) = least.pop().unwrap();\n    println(f\"{a} {b}\");\n}";
    let (_, text, clean, out) = mend("lets", src);
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("let mut most = PriorityQueue.max_first()")
            && text.contains("let mut least = PriorityQueue.new()"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "9 1", "{text}");
}

/// A heap built where no binding names it, in a program with both kinds:
/// the check says what each kind becomes and offers no heap fix at all.
#[test]
fn kata_2034_unnamed_heap_beside_both_kinds_gets_no_fix() {
    let src = "use std::cmp::Reverse;\nuse std::collections::BinaryHeap;\n\nfn top(h: BinaryHeap<i32>) -> i32 {\n    *h.peek().unwrap()\n}\n\nfn main() {\n    let mut most = BinaryHeap::new();\n    let mut least = BinaryHeap::new();\n    most.push(3);\n    least.push(Reverse(3));\n    println(top(BinaryHeap::new()));\n}";
    let (first, text, _, _) = mend("unnamed", src);
    assert!(
        first.contains("which kind this one is cannot be told"),
        "{first}"
    );
    // Nothing heap-shaped is fixed while one heap is unsure: a later round
    // would no longer see the `Reverse` that tells the others apart.
    assert!(
        text.contains("top(BinaryHeap.new())")
            && text.contains("let mut most = BinaryHeap.new()")
            && text.contains("least.push(Reverse(3))"),
        "{text}"
    );
}
