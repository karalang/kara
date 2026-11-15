//! B-2026-11-15-34: `successors(Some(x), |lap| Some(lap * 2))` with `x` a
//! `ref i64` from `v.iter()` fixes the item type to `ref i64`, so the
//! closure's new `i64` cannot be its result. The MIR borrow check refused it
//! as E0509 on the closure, naming neither the seed nor the item type, with
//! no fix. The refusal is now at the seed, names the item type, and carries
//! the deref fix (`Some(*x)`).

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

#[test]
fn a_borrowed_successors_seed_is_named_and_fixed_with_a_deref() {
    let src = "fn main() {
    let v = [1, 3].to_vec();
    for x in v.iter() {
        let w: Vec[i64] = successors(Some(x), |lap| Some(lap * 2)).take(3).collect();
        println(f\"{w}\");
    }
}
";
    let dir = std::env::temp_dir().join(format!("karac-succseed-{}", std::process::id()));
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
        text.contains("the seed makes `successors`'s item a `ref i64`"),
        "{text}"
    );
    assert!(text.contains("`Some(*x)`"), "{text}");
    assert!(!text.contains("E0509"), "{text}");
    let fix = karac(&["fix", p]);
    assert!(fix.status.success());
    let after = std::fs::read_to_string(&f).unwrap();
    assert!(
        after.contains("successors(Some(*x), |lap| Some(lap * 2))"),
        "{after}"
    );
    let run = karac(&["run", "--interp", p]);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "[1, 2, 4]\n[3, 6, 12]\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_successors_walk_that_returns_borrows_keeps_its_ref_seed() {
    let src = "struct Node {
    val: i64,
    next: Option[Node],
}

fn main() {
    let head = Node { val: 1, next: Some(Node { val: 2, next: None }) };
    let vals: Vec[i64] = successors(Some(ref head), |n| n.next.as_ref()).map(|n| n.val).collect();
    println(f\"{vals}\");
}
";
    let dir = std::env::temp_dir().join(format!("karac-succwalk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    let check = karac(&["check", p]);
    let text = String::from_utf8_lossy(&check.stdout).into_owned();
    assert!(!text.contains("the seed makes"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
