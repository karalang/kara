//! Rust's linked-list cursor step, `tail = &mut tail.as_mut().unwrap().next`
//! inside `while tail.is_some()`, borrows through a call result, which
//! core-semantics.md §5.1 refuses. The refusal names §5.3's cursor walk,
//! and when the loop's body is that one step `karac fix` rewrites the loop
//! to it (kata 2046's Rust arm, B-2026-11-20-12).

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
    std::env::temp_dir().join(format!("karac-cursor-walk-{name}-{}", std::process::id()))
}

const LIST: &str = "struct N {\n    v: i64,\n    next: Option[N],\n}\n\nfn show(list: ref Option[N]) {\n    let mut at = ref list;\n    while let Some(n) = at {\n        print(f\"{n.v} \");\n        at = ref n.next;\n    }\n    println(\"\");\n}\n\n";

fn program(walk: &str) -> String {
    format!(
        "{LIST}fn append(list: own Option[N], rest: own Option[N]) -> Option[N] {{\n    let mut list = list;\n    let mut tail = mut ref list;\n{walk}    *tail = rest;\n    list\n}}\n\nfn main() {{\n    let two = Some(N {{ v: 2, next: None }});\n    let list = append(Some(N {{ v: 1, next: None }}), two);\n    let list = append(list, Some(N {{ v: 3, next: None }}));\n    show(list);\n}}"
    )
}

#[test]
fn rusts_cursor_step_loop_rewrites_to_the_cursor_walk() {
    let src = program(
        "    while tail.is_some() {\n        tail = mut ref tail.as_mut().unwrap().next;\n    }\n",
    );
    let (first, text, clean, out) = mend(&scratch("loop"), &src);
    assert!(first.contains("E_REF_OPERAND_UNSUPPORTED"), "{first}");
    assert!(
        first.contains("`while let Some(n) = tail { tail = mut ref n.next; }`"),
        "{first}"
    );
    assert!(
        first.contains("which `karac fix` writes for this loop"),
        "{first}"
    );
    assert!(
        text.contains("    while let Some(n) = tail {\n        tail = mut ref n.next;\n    }\n"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "1 2 3 \n");
}

/// A cursor already named `n` steps through `node`.
#[test]
fn a_cursor_named_n_steps_through_node() {
    let src = program(
        "    let mut n = tail;\n    while n.is_some() {\n        n = mut ref n.as_mut().unwrap().next;\n    }\n    tail = n;\n",
    );
    let (first, text, _, _) = mend(&scratch("named-n"), &src);
    assert!(
        first.contains("`while let Some(node) = n { n = mut ref node.next; }`"),
        "{first}"
    );
    assert!(
        text.contains("    while let Some(node) = n {\n        n = mut ref node.next;\n    }\n"),
        "{text}"
    );
}

/// A step that is not the whole body of such a loop is named, not
/// rewritten.
#[test]
fn a_cursor_step_outside_the_loop_shape_is_named_but_not_rewritten() {
    let src = program(
        "    let mut steps = 0;\n    while tail.is_some() {\n        steps += 1;\n        tail = mut ref tail.as_mut().unwrap().next;\n    }\n    println(steps);\n",
    );
    let (first, text, clean, _) = mend(&scratch("shape"), &src);
    assert!(
        first.contains("`while let Some(n) = tail { tail = mut ref n.next; }`"),
        "{first}"
    );
    assert!(!first.contains("which `karac fix` writes"), "{first}");
    assert!(text.contains("tail.as_mut().unwrap().next"), "{text}");
    assert!(!clean, "{text}");
}
