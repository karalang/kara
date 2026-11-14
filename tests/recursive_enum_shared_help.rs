//! B-2026-11-14-2 (design.md § Recursive enums require `shared enum`): the ownership
//! cycle on a recursive plain enum offered "use 'ref', 'Box[T]', or mark the
//! type as 'shared'", and following it with `Box` met "`Box` is Rust's heap
//! box: Kāra has none". The help now gives the design's remedy, `shared enum`
//! (which `karac fix` writes) or a `Vec`, and never `Box`.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-recursive-enum-help-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

const TREE: &str = "enum Tree { Leaf(i64), Node(Tree, Tree) }\n\
    fn depth(t: Tree) -> i64 {\n    \
        match t {\n        \
            Tree.Leaf(_) => 1,\n        \
            Tree.Node(l, r) => {\n            \
                let a = depth(l);\n            \
                let b = depth(r);\n            \
                1 + (if a > b { a } else { b })\n        \
            }\n    \
        }\n\
    }\n\
    fn main() {\n    \
        let t = Tree.Node(Tree.Leaf(1), Tree.Node(Tree.Leaf(2), Tree.Leaf(3)));\n    \
        println(depth(t));\n\
    }\n";

#[test]
fn recursive_enum_cycle_help_names_shared_enum_and_fix_writes_it() {
    let (dir, path) = fixture("tree", TREE);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must refuse: {err}");
    assert!(err.contains("ownership cycle detected"), "{err}");
    assert!(err.contains("shared enum Tree"), "{err}");
    assert!(!err.contains("Box"), "the help must not offer `Box`: {err}");
    for _ in 0..3 {
        karac().arg("fix").arg(&path).output().unwrap();
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fixed.starts_with("shared enum Tree {"),
        "`karac fix` must write `shared enum`:\n{fixed}"
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "check must pass after `karac fix`:\n{fixed}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A cycle with no case that ends it is not fixed by `shared`, so the help
/// names a type that can end the recursion, and still never `Box`.
#[test]
fn endless_cycle_help_names_option_or_vec() {
    let src = "struct Node { v: i64, next: Node }\nfn main() {}\n";
    let (dir, path) = fixture("struct", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success(), "check must refuse: {err}");
    assert!(err.contains("Option[T]"), "{err}");
    assert!(!err.contains("Box"), "the help must not offer `Box`: {err}");
}
