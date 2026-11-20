//! Rust's cursor over a list, `let mut cur = Some(head); while let
//! Some(node) = cur { cur = node.next; }`, over a bare parameter: the
//! parameter borrows (core-semantics.md §4.1), so `cur` holds borrows, and
//! both the move out of `node` and the store into `cur` are refused. Each
//! refusal names the parameter and takes its `own` as the fix, where
//! `karac fix` used to derive `Clone` and copy the rest of the list per
//! step (kata 1265's solution arm, B-2026-11-20-85).

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

/// The first check's diagnostics, the text `karac fix` leaves, whether that
/// text checks clean, and what it prints under the MIR interpreter.
fn mend(dir: &Path, src: &str) -> (String, String, bool, String) {
    std::fs::create_dir_all(dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let first = karac(dir, &["check", "cell.kara"]).2;
    for _ in 0..3 {
        let before = std::fs::read_to_string(&file).unwrap();
        karac(dir, &["fix", "cell.kara"]);
        if std::fs::read_to_string(&file).unwrap() == before {
            break;
        }
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (clean, _, _) = karac(dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(dir);
    (first, text, clean, out)
}

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("karac-cursor-param-{name}-{}", std::process::id()))
}

#[test]
fn a_list_cursor_over_a_bare_parameter_takes_the_parameters_own() {
    let src = "struct ListNode {\n    val: i64,\n    next: Option[ListNode],\n}\n\nfn total(head: ListNode) -> i64 {\n    let mut sum = 0;\n    let mut cur = Some(head);\n    while let Some(node) = cur {\n        sum += node.val;\n        cur = node.next;\n    }\n    sum\n}\n\nfn main() {\n    let list = ListNode { val: 1, next: Some(ListNode { val: 2, next: None }) };\n    println(total(list));\n}";
    let (first, text, clean, out) = mend(&scratch("struct"), src);
    assert!(
        first.contains("`cur` is bound out of parameter `head`"),
        "{first}"
    );
    assert!(
        first.contains("`node` is bound out of parameter `head`"),
        "{first}"
    );
    assert!(text.contains("fn total(head: own ListNode)"), "{text}");
    assert!(!text.contains("derive(Clone)"), "{text}");
    assert!(!text.contains(".clone()"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "3\n");
}

#[test]
fn a_dyn_list_cursor_over_a_bare_parameter_takes_the_parameters_own() {
    let src = "trait Node {\n    fn value(self) -> i64;\n    fn get_next(self) -> Option[dyn Node];\n}\n\n#[derive(Clone)]\nstruct Cell {\n    v: i64,\n    next: Option[Cell],\n}\n\nimpl Node for Cell {\n    fn value(self) -> i64 {\n        self.v\n    }\n\n    fn get_next(self) -> Option[dyn Node] {\n        match self.next {\n            Some(n) => {\n                let d: dyn Node = n.clone();\n                Some(d)\n            }\n            None => None,\n        }\n    }\n}\n\nfn total(head: dyn Node) -> i64 {\n    let mut sum = 0;\n    let mut cur = Some(head);\n    while let Some(node) = cur {\n        sum += node.value();\n        cur = node.get_next();\n    }\n    sum\n}\n\nfn main() {\n    let list = Cell { v: 1, next: Some(Cell { v: 2, next: None }) };\n    println(total(list));\n}";
    let (first, text, clean, out) = mend(&scratch("dyn"), src);
    assert!(
        first.contains("Take `head` by value with `head: own dyn Node`"),
        "{first}"
    );
    assert!(text.contains("fn total(head: own dyn Node)"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "3\n");
}
