//! `[1].iter().chain(v.iter().map(|t| t + 1))` over a `Vec[i32]`: the
//! receiver yields `ref` items of an open literal and the argument `i32`
//! ones. The literal was read at its default, so the error named
//! `ref i64` against `i32` and offered no fix; it now takes the other
//! side's items and the error offers `.cloned()` on the receiver, after
//! which the program checks and runs (kata 1629).

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

#[test]
fn a_literal_iter_chained_with_owned_items_is_fixed_with_cloned() {
    let dir = std::env::temp_dir().join(format!("karac-litchain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    let src = "fn main() {\n    let v: Vec[i32] = [3, 5];\n    let d: Vec[i32] = [1].iter().chain(v.iter().map(|t| t + 1)).collect();\n    println(f\"{d}\");\n}\n";
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    let before = karac(&["check", p]);
    let text = String::from_utf8_lossy(&before.stdout).into_owned()
        + &String::from_utf8_lossy(&before.stderr);
    assert!(text.contains("Item = ref i32`"), "{text}");
    assert!(text.contains(".cloned()"), "{text}");
    karac(&["fix", p]);
    let after = std::fs::read_to_string(&f).unwrap();
    assert!(after.contains("[1].iter().cloned().chain("), "{after}");
    let check = karac(&["check", p]);
    assert!(check.status.success(), "{after}");
    let run = karac(&["run", "--interp", p]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "[1, 4, 6]\n");
    let _ = std::fs::remove_dir_all(&dir);
}
