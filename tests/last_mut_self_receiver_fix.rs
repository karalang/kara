//! B-2026-11-21-65: `if let Some(below) = self.owed.last_mut() { *below +=
//! extra; }`, Rust's way of handing a pop's pending increment down (kata
//! 1381's ★ arm), was rewritten by `karac fix` as `r#self.owed[self.owed
//! .len() - 1] += extra`: the element stood in for the binding as a name
//! spelling its text, and the renderer escaped that name because it began
//! with the keyword `self`. The fix refused to write the file ("would
//! leave ... MORE parse errors than it started with"). The element is now
//! built as the index expression it is.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir =
        std::env::temp_dir().join(format!("karac-last-mut-self-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

fn fix_check_run(tag: &str, src: &str) -> (String, bool, String) {
    let (dir, path) = fixture(tag, src);
    let fix = karac().arg("fix").arg(&path).output().unwrap();
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&path).unwrap();
    let check = karac().arg("check").arg(&path).output().unwrap();
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        fixed,
        check.status.success(),
        String::from_utf8_lossy(&run.stdout).into_owned(),
    )
}

#[test]
fn a_self_field_receiver_of_last_mut_is_rewritten_without_an_escape() {
    let src = "struct Owing {
    owed: Vec[i32],
}

impl Owing {
    fn hand_down(mut ref self, extra: i32) {
        if let Some(below) = self.owed.last_mut() {
            *below += extra;
        }
    }
}

fn main() {
    let mut o = Owing { owed: [1, 2] };
    o.hand_down(5);
    let mut none = Owing { owed: [] };
    none.hand_down(5);
    println(f\"{o.owed} {none.owed.len()}\");
}
";
    let (fixed, clean, out) = fix_check_run("plain", src);
    assert!(!fixed.contains("r#self"), "{fixed}");
    assert!(
        fixed.contains("self.owed[self.owed.len() - 1] += extra;"),
        "{fixed}"
    );
    assert!(clean, "{fixed}");
    assert_eq!(out, "[1, 7] 0\n");
}

#[test]
fn a_self_field_receiver_of_pairs_reads_the_fields_without_an_escape() {
    let src = "struct Runs {
    runs: Vec[(i32, i32)],
}

impl Runs {
    fn add(mut ref self, c: i32) {
        match self.runs.first_mut() {
            Some((v, n)) if *v == c => *n += 1,
            _ => self.runs.push((c, 1)),
        }
    }
}

fn main() {
    let mut r = Runs { runs: [] };
    for c in [4, 4, 5] {
        r.add(c);
    }
    println(f\"{r.runs}\");
}
";
    let (fixed, clean, out) = fix_check_run("pairs", src);
    assert!(!fixed.contains("r#self"), "{fixed}");
    assert!(fixed.contains("self.runs[0].1 += 1"), "{fixed}");
    assert!(clean, "{fixed}");
    assert_eq!(out, "[(4, 2), (5, 1)]\n");
}
