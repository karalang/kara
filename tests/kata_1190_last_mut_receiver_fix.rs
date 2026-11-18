//! B-2026-11-18-30: Rust's `stack.last_mut().unwrap().push(c)`, a call
//! through the last element (kata 1190's Rust arm), got "no method
//! 'last_mut' on type 'Vec': write through an index instead" with no fix,
//! where `*v.last_mut().unwrap() = x` (kata 1520) and the `let`, `match`
//! and `if let` shapes were rewritten. A call, a field or an element read
//! or written through `v.last_mut().unwrap()` (or `first_mut`, or
//! `.expect(..)`) now takes the index, `v[v.len() - 1]` or `v[0]`, unless
//! what else the expression evaluates names `v`.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-last-mut-receiver-{tag}-{}-{}",
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

#[test]
fn a_call_field_or_element_through_last_mut_is_fixed_to_the_index() {
    let src = "struct Run {\n    start: i64,\n    count: i64,\n}\n\n\
               struct Book {\n    runs: Vec[Run],\n}\n\n\
               impl Book {\n    fn grow(mut ref self) {\n        \
               self.runs.last_mut().unwrap().count += 1;\n    }\n}\n\n\
               fn main() {\n    let mut groups: Vec[Vec[i64]] = [[1], [2]];\n    \
               groups.last_mut().unwrap().push(3);\n    \
               groups.first_mut().expect(\"a group\").push(0);\n    \
               groups.last_mut().unwrap()[0] = 7;\n    \
               let mut runs = [Run { start: 0, count: 1 }];\n    \
               runs.last_mut().unwrap().count = 4;\n    \
               *groups.last_mut().unwrap() = [9, 9];\n    \
               let mut book = Book { runs: [Run { start: 5, count: 1 }] };\n    \
               book.grow();\n    \
               book.runs.last_mut().unwrap().start = 6;\n    \
               println(f\"{groups} {runs[0].count} {book.runs[0].start} {book.runs[0].count}\");\n}\n";
    let (dir, path) = fixture("shapes", src);
    let first = karac().arg("check").arg(&path).output().unwrap();
    assert!(!first.status.success());
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    let want = src
        .replace(
            "self.runs.last_mut().unwrap().count",
            "self.runs[self.runs.len() - 1].count",
        )
        .replace(
            "groups.last_mut().unwrap().push(3)",
            "groups[groups.len() - 1].push(3)",
        )
        .replace(
            "groups.first_mut().expect(\"a group\").push(0)",
            "groups[0].push(0)",
        )
        .replace(
            "groups.last_mut().unwrap()[0] = 7",
            "groups[groups.len() - 1][0] = 7",
        )
        .replace(
            "runs.last_mut().unwrap().count = 4",
            "runs[runs.len() - 1].count = 4",
        )
        .replace(
            "*groups.last_mut().unwrap() = [9, 9]",
            "groups[groups.len() - 1] = [9, 9]",
        )
        .replace(
            "book.runs.last_mut().unwrap().start",
            "book.runs[book.runs.len() - 1].start",
        );
    assert_eq!(fixed, want);
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert!(
        run.status.success(),
        "{fixed}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "[[1, 0], [9, 9]] 4 6 2\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_call_through_last_mut_that_names_the_vector_is_left_alone() {
    let src = "fn main() {\n    let mut groups: Vec[Vec[i64]] = [[1], [2]];\n    \
               groups.last_mut().unwrap().push(groups.len());\n    \
               println(f\"{groups}\");\n}\n";
    let (dir, path) = fixture("names", src);
    let first = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&first.stderr);
    assert!(
        err.contains("no method 'last_mut' on type 'Vec': write through an index instead"),
        "{err}"
    );
    karac().arg("fix").arg(&path).output().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), src);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_rust_stack_of_groups_carries_through_karac_fix() {
    let src = "struct Solution {}\n\nimpl Solution {\n    \
               pub fn reverse_parentheses(s: String) -> String {\n        \
               let mut stack: Vec[Vec[char]] = [Vec.new()];\n        \
               for c in s.chars() {\n            match c {\n                \
               '(' => stack.push(Vec.new()),\n                ')' => {\n                    \
               let mut top = stack.pop().unwrap();\n                    top.reverse();\n                    \
               stack.last_mut().unwrap().extend(top);\n                }\n                \
               _ => stack.last_mut().unwrap().push(c),\n            }\n        }\n        \
               stack.pop().unwrap().into_iter().collect()\n    }\n}\n\n\
               fn main() {\n    println(Solution.reverse_parentheses(\"(ed(et(oc))el)\"));\n}\n";
    let (dir, path) = fixture("stack", src);
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        fixed,
        src.replace("stack.last_mut().unwrap()", "stack[stack.len() - 1]")
    );
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "leetcode\n");
    let _ = std::fs::remove_dir_all(&dir);
}
