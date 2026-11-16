//! Kata 1171's Rust arms walk a plain recursive list,
//! `struct ListNode { val: i32, next: Option[ListNode] }`, and three gaps
//! showed up on the way:
//!
//! - `let mut cur = head; while let Some(node) = cur { cur = node.next; }`
//!   over a borrowed `head` was refused by the MIR's move check, with no
//!   fix. `cur` is a `ref`, so the fix is `cur = ref node.next` (§5.1).
//! - `Some(n) => n.next` over a borrowed list offered `#[derive(Copy,
//!   Clone)]` on the list's struct, which holds itself, and
//! - `karac check` on that derive overflowed its stack. A type that holds
//!   itself is boxed, so it is never `Copy`: the derive is refused, and the
//!   fix offers `Clone` instead.
//! - `iter().skip(1).rev()` and `take(n).rev()` had no `rev`.
//!
//! Rows B-2026-11-16-177 (the cursor), B-2026-11-16-178 (the derive and
//! the crash) and B-2026-11-16-176 (`rev`).

use std::path::Path;
use std::process::{Command, Stdio};

/// Runs `karac` on `cell.kara` in `dir`: its verdict, its output, and the
/// file as it stands after.
fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let fixed = std::fs::read_to_string(dir.join("cell.kara")).unwrap_or_default();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        fixed,
    )
}

fn cell(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-kata-1171-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    dir
}

/// One `karac fix` round over `src`, then a second that must find nothing,
/// then `__mir-run`: the source after the one round, and what it prints.
fn one_round(tag: &str, src: &str) -> (String, String) {
    let dir = cell(tag, src);
    let (_, out, fixed) = karac(&dir, &["fix"]);
    assert!(out.contains("applied"), "{tag}: {out}");
    let (_, out, again) = karac(&dir, &["fix"]);
    assert_eq!(
        again, fixed,
        "{tag}: a second round changed the source: {out}"
    );
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(ok, "{tag}: {out}\n{fixed}");
    let (ok, out, _) = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{tag}: {out}\n{fixed}");
    (fixed, out)
}

#[test]
fn kata_1171_cursor_over_a_borrowed_list_takes_ref_in_one_round() {
    let (fixed, out) = one_round(
        "cursor",
        r#"struct Node {
    val: i64,
    next: Option[Node],
}

fn to_vec(head: Option[Node]) -> Vec[i64] {
    let mut out = Vec.new();
    let mut cur = head;
    while let Some(node) = cur {
        out.push(node.val);
        cur = node.next;
    }
    out
}

fn main() {
    let l = Some(Node { val: 1, next: Some(Node { val: 2, next: None }) });
    println(f"{to_vec(l)}");
}
"#,
    );
    assert!(fixed.contains("cur = ref node.next;"), "{fixed}");
    assert_eq!(out.trim(), "[1, 2]");
}

#[test]
fn kata_1171_cursor_message_names_the_ref_fix() {
    let dir = cell(
        "cursor-msg",
        r#"struct Node {
    val: i64,
    next: Option[Node],
}

fn last(head: Option[Node]) -> i64 {
    let mut cur = head;
    let mut v = 0;
    while let Some(node) = cur {
        v = node.val;
        cur = node.next;
    }
    v
}

fn main() {
    println(f"{last(Some(Node { val: 4, next: None }))}");
}
"#,
    );
    let (ok, out, _) = karac(&dir, &["check"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok, "{out}");
    assert!(out.contains("`cur` is a `ref Option[Node]`"), "{out}");
    assert!(out.contains("cur = ref <place>"), "{out}");
    assert!(!out.contains("error[mir]"), "{out}");
}

