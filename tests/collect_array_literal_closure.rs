//! B-2026-11-15-38: `d.iter().map(|e| [e[1], e[0], 1 - e[2]]).collect()`
//! into a `Vec[Array[i64, 3]]`. The closure's list literal is checked
//! before the `collect` names its element, so it is a `Vec` (design.md
//! § List literals) and the `FromIterator` bound was refused with no fix.
//! The refusal now says why and carries the fix that types the literal
//! where the closure makes it.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

#[test]
fn an_array_literal_returned_by_a_map_closure_is_typed_by_the_fix() {
    let src = "fn main() {
    let a: Array[i64, 3] = [1, 2, 0];
    let b: Array[i64, 3] = [3, 4, 1];
    let mut d: Vec[Array[i64, 3]] = Vec.new();
    d.push(a);
    d.push(b);
    let swapped: Vec[Array[i64, 3]] = d.iter().map(|e| [e[1], e[0], 1 - e[2]]).collect();
    println(f\"{swapped}\");
}
";
    let dir = std::env::temp_dir().join(format!("karac-collectarr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    let check = karac(&["check", p]);
    let text = String::from_utf8_lossy(&check.stdout).into_owned()
        + &String::from_utf8_lossy(&check.stderr);
    assert!(!check.status.success(), "{text}");
    assert!(
        text.contains("a list literal is one unless its use asks for an array"),
        "{text}"
    );
    let fix = karac(&["fix", p]);
    assert!(fix.status.success());
    let after = std::fs::read_to_string(&f).unwrap();
    assert!(
        after.contains("map(|e| { let r: Array[i64, 3] = [e[1], e[0], 1 - e[2]]; r })"),
        "{after}"
    );
    let run = karac(&["run", "--interp", p]);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "[[2, 1, 1], [4, 3, 0]]\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_literal_of_the_wrong_length_gets_no_array_fix() {
    let src = "fn main() {
    let a: Array[i64, 3] = [1, 2, 0];
    let mut d: Vec[Array[i64, 3]] = Vec.new();
    d.push(a);
    let swapped: Vec[Array[i64, 3]] = d.iter().map(|e| [e[1], e[0]]).collect();
    println(f\"{swapped}\");
}
";
    let dir = std::env::temp_dir().join(format!("karac-collectarr2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let check = karac(&["check", f.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&check.stdout).into_owned()
        + &String::from_utf8_lossy(&check.stderr);
    assert!(!check.status.success(), "{text}");
    assert!(!text.contains("let r: Array"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
