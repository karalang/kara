//! Kata 1080, LeetCode's Rust template for Insufficient Nodes in Root to
//! Leaf Paths: `struct Solution; impl Solution { pub fn
//! sufficient_subset(root: Option<Rc<RefCell<TreeNode>>>, ..) }`. `karac
//! fix` carries it to `Option[TreeNode]` over the program's `shared struct
//! TreeNode`, and the checker then refused the method with E0221, "private
//! type 'TreeNode' leaks through method parameter". `Solution` is not
//! `pub` either, so the method is on no public surface; Rust accepts the
//! same shape. An inherent impl of a type that is not `pub` is no longer
//! checked for leaks (B-2026-11-09-24).

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

/// Whether the program checks clean, the check's diagnostics, and what it
/// prints under the MIR interpreter.
fn checked(name: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-1080-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let (clean, _, diags) = karac(&dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (clean, diags, out)
}

#[test]
fn kata_1080_a_pub_fn_of_a_private_type_may_name_private_types() {
    let src = "shared struct TreeNode {\n    mut val: i32,\n    mut left: Option[TreeNode],\n}\n\nstruct Solution {}\n\nimpl Solution {\n    pub fn deeper(root: Option[TreeNode], by: i32) -> Option[TreeNode] {\n        let n = root?;\n        Some(TreeNode { val: n.val + by, left: n.left })\n    }\n}\n\nfn main() {\n    let t = Solution.deeper(Some(TreeNode { val: 1, left: None }), 2).unwrap();\n    println(f\"{t.val}\");\n}";
    let (clean, diags, out) = checked("private", src);
    assert!(clean, "{diags}");
    assert!(!diags.contains("leaks through"), "{diags}");
    assert_eq!(out.trim_end(), "3");
}

#[test]
fn kata_1080_a_pub_fn_of_a_pub_type_still_may_not() {
    let src = "struct Hidden {\n    y: i64,\n}\n\npub struct Outer {\n    pub x: i64,\n}\n\nimpl Outer {\n    pub fn peek(ref self) -> Hidden {\n        Hidden { y: self.x }\n    }\n}\n\nfn main() {\n    println(f\"{Outer { x: 4 }.peek().y}\");\n}";
    let (clean, diags, _) = checked("public", src);
    assert!(!clean, "a pub type's pub fn leaks the private type");
    assert!(
        diags.contains("private type 'Hidden' leaks through method return type of 'peek'"),
        "{diags}"
    );
}
