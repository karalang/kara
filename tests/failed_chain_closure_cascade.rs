//! B-2026-11-19-88: when an assignment's iterator chain failed to type, its
//! closures were read as outliving the statement, so a value a filter
//! closure borrowed got a spurious E0500 ("moved inside a loop"), whose
//! machine fix cloned it beside the real fix. Only the chain's own error is
//! reported now, and its fix alone makes the program check.

use std::process::Command;

const SRC: &str = "fn links(u: i64) -> Vec[i64] { [u + 1, u * 2] }
fn main() {
    let mut seen: Set[i64] = Set.new();
    let mut level: Vec[i64] = [1];
    while not level.is_empty() {
        level = level.iter().flat_map(|u| links(u)).filter(|v| v < 10 and seen.insert(v)).collect();
    }
    let all: Vec[i64] = seen.into_iter().collect();
    println(all.len());
}
";

#[test]
fn failed_assignment_chain_reports_no_move_of_what_its_closures_borrow() {
    let dir = std::env::temp_dir().join(format!("karac-chain-cascade-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, SRC).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let out = Command::new(karac).arg("check").arg(&f).output().unwrap();
    let text =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("E0200"), "{text}");
    assert!(!text.contains("E0500"), "{text}");
    Command::new(karac).arg("fix").arg(&f).output().unwrap();
    let fixed = std::fs::read_to_string(&f).unwrap();
    assert!(fixed.contains("links(u).into_iter()"), "{fixed}");
    assert!(!fixed.contains("clone"), "{fixed}");
    let run = Command::new(karac)
        .arg("__mir-run")
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "8\n");
}
