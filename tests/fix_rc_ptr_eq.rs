//! `Rc::ptr_eq(&a, &b)`, the identity test LeetCode's Rust tree
//! templates write (kata 1602), had no fix: the `Rc<RefCell<T>>` rewrite
//! left `Rc.ptr_eq(a, b)` behind with no fix and no word of `ref_eq`.
//! `karac fix` now writes it `a.ref_eq(b)`, the identity test every
//! `shared` handle has (design.md § Equality), with a non-trivial first
//! argument parenthesized. B-2026-11-18-5.

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

const TEMPLATE: &str = r#"use std::rc::Rc;
use std::cell::RefCell;

pub struct TreeNode {
    pub val: i32,
    pub left: Option<Rc<RefCell<TreeNode>>>,
    pub right: Option<Rc<RefCell<TreeNode>>>,
}

fn leaf(v: i32) -> Rc<RefCell<TreeNode>> {
    Rc::new(RefCell::new(TreeNode { val: v, left: None, right: None }))
}

fn same(a: &Rc<RefCell<TreeNode>>, b: &Rc<RefCell<TreeNode>>) -> bool {
    Rc::ptr_eq(&a, &b)
}

fn main() {
    let root = leaf(1);
    let kid = leaf(2);
    root.borrow_mut().left = Some(kid.clone());
    let twin = leaf(2);
    let left = root.borrow().left.clone().unwrap();
    println!("{}", same(&left, &kid));
    println!("{}", Rc::ptr_eq(&kid, &twin));
    println!("{}", Rc::ptr_eq(root.borrow().left.as_ref().unwrap(), &kid));
}
"#;

#[test]
fn rc_ptr_eq_is_fixed_to_ref_eq() {
    let dir = std::env::temp_dir().join(format!("karac-rc-ptr-eq-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("tree.kara");
    std::fs::write(&file, TEMPLATE).unwrap();
    for _ in 0..8 {
        let before = std::fs::read_to_string(&file).unwrap();
        karac(&dir, &["fix", "tree.kara"]);
        if std::fs::read_to_string(&file).unwrap() == before {
            break;
        }
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (clean, out, err) = karac(&dir, &["check", "tree.kara"]);
    let (_, run, _) = karac(&dir, &["__mir-run", "tree.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(clean, "{text}\n{out}{err}");
    assert!(!text.contains("ptr_eq"), "{text}");
    assert!(text.contains("a.ref_eq(b)"), "{text}");
    assert!(text.contains("kid.ref_eq(twin)"), "{text}");
    assert_eq!(run, "true\nfalse\ntrue\n", "{text}");
}
