//! B-2026-11-15-40: `println(f"{v} -> {total(v)}")` with `total(v: own
//! Vec[i64])`. The f-string's `{v}` borrows `v` for the whole `println`, so
//! the move in the later segment is E0516, and it had no machine fix:
//! `karac fix` stopped there. The fix now clones the moved argument.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

#[test]
fn a_value_moved_inside_the_fstring_that_shows_it_is_cloned_by_the_fix() {
    let src = "fn total(v: own Vec[i64]) -> i64 { v.iter().sum() }
fn show(v: own Vec[i64]) { println(f\"{v} -> {total(v)}\"); }
fn main() { show([1, 2, 3]); }
";
    let dir = std::env::temp_dir().join(format!("karac-fstrmove-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    let check = karac(&["check", p]);
    let text = String::from_utf8_lossy(&check.stdout).into_owned()
        + &String::from_utf8_lossy(&check.stderr);
    assert!(text.contains("E0516"), "{text}");
    let fix = karac(&["fix", p]);
    assert!(fix.status.success());
    let after = std::fs::read_to_string(&f).unwrap();
    assert!(after.contains("{total(v.clone())}"), "{after}");
    let run = karac(&["run", "--interp", p]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "[1, 2, 3] -> 6\n");
    let _ = std::fs::remove_dir_all(&dir);
}
