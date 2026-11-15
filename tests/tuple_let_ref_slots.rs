//! B-2026-11-14-23: `let (short, long) = if .. { (self.a, o.a) } else
//! { (o.a, self.a) };` over borrowed places got a `.clone()` fix on every
//! element, copying both collections on each call. A slot whose binding is
//! only read is fixed by borrowing each place it takes (`ref self.a`), as
//! the single-binding `let` already was (kata 2121); a `let mut`, or a slot
//! that some result fills with a value, keeps the clone.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn fixed(tag: &str, src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-tupleref-{tag}-{}", std::process::id()));
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

const HEAD: &str = "struct V {\n    pairs: Vec[i64],\n}\n\nimpl V {\n";
const MAIN: &str = "}\n\nfn main() {\n    let a = V { pairs: [1, 2] };\n    let b = V { pairs: [1, 2, 3] };\n    println(a.total(b));\n}\n";

#[test]
fn a_tuple_of_borrowed_fields_only_read_is_fixed_with_ref() {
    let body = "    fn total(ref self, other: V) -> i64 {\n        \
let (short, long) = if self.pairs.len() <= other.pairs.len() { (self.pairs, other.pairs) } else { (other.pairs, self.pairs) };\n        \
short.len() * 100 + long.len()\n    }\n";
    let (after, out) = fixed("read", &format!("{HEAD}{body}{MAIN}"));
    assert!(
        after.contains(
            "{ (ref self.pairs, ref other.pairs) } else { (ref other.pairs, ref self.pairs) }"
        ),
        "{after}"
    );
    assert!(!after.contains(".clone()"), "{after}");
    assert_eq!(out, "203\n");
}

#[test]
fn a_mut_tuple_let_keeps_the_clone() {
    let body = "    fn total(ref self, other: V) -> i64 {\n        \
let mut (short, long) = (self.pairs, other.pairs);\n        \
short.push(9);\n        \
short.len() * 100 + long.len()\n    }\n";
    let (after, out) = fixed("written", &format!("{HEAD}{body}{MAIN}"));
    assert!(
        after.contains("(self.pairs.clone(), other.pairs.clone())"),
        "{after}"
    );
    assert_eq!(out, "303\n");
}

#[test]
fn a_tuple_slot_a_branch_fills_with_a_value_keeps_the_clone() {
    let body = "    fn total(ref self, other: V) -> i64 {\n        \
let (short, long) = if self.pairs.len() <= other.pairs.len() { (self.pairs, other.pairs) } else { (Vec.new(), self.pairs) };\n        \
short.len() * 100 + long.len()\n    }\n";
    let (after, out) = fixed("mixed", &format!("{HEAD}{body}{MAIN}"));
    assert!(
        after.contains(
            "{ (self.pairs.clone(), ref other.pairs) } else { (Vec.new(), ref self.pairs) }"
        ),
        "{after}"
    );
    assert_eq!(out, "203\n");
}
