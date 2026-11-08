//! LeetCode's Rust tree template, `Option<Rc<RefCell<TreeNode>>>` (kata
//! 1530's): Kāra has no `Rc` or `RefCell`, and `karac fix` now carries the
//! shape to a `shared struct TreeNode` with `mut` fields, the type to
//! `Option[TreeNode]`, `Rc::new(RefCell::new(x))` to `x`, and drops
//! `.borrow()` / `.borrow_mut()`. B-2026-11-08-65.

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

/// The first check's diagnostics, the text `karac fix` leaves, whether
/// that text checks clean, and what it prints under the MIR interpreter.
fn mend(dir: &Path, src: &str) -> (String, String, bool, String) {
    std::fs::create_dir_all(dir).unwrap();
    let file = dir.join("tree.kara");
    std::fs::write(&file, src).unwrap();
    let first = karac(dir, &["check", "tree.kara"]).2;
    for _ in 0..8 {
        let before = std::fs::read_to_string(&file).unwrap();
        karac(dir, &["fix", "tree.kara"]);
        if std::fs::read_to_string(&file).unwrap() == before {
            break;
        }
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (clean, _, _) = karac(dir, &["check", "tree.kara"]);
    let (_, out, _) = karac(dir, &["__mir-run", "tree.kara"]);
    let _ = std::fs::remove_dir_all(dir);
    (first, text, clean, out)
}

const TEMPLATE: &str = r#"use std::rc::Rc;
use std::cell::RefCell;

#[derive(Debug, PartialEq, Eq)]
pub struct TreeNode {
    pub val: i32,
    pub left: Option<Rc<RefCell<TreeNode>>>,
    pub right: Option<Rc<RefCell<TreeNode>>>,
}

impl TreeNode {
    #[inline]
    pub fn new(val: i32) -> Self {
        TreeNode { val, left: None, right: None }
    }
}

struct Solution {}

impl Solution {
    pub fn count_pairs(root: Option<Rc<RefCell<TreeNode>>>, distance: i32) -> i32 {
        let mut ans = 0;
        Self::dfs(&root, distance as usize, &mut ans);
        ans
    }

    // counts[i]: the leaves i edges below `node`, for i < d
    fn dfs(node: &Option<Rc<RefCell<TreeNode>>>, d: usize, ans: &mut i32) -> Vec<i32> {
        let mut counts: Vec<i32> = vec![0; d];
        if let Some(n) = node {
            let n = n.borrow();
            if n.left.is_none() && n.right.is_none() {
                counts[0] = 1;
                return counts;
            }
            let l = Self::dfs(&n.left, d, ans);
            let r = Self::dfs(&n.right, d, ans);
            for i in 0..d {
                for j in 0..d {
                    if i + j + 2 <= d {
                        *ans += l[i] * r[j];
                    }
                }
            }
            for i in 0..d - 1 {
                counts[i + 1] = l[i] + r[i];
            }
        }
        counts
    }
}

fn leaf(v: i32) -> Option<Rc<RefCell<TreeNode>>> {
    Some(Rc::new(RefCell::new(TreeNode::new(v))))
}

fn main() {
    let root = Rc::new(RefCell::new(TreeNode::new(1)));
    root.borrow_mut().left = leaf(2);
    root.borrow_mut().right = Some(Rc::new(RefCell::new(TreeNode { val: 3, left: leaf(4), right: leaf(5) })));
    let again = Rc::clone(&root);
    again.borrow_mut().val = 10;
    let val = root.borrow().val;
    println(f"{val} {Solution::count_pairs(Some(root), 3)}");
}
"#;

#[test]
fn rc_refcell_tree_template_becomes_a_shared_struct() {
    let dir = std::env::temp_dir().join(format!("karac_rc_refcell_{}", std::process::id()));
    let (first, text, clean, out) = mend(&dir, TEMPLATE);
    assert!(
        first.contains("Kāra has no `Rc` or `RefCell`") && first.contains("shared struct"),
        "the import's diagnostic names `shared struct`:\n{first}"
    );
    assert!(
        !first.contains("std.io"),
        "no `did you mean std.io`:\n{first}"
    );
    assert!(
        text.contains("pub shared struct TreeNode")
            && text.contains("pub mut val: i32")
            && text.contains("pub mut left: Option[TreeNode]"),
        "the struct is shared, its fields mut:\n{text}"
    );
    for gone in ["Rc", "RefCell", "borrow"] {
        assert!(!text.contains(gone), "`{gone}` is left in:\n{text}");
    }
    assert!(clean, "the fixed template checks clean:\n{text}");
    // Writing through `again` writes the node `root` holds: one node, two handles.
    assert_eq!(out, "10 3\n", "fixed text:\n{text}");
}

#[test]
fn rc_used_without_an_import_names_shared_struct() {
    // `Rc` with no `use` line: the resolver says what it is in Kāra.
    let dir = std::env::temp_dir().join(format!("karac_rc_dots_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("m.kara"),
        "fn main() {\n    let x = Rc.new(5);\n}\n",
    )
    .unwrap();
    let (_, _, err) = karac(&dir, &["check", "m.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        err.contains("`Rc` and `RefCell` are Rust's") && err.contains("shared struct"),
        "{err}"
    );
}

#[test]
fn self_return_of_a_shared_struct_is_the_handle_type() {
    // `-> Self` in an impl of a `shared struct` was "expected 'Node', found
    // 'Node'" at every annotated use of the call (B-2026-11-08-70).
    let dir = std::env::temp_dir().join(format!("karac_shared_self_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = "shared struct Node {\n    mut val: i64,\n}\n\nimpl Node {\n    fn new(val: i64) -> Self {\n        Node { val }\n    }\n}\n\nfn main() {\n    let x: Option[Node] = Some(Node.new(1));\n    let y: Node = Node.new(2);\n    let z = y;\n    z.val = 5;\n    println(f\"{x.unwrap().val} {y.val}\");\n}\n";
    std::fs::write(dir.join("m.kara"), src).unwrap();
    let (ok, _, err) = karac(&dir, &["check", "m.kara"]);
    let (_, out, _) = karac(&dir, &["__mir-run", "m.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{err}");
    assert_eq!(out, "1 5\n");
}
