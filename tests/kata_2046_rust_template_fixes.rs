//! Kata 2046: LeetCode's Rust template for the linked-list sort, carried
//! by `karac fix`, stopped at three refusals with no fix:
//! - `Option[Box[ListNode]]` (Rust's `Option<Box<ListNode>>`) was
//!   "undefined type 'Box'". The type is now fixed to `ListNode`, as
//!   `Box.new(e)` already was to `e`.
//! - `let mut cur = head; .. cur = node.next.take();` with `head` a bare
//!   (borrowed) parameter named `own` only in prose, and the node moved
//!   out of `cur` into `Some(node)` was fixed with a `.clone()` of the
//!   whole remaining list. Both now carry the parameter's `own` fix, the
//!   meaning of Rust's by-value parameter, read through a
//!   `let mut node = node;` rebinding too.
//! - `for v in [10, -10, 5] { ListNode::new(v) }` with an `i32` field:
//!   `v` now names the array's literals, and the fix suffixes the first
//!   (`10i32`), which types the array; never when a literal would not fit.

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

/// `karac fix` twice, then the fixed source, whether it checks clean, and
/// what `karac check` said before the fix.
fn fix(name: &str, src: &str) -> (String, bool, String) {
    let file = write(name, src);
    let before = karac(&["check", &file]);
    let _ = karac(&["fix", &file]);
    let _ = karac(&["fix", &file]);
    let fixed = std::fs::read_to_string(&file).unwrap();
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    (
        fixed,
        check.status.success(),
        String::from_utf8_lossy(&before.stderr).into_owned(),
    )
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn kata_2046_box_type_fixes_to_its_argument() {
    let src = "struct ListNode {
    val: i64,
    next: Option[Box[ListNode]],
}

fn total(list: Option[Box[ListNode]]) -> i64 {
    match list {
        Some(n) => n.val + total(n.next),
        None => 0,
    }
}

fn main() {
    let list = Some(Box.new(ListNode { val: 1, next: Some(ListNode { val: 2, next: None }) }));
    println(f\"{total(list)}\");
}
";
    let (fixed, clean, before) = fix("k2046-box", src);
    assert!(before.contains("so `Box[T]` is `T`"), "{before}");
    assert!(clean, "{fixed}");
    assert!(fixed.contains("    next: Option[ListNode],\n"), "{fixed}");
    assert!(!fixed.contains("Box"), "{fixed}");
    assert_eq!(run(&fixed), "3\n");
}

#[test]
fn kata_2046_local_out_of_bare_param_fixes_to_own() {
    let src = "#[derive(Clone)]
struct ListNode {
    val: i64,
    next: Option[ListNode],
}

fn reverse(head: Option[ListNode]) -> Option[ListNode] {
    let mut out: Option[ListNode] = None;
    let mut cur = head;
    while let Some(node) = cur {
        let mut node = node;
        cur = node.next.take();
        node.next = out;
        out = Some(node);
    }
    out
}

fn main() {
    let list = Some(ListNode { val: 1, next: Some(ListNode { val: 2, next: None }) });
    let mut cur = reverse(list);
    while let Some(node) = cur {
        println(f\"{node.val}\");
        cur = node.next;
    }
}
";
    let (fixed, clean, before) = fix("k2046-own", src);
    assert!(before.contains("take `head` as `own`"), "{before}");
    assert!(before.contains("bound out of parameter `head`"), "{before}");
    assert!(clean, "{fixed}");
    assert!(
        fixed.contains("fn reverse(head: own Option[ListNode])"),
        "{fixed}"
    );
    assert!(!fixed.contains(".clone()"), "no deep copy:\n{fixed}");
    assert_eq!(run(&fixed), "2\n1\n");
}

#[test]
fn kata_2046_for_over_literal_array_fixes_with_a_suffix() {
    let src = "fn half(x: i32) -> i32 {
    x / 2
}

fn main() {
    let mut total = 0;
    for v in [10, -10, 6, 0] {
        total += half(v);
    }
    println(f\"{total}\");
}
";
    let (fixed, clean, _) = fix("k2046-lit", src);
    assert!(clean, "{fixed}");
    assert!(fixed.contains("for v in [10i32, -10, 6, 0] {"), "{fixed}");
    assert_eq!(run(&fixed), "3\n");
}

#[test]
fn kata_2046_for_over_literal_array_out_of_range_has_no_suffix_fix() {
    let src = "fn half(x: i32) -> i32 {
    x / 2
}

fn main() {
    let mut total = 0;
    for v in [10, 5000000000] {
        total += half(v);
    }
    println(f\"{total}\");
}
";
    let (fixed, clean, _) = fix("k2046-lit-big", src);
    assert!(!clean, "{fixed}");
    assert!(fixed.contains("for v in [10, 5000000000] {"), "{fixed}");
}
