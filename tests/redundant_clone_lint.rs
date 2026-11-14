//! B-2026-11-14-3: `f(x.clone())` where `f`'s parameter only borrows copies
//! `x` for nothing (a bare parameter borrows, core-semantics.md §4.1), and no
//! lint said so: kata 2125's adapter kept `Solution.number_of_beams(bank.clone())`
//! after `karac fix` carried it from Rust. `redundant_clone` flags it, and
//! `karac fix` deletes the `.clone()`.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-redundant-clone-{tag}-{}-{}",
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

const DEFS: &str = "struct Solution {}\n\
    impl Solution { pub fn beams(bank: Vec[String]) -> i64 { bank.len() } }\n\
    fn count(v: Vec[i64]) -> i64 { v.len() }\n\
    fn take(v: own Vec[i64]) -> i64 { v.len() }\n\
    fn both(a: Vec[i64], b: mut ref Vec[i64]) -> i64 { b.push(1); a.len() }\n";

fn lint_count(src: &str, tag: &str) -> (usize, String) {
    let (dir, path) = fixture(tag, src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "{tag}: check must accept:\n{src}\n{err}"
    );
    (err.matches("warning[redundant_clone]").count(), err)
}

#[test]
fn redundant_clone_flags_and_fixes_a_clone_into_a_borrowing_parameter() {
    let src = format!(
        "{DEFS}fn adapter(bank: Vec[String]) -> i64 {{ Solution.beams(bank.clone()) }}\n\
         fn main() {{\n    let v = [1, 2, 3];\n    println(count(v.clone()));\n    \
         println(adapter([\"a\".to_string()]));\n}}\n"
    );
    let (n, err) = lint_count(&src, "flag");
    assert_eq!(n, 2, "{err}");
    assert!(
        err.contains("`bank.clone()` copies `bank` for a parameter that only borrows it"),
        "{err}"
    );
    let (dir, path) = fixture("fix", &src);
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(fixed.contains("Solution.beams(bank)"), "{fixed}");
    assert!(fixed.contains("println(count(v));"), "{fixed}");
    let check = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        check.status.success(),
        "{fixed}\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "3\n1\n");
}

/// An `own` parameter takes the copy, a `mut ref` argument or receiver on
/// the same root would conflict with the borrow, and a hand-written `Clone`
/// may do more than copy: none is flagged.
#[test]
fn redundant_clone_leaves_needed_clones_alone() {
    let src = format!(
        "{DEFS}struct P {{ v: Vec[i64] }}\n\
         impl Clone for P {{ fn clone(self) -> P {{ println(\"copied\"); P {{ v: self.v.clone() }} }} }}\n\
         fn look(p: P) -> i64 {{ p.v.len() }}\n\
         struct H {{ v: Vec[i64] }}\n\
         impl H {{ fn add(mut ref self, w: Vec[i64]) {{ self.v.push(w.len()); }} }}\n\
         fn main() {{\n    let mut v = [1, 2, 3];\n    \
         println(take(v.clone()));\n    \
         println(both(v.clone(), mut v));\n    \
         let p = P {{ v: [1] }};\n    println(look(p.clone()));\n    \
         let mut h = H {{ v: [] }};\n    h.add(h.v.clone());\n    println(h.v.len());\n}}\n"
    );
    let (n, err) = lint_count(&src, "needed");
    assert_eq!(n, 0, "{err}");
}
