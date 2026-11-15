//! Kata 1166's trie arm walks a tree of `shared struct Dir` nodes with
//! `match at.kids.get(name) { Some(next) => at = next, None => .. }`.
//! `get` returns `Option[ref Dir]`, so `next` borrows `at` and the
//! assignment that drops `at` is refused (E0516), which is right. The
//! message names the repair, `.cloned()` on the `Option[ref Dir]`, and a
//! `while let` or `if let` scrutinee got it as a fix-it, but a `match`
//! scrutinee got none (B-2026-11-15-49). It now gets `.cloned()` when the
//! element is `Copy` or `shared`.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

const TRIE: &str = "shared struct Dir {
    mut kids: Map[String, Dir],
    value: i64,
}

fn depth_of(root: Dir, names: Vec[Str]) -> i64 {
    let mut at = root;
    let mut depth = 0;
    for name in names.iter() {
        match at.kids.get(name) {
            Some(next) => at = next,
            None => return -depth,
        }
        depth += 1;
    }
    depth * 100 + at.value
}

fn main() {
    let leaf = Dir { kids: Map.new(), value: 7 };
    let mid = Dir { kids: Map.new(), value: 3 };
    mid.kids.insert(\"b\".to_string(), leaf);
    let root = Dir { kids: Map.new(), value: -1 };
    root.kids.insert(\"a\".to_string(), mid);
    println(f\"{depth_of(root, [\"a\", \"b\"])} {depth_of(root, [\"a\", \"x\"])}\");
}
";

#[test]
fn kata_1166_match_on_a_borrowed_shared_value_gets_cloned_fix() {
    let file = write("k1166-match-cloned", TRIE);
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(stderr.contains("[E0516]"), "{stderr}");
    let fix = karac(&["fix", &file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        fixed.contains("match at.kids.get(name).cloned() {"),
        "{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "207 -1\n");
}

#[test]
fn kata_1166_match_on_a_borrowed_string_gets_no_fix() {
    // A `String` would be a deep copy: no fix-it, the refusal stays.
    let src = "fn main() {
    let mut words = [\"a\".to_string()].to_vec();
    match words.first() {
        Some(w) => words.push(w.clone()),
        None => {}
    }
    println(f\"{words.len()}\");
}
";
    let file = write("k1166-match-string", src);
    let _ = karac(&["fix", &file]);
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(fixed.contains("match words.first() {"), "{fixed}");
}