#[test]
fn kata_1171_recursive_struct_is_offered_clone_not_copy() {
    let (fixed, out) = one_round(
        "derive",
        r#"struct Node {
    val: i64,
    next: Option[Node],
}

fn second(head: Option[Node]) -> Option[Node] {
    match head {
        Some(n) => n.next,
        None => None,
    }
}

fn main() {
    let l = Some(Node { val: 1, next: Some(Node { val: 2, next: None }) });
    match second(l) {
        Some(n) => println(f"{n.val}"),
        None => println("none"),
    }
}
"#,
    );
    assert!(fixed.contains("#[derive(Clone)]\nstruct Node"), "{fixed}");
    assert!(!fixed.contains("Copy"), "{fixed}");
    assert!(fixed.contains("Some(n) => n.next.clone(),"), "{fixed}");
    assert_eq!(out.trim(), "2");
}

#[test]
fn kata_1171_copy_derive_on_a_type_that_holds_itself_is_refused() {
    for (tag, src) in [
        (
            "struct",
            "#[derive(Copy, Clone)]\nstruct Node {\n    val: i64,\n    next: Option[Node],\n}\n\nfn main() {\n    let n = Node { val: 3, next: None };\n    println(f\"{n.val}\");\n}\n",
        ),
        (
            "pair",
            "#[derive(Copy, Clone)]\nstruct A {\n    b: Option[B],\n}\n\n#[derive(Copy, Clone)]\nstruct B {\n    a: (i64, Option[A]),\n}\n\nfn main() {\n    let a = A { b: None };\n    println(f\"{a.b.is_none()}\");\n}\n",
        ),
        (
            "enum",
            "#[derive(Copy, Clone)]\nenum Tree {\n    Leaf(i64),\n    Pair(Option[Tree], i64),\n}\n\nfn main() {\n    let t = Tree.Leaf(1);\n    match t {\n        Tree.Leaf(v) => println(f\"{v}\"),\n        _ => println(\"pair\"),\n    }\n}\n",
        ),
    ] {
        let dir = cell(&format!("copy-{tag}"), src);
        let (ok, out, _) = karac(&dir, &["check"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!ok, "{tag}: {out}");
        assert!(out.contains("holds itself"), "{tag}: {out}");
        assert!(!out.contains("overflowed"), "{tag}: {out}");
    }
}

#[test]
fn kata_1171_copy_derive_on_a_struct_of_copy_structs_still_passes() {
    let dir = cell(
        "copy-ok",
        "#[derive(Copy, Clone)]\nstruct P {\n    x: i64,\n}\n\n#[derive(Copy, Clone)]\nstruct Seg {\n    a: P,\n    b: Option[P],\n}\n\nfn main() {\n    let s = Seg { a: P { x: 1 }, b: Some(P { x: 2 }) };\n    let t = s;\n    println(f\"{s.a.x} {t.a.x}\");\n}\n",
    );
    let (ok, out, _) = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "1 1");
}

#[test]
fn kata_1171_skip_and_take_run_from_the_back() {
    let dir = cell(
        "rev",
        r#"fn main() {
    let v = [1, 2, 3, 4, 5];
    let a: Vec[i64] = v.iter().skip(1).rev().cloned().collect();
    let b: Vec[i64] = v.iter().take(3).rev().cloned().collect();
    let c: Vec[i64] = v.iter().skip(7).rev().cloned().collect();
    let d: Vec[i64] = v.iter().take(9).rev().cloned().collect();
    let e: Vec[i64] = v.iter().skip(1).take(3).rev().cloned().collect();
    let f: Vec[(i64, i64)] = v.iter().skip(2).enumerate().rev().map(|(i, x)| (i, x)).collect();
    let mut it = v.iter().skip(1);
    let front = it.next();
    let back = it.next_back();
    println(f"{a} {b} {c} {d} {e} {f} {front} {back} {v.iter().skip(2).len()} {v.iter().take(2).len()}");
}
"#,
    );
    let (ok, out, _) = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{out}");
    assert_eq!(
        out.trim(),
        "[5, 4, 3, 2] [3, 2, 1] [] [5, 4, 3, 2, 1] [4, 3, 2] [(2, 5), (1, 4), (0, 3)] Some(2) Some(5) 3 2"
    );
}
