//! Resources rooted at a value in an effect clause (design.md § Value-rooted
//! resources): at the receiver, `sends(self)` and `writes(self.n)`
//! (B-2026-10-31-4), and at a parameter of any non-scalar type, `writes(f)`
//! and `receives(listener)` (B-2026-10-31-9). The clause states the
//! resource; `par` conflicts come from what the body does to each value.

use std::path::PathBuf;
use std::process::{Command, Output};

fn program(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-value-roots-{}-{tag}", std::process::id()));
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

/// `karac check` passes and the MIR interpreter prints `expected`.
fn checks_and_prints(tag: &str, src: &str, expected: &str) {
    let path = program(tag, src);
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{}", all(&out));
    let out = karac(&["__mir-run"], &path);
    assert!(out.status.success(), "{}", all(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
}

fn check_fails_with(tag: &str, src: &str, needle: &str) {
    let path = program(tag, src);
    let out = karac(&["check"], &path);
    let text = all(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains(needle), "{text}");
}

#[test]
fn a_method_roots_a_resource_at_self_and_at_a_field_of_self() {
    // `bump` is public and declares nothing: a value-rooted resource on the
    // argument it passes is not a capability it has to state.
    checks_and_prints(
        "self-roots",
        "pub struct C {\n    n: i64,\n}\n\n\
         impl C {\n    fn g(mut ref self) with writes(self.n) {\n        self.n += 1;\n    }\n\n    \
         pub fn h(ref self) -> i64 with reads(self) {\n        self.n\n    }\n}\n\n\
         pub fn bump(c: mut ref C) -> i64 {\n    c.g();\n    c.h()\n}\n\n\
         fn main() {\n    let mut c = C { n: 1 };\n    println(bump(mut c));\n}\n",
        "2\n",
    );
}

#[test]
fn a_function_without_a_receiver_cannot_root_a_resource_at_self() {
    check_fails_with(
        "no-receiver",
        "fn f(x: i64) -> i64 with reads(self) {\n    x\n}\n\nfn main() {\n    println(f(1));\n}\n",
        "the resource `self` is rooted at `self`, and this function has no `self` receiver",
    );
}

const OUT: &str = "struct Out {\n    tx: Sender[i64],\n}\n\n\
     impl Out {\n    fn put(ref self, v: i64) with sends(self) blocks {\n        self.tx.send(v).unwrap();\n    }\n}\n\n";

#[test]
fn par_branches_sending_through_two_receivers_do_not_conflict() {
    checks_and_prints(
        "two-values",
        &format!(
            "{OUT}fn main() {{\n    \
             let (tx1, rx1): (Sender[i64], Receiver[i64]) = channel(8);\n    \
             let (tx2, rx2): (Sender[i64], Receiver[i64]) = channel(8);\n    \
             let a = Out {{ tx: tx1 }};\n    let b = Out {{ tx: tx2 }};\n    \
             par {{\n        a.put(1),\n        b.put(2),\n    }};\n    \
             println(rx1.recv().unwrap() + rx2.recv().unwrap());\n}}\n"
        ),
        "3\n",
    );
}

#[test]
fn par_branches_sending_through_one_receiver_conflict() {
    check_fails_with(
        "one-value",
        &format!(
            "{OUT}fn main() {{\n    \
             let (tx, rx): (Sender[i64], Receiver[i64]) = channel(8);\n    \
             let a = Out {{ tx: tx }};\n    \
             par {{\n        a.put(1),\n        a.put(2),\n    }};\n    \
             println(rx.recv().unwrap() + rx.recv().unwrap());\n}}\n"
        ),
        "branches 1 and 2 of the `par` conflict",
    );
}

#[test]
fn a_parameter_of_any_non_scalar_type_roots_a_resource() {
    checks_and_prints(
        "param-roots",
        "pub struct Log {\n    lines: Vec[String],\n}\n\n\
         pub fn note(log: mut ref Log, line: Str) with writes(log) writes(log.lines) {\n    \
         log.lines.push(line.to_string());\n}\n\n\
         fn main() {\n    let mut log = Log { lines: Vec.new() };\n    note(mut log, \"a\");\n    \
         note(mut log, \"b\");\n    println(log.lines.len());\n}\n",
        "2\n",
    );
}

#[test]
fn a_scalar_parameter_still_roots_no_resource() {
    check_fails_with(
        "scalar-param",
        "fn f(x: i64) -> i64 with writes(x) {\n    x\n}\n\nfn main() {\n    println(f(1));\n}\n",
        "is not an effect resource",
    );
}
