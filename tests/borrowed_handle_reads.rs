//! A `shared` handle read out of a borrowed place counts the handle and
//! leaves the place usable (core semantics §6.1): `d ?? fallback` on a bare
//! `d: Option[Node]` (B-2026-11-01-1).

use std::path::PathBuf;
use std::process::{Command, Output};

fn program(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-borrowed-handle-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .current_dir(path.parent().unwrap())
        .env("KARAC_AUTO_PAR", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn all(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `karac check` passes and the MIR interpreter prints `expected`; under
/// `llvm`, `karac run` prints it too.
fn checks_and_prints(tag: &str, src: &str, expected: &str) {
    let path = program(tag, src);
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{}", all(&out));
    let mut cmds = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        cmds.push("run");
    }
    for cmd in cmds {
        let out = karac(&[cmd], &path);
        assert!(out.status.success(), "{cmd}: {}", all(&out));
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{cmd}");
    }
}

const NODE: &str = "shared enum Node {\n    Leaf(i64),\n    Pair(Node, Node),\n}\n\n\
     fn show(t: Node) -> String {\n    match t {\n        Node.Leaf(v) => f\"{v}\",\n        \
     Node.Pair(_, _) => \"pair\",\n    }\n}\n\n";

/// The expected lines are the program's arithmetic: `take` lends `some`,
/// so `some` is still `Leaf(2)` after it, and `h.d` is still the pair.
#[test]
fn nil_coalesce_counts_a_handle_out_of_a_borrowed_option() {
    let src = format!(
        "{NODE}struct Holder {{\n    d: Option[Node],\n}}\n\n\
         fn take(d: Option[Node]) -> Node {{\n    d ?? Node.Leaf(0)\n}}\n\n\
         fn main() {{\n    let some = Some(Node.Leaf(2));\n    println(show(take(some)));\n    \
         println(show(take(None)));\n    println(show(some ?? Node.Leaf(9)));\n    \
         let h = Holder {{ d: Some(Node.Pair(Node.Leaf(1), Node.Leaf(2))) }};\n    \
         println(show(take(h.d)));\n    println(show(h.d ?? Node.Leaf(5)));\n}}\n"
    );
    checks_and_prints("coalesce", &src, "2\n0\n2\npair\npair\n");
}

/// `Some(k)` over `n: ref Option[Node]` binds `k` as a `ref` into `n`
/// (core semantics §4.6), so `kind_name(k)`'s view lives as long as `n`;
/// it used to bind a counted copy the arm dropped under the view
/// (B-2026-11-01-3). A use of `k` as a value still counts the handle.
/// The expected lines are the program's arithmetic.
#[test]
fn a_handle_bound_from_a_ref_scrutinee_is_a_ref_into_it() {
    let src = format!(
        "{NODE}fn kind_name(n: ref Node) -> Str {{\n    match n {{\n        Node.Leaf(_) => \"Leaf\",\n        \
         Node.Pair(_, _) => \"Pair\",\n    }}\n}}\n\n\
         fn eat(n: own Node) -> i64 {{\n    match n {{\n        Node.Leaf(v) => v,\n        \
         Node.Pair(_, _) => -1,\n    }}\n}}\n\n\
         fn no_attr(n: ref Option[Node]) -> String {{\n    let cls = match n {{\n        \
         Some(k) => kind_name(k),\n        None => \"NoneType\",\n    }};\n    f\"{{cls}} object\"\n}}\n\n\
         fn cls_of(n: ref Option[Node]) -> Str {{\n    match n {{\n        Some(k) => kind_name(k),\n        \
         None => \"NoneType\",\n    }}\n}}\n\n\
         fn get(n: ref Option[Node]) -> Node {{\n    match n {{\n        Some(k) => k,\n        \
         None => Node.Leaf(0),\n    }}\n}}\n\n\
         fn many(n: ref Option[Node]) -> i64 {{\n    let mut v: Vec[Node] = Vec.new();\n    \
         let mut total = 0;\n    if let Some(k) = n {{\n        v.push(k);\n        let y = k;\n        \
         total += eat(y) + eat(k);\n    }}\n    total + v.len()\n}}\n\n\
         fn main() {{\n    let o = Some(Node.Leaf(4));\n    println(no_attr(o));\n    \
         println(cls_of(None));\n    println(show(get(o)));\n    println(many(o));\n    \
         println(no_attr(o));\n}}\n"
    );
    checks_and_prints(
        "ref-scrutinee",
        &src,
        "Leaf object\nNoneType\n4\n9\nLeaf object\n",
    );
}
