//! B-2026-11-15-1: the remainder of B-2026-11-14-23. A tuple `let` slot
//! whose binding is read only inside a closure kept its `.clone()` fix,
//! because any mention in a closure counted as a capture that is never
//! "only read" (kata 1570, Dot Product of Two Sparse Vectors: the larger
//! map was copied on every product). A closure passed straight to a call
//! that borrows its captures is now judged like any other use; a closure
//! bound to a name, or one that takes its captures `own`, keeps the clone.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn fixed(tag: &str, src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-tuplerefcl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    let out = karac(&["fix", p]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = std::fs::read_to_string(&f).unwrap();
    let check = karac(&["check", p]);
    assert!(
        check.status.success(),
        "{after}\n{}",
        String::from_utf8_lossy(&check.stdout)
    );
    let run = karac(&["run", "--interp", p]);
    assert!(
        run.status.success(),
        "{after}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    (after, String::from_utf8_lossy(&run.stdout).into_owned())
}

const HEAD: &str = "struct Sparse {\n    values: Map[i64, i64],\n}\n\nimpl Sparse {\n";
const MAIN: &str =
    "}\n\nfn main() {\n    let mut a = Map.new();\n    a[1] = 2;\n    a[3] = 4;\n    \
let mut b = Map.new();\n    b[1] = 5;\n    b[2] = 7;\n    b[3] = 1;\n    \
let x = Sparse { values: a };\n    let y = Sparse { values: b };\n    println(x.dot(y));\n}\n";
const SPLIT: &str = "let (small, large) = if self.values.len() <= other.values.len() \
{ (self.values, other.values) } else { (other.values, self.values) };\n        ";

#[test]
fn a_slot_read_only_in_a_call_argument_closure_is_fixed_with_ref() {
    let body = format!(
        "    fn dot(ref self, other: Sparse) -> i64 {{\n        {SPLIT}\
small.iter().map(|(i, x)| x * large.get(i).cloned().unwrap_or(0)).sum()\n    }}\n"
    );
    let (after, out) = fixed("arg", &format!("{HEAD}{body}{MAIN}"));
    assert!(
        after.contains(
            "{ (ref self.values, ref other.values) } else { (ref other.values, ref self.values) }"
        ),
        "{after}"
    );
    assert!(!after.contains(".clone()"), "{after}");
    assert_eq!(out, "14\n");
}

#[test]
fn a_slot_read_in_an_own_closure_keeps_the_clone() {
    let body = format!(
        "    fn dot(ref self, other: Sparse) -> i64 {{\n        {SPLIT}\
small.iter().map(own |(i, x)| x * large.get(i).cloned().unwrap_or(0)).sum()\n    }}\n"
    );
    let (after, out) = fixed("own", &format!("{HEAD}{body}{MAIN}"));
    assert!(
        after.contains(
            "{ (ref self.values, other.values.clone()) } else { (ref other.values, self.values.clone()) }"
        ),
        "{after}"
    );
    assert_eq!(out, "14\n");
}

#[test]
fn a_slot_read_in_a_bound_closure_keeps_the_clone() {
    let body = format!(
        "    fn dot(ref self, other: Sparse) -> i64 {{\n        {SPLIT}\
let weight = |i: i64| large.get(i).cloned().unwrap_or(0);\n        \
small.iter().map(|(i, x)| x * weight(i)).sum()\n    }}\n"
    );
    let (after, out) = fixed("bound", &format!("{HEAD}{body}{MAIN}"));
    assert!(
        after.contains(
            "{ (ref self.values, other.values.clone()) } else { (ref other.values, self.values.clone()) }"
        ),
        "{after}"
    );
    assert_eq!(out, "14\n");
}
