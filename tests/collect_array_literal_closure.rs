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

/// B-2026-11-16-181 flipped this pin: the `collect`'s element reaches the
/// `map` closure before its body is checked, so the literal is the array
/// and the program checks with no fix.
#[test]
fn an_array_literal_returned_by_a_map_closure_follows_the_collect() {
    let src = "fn main() {
    let a: Array[i64, 3] = [1, 2, 0];
    let b: Array[i64, 3] = [3, 4, 1];
    let mut d: Vec[Array[i64, 3]] = Vec.new();
    d.push(a);
    d.push(b);
    let swapped: Vec[Array[i64, 3]] = d.iter().map(|e| [e[1], e[0], 1 - e[2]]).collect();
    println(f\"{swapped}\");
    let kept: Vec[Array[i64, 3]] = d.iter().filter_map(|e| {
        if e[2] == 0 { Some([e[1], e[0], 1]) } else { None }
    }).collect();
    println(f\"{kept}\");
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
    assert!(check.status.success(), "{text}");
    let run = karac(&["run", "--interp", p]);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "[[2, 1, 1], [4, 3, 0]]\n[[2, 1, 1]]\n",
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
