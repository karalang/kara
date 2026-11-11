//! Kata 1123's solution arm, written as Rust, passes the tree as
//! `&Option<Rc<RefCell<TreeNode>>>` and returns `Some(Rc::clone(n))` for a
//! node matched out of it. `karac fix` spelled the `&` as `ref`, so `n` was a
//! `ref TreeNode` and `Some(n)` an `Option[ref TreeNode]`: a type error with
//! no fix, which also kept the `redundant_ref_param` lint from dropping the
//! `ref`. A parameter's `&` before an `Rc<RefCell<T>>` now goes: the type is
//! the shared struct's handle, and a bare parameter already borrows.

use std::path::{Path, PathBuf};
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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kata_1123_rc_param_borrow_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The text `karac fix` leaves once it has nothing left to apply.
fn mend(dir: &Path, src: &str) -> String {
    let file = dir.join("m.kara");
    std::fs::write(&file, src).unwrap();
    for _ in 0..8 {
        karac(dir, &["fix", "m.kara"]);
    }
    std::fs::read_to_string(&file).unwrap()
}

const SOLUTION: &str = "use std::cell::RefCell;
use std::rc::Rc;

struct TreeNode {
    val: i32,
    left: Option<Rc<RefCell<TreeNode>>>,
    right: Option<Rc<RefCell<TreeNode>>>,
}

fn dfs(node: &Option<Rc<RefCell<TreeNode>>>) -> (i32, Option<Rc<RefCell<TreeNode>>>) {
    match node {
        None => (0, None),
        Some(n) => {
            let (ld, la) = dfs(&n.borrow().left);
            let (rd, ra) = dfs(&n.borrow().right);
            if ld == rd {
                (ld + 1, Some(Rc::clone(n)))
            } else if ld > rd {
                (ld + 1, la)
            } else {
                (rd + 1, ra)
            }
        }
    }
}

fn leaf(val: i32) -> Option<Rc<RefCell<TreeNode>>> {
    Some(Rc::new(RefCell::new(TreeNode { val, left: None, right: None })))
}

fn main() {
    let root = leaf(1);
    let two = leaf(2);
    root.clone().unwrap().borrow_mut().left = two.clone();
    root.clone().unwrap().borrow_mut().right = leaf(3);
    two.clone().unwrap().borrow_mut().left = leaf(4);
    let (d, a) = dfs(&root);
    let v = a.unwrap().borrow().val;
    println(f\"{d} {v}\");
}
";

#[test]
fn a_ref_rc_refcell_option_parameter_mends_to_a_bare_parameter() {
    let dir = scratch("solution");
    let text = mend(&dir, SOLUTION);
    assert!(
        text.contains("fn dfs(node: Option[TreeNode]) -> (i32, Option[TreeNode])"),
        "{text}"
    );
    let (ok, _, err) = karac(&dir, &["check", "m.kara"]);
    assert!(ok, "the mended solution checks clean:\n{err}\n{text}");
    let (ok, out, err) = karac(&dir, &["__mir-run", "m.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{err}");
    assert_eq!(out, "3 4\n", "{text}");
}

#[test]
fn a_ref_rc_refcell_parameter_read_through_a_star_keeps_its_ref() {
    let dir = scratch("star");
    let src = "use std::cell::RefCell;
use std::rc::Rc;

struct Node {
    val: i32,
}

fn get(node: &Option<Rc<RefCell<Node>>>) -> i32 {
    match *node {
        Some(_) => 1,
        None => 0,
    }
}

fn main() {
    println(f\"{get(&None)}\");
}
";
    let text = mend(&dir, src);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(text.contains("fn get(node: ref Option[Node])"), "{text}");
}
