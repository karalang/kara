//! Kata 2095's nodes arm walks a linked list with its own `impl Iterator`
//! whose `Item` is the list's node, a struct with no ordering. `karac
//! check` took it, but `karac run` refused it: `Iterator`'s default `min`
//! and `max` are copied into every impl, and their bodies, written against
//! `where Self.Item: Ord`, were type-checked for an `Item` that has no
//! order. Check keeps only the program's own errors, so only the run saw
//! them. Such a copy is now left unchecked, and a call of `max` on that
//! iterator is still refused at the call.

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

const WALK: &str = "shared struct Node {
    val: i64,
    mut next: Option[Node],
}

struct Nodes {
    cur: Option[Node],
}

impl Iterator for Nodes {
    type Item = Node;

    fn next(mut ref self) -> Option[Node] {
        let node = self.cur?;
        self.cur = node.next;
        Some(node)
    }
}

struct Tables {
    left: i64,
}

impl Iterator for Tables {
    type Item = Map[i64, i64];

    fn next(mut ref self) -> Option[Map[i64, i64]] {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        let mut m: Map[i64, i64] = Map.new();
        m.insert(self.left, 1);
        Some(m)
    }
}

struct Count {
    i: i64,
}

impl Iterator for Count {
    type Item = i64;

    fn next(mut ref self) -> Option[i64] {
        if self.i == 4 {
            return None;
        }
        self.i += 1;
        Some(self.i)
    }
}
";

#[test]
fn kata_2095_iterator_of_unordered_items_runs() {
    let src = format!(
        "{WALK}
fn main() {{
    let list = Some(Node {{ val: 1, next: Some(Node {{ val: 2, next: Some(Node {{ val: 3, next: None }}) }}) }});
    let nodes: Vec[Node] = Nodes {{ cur: list }}.collect();
    nodes[0].next = Some(nodes[2]);
    let left: Vec[i64] = Nodes {{ cur: Some(nodes[0]) }}.map(|n| n.val).collect();
    let sizes: i64 = Tables {{ left: 3 }}.map(|m| m.len()).sum();
    println(f\"{{nodes.len()}} {{left}} {{sizes}} {{Count {{ i: 0 }}.max().unwrap()}}\");
}}
"
    );
    let file = write("k2095-unordered-items", &src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let out = karac::mir::lower::run_source(&src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "3 [1, 3] 3 4\n");
}

/// `max` on the iterator of nodes is still refused, at the call.
#[test]
fn kata_2095_max_of_unordered_items_refused() {
    let src = format!(
        "{WALK}
fn main() {{
    let list = Some(Node {{ val: 1, next: None }});
    let m = Nodes {{ cur: list }}.max();
    println(f\"{{m.is_some()}}\");
}}
"
    );
    let file = write("k2095-unordered-max", &src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(!check.status.success(), "max of nodes should be refused");
    assert!(
        String::from_utf8_lossy(&check.stderr).contains("Ord"),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
}
