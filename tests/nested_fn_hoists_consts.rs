//! B-2026-11-15-9: `karac fix` moves Rust's nested `fn` to the top level,
//! and a Rust nested `fn` sees the items of the block around it, so a
//! `const MOD` the body declared above it was left behind and the moved
//! `fn` named an undefined `MOD`. The `const` now moves with it; a
//! top-level `const` of the same name refuses the move.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn fixed(tag: &str, src: &str) -> (String, std::process::Output, String) {
    let dir = std::env::temp_dir().join(format!("karac-hoistconst-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    for _ in 0..4 {
        karac(&["fix", p]);
    }
    let after = std::fs::read_to_string(&f).unwrap();
    let check = karac(&["check", p]);
    let run = karac(&["run", "--interp", p]);
    let _ = std::fs::remove_dir_all(&dir);
    (
        after,
        check,
        String::from_utf8_lossy(&run.stdout).into_owned(),
    )
}

const SRC: &str = "fn count(n: i64) -> i64 {\n    const MOD: i64 = 1000;\n    fn step(k: i64) -> i64 {\n        k * 7 % MOD\n    }\n    step(n) + step(n + 1)\n}\n\nfn main() {\n    println(count(200));\n}\n";

#[test]
fn a_nested_fn_moves_with_the_block_const_it_names() {
    let (after, check, out) = fixed("moves", SRC);
    assert!(
        check.status.success(),
        "{after}\n{}",
        String::from_utf8_lossy(&check.stdout)
    );
    assert!(
        after.starts_with("const MOD: i64 = 1000;\n\nfn step(k: i64) -> i64 {"),
        "{after}"
    );
    assert_eq!(after.matches("const MOD").count(), 1, "{after}");
    assert_eq!(out, "807\n");
}

#[test]
fn a_top_level_const_of_the_same_name_refuses_the_move() {
    let src = format!("const MOD: i64 = 5;\n\n{SRC}");
    let (after, check, _) = fixed("clash", &src);
    assert!(!check.status.success(), "{after}");
    assert!(after.contains("    fn step(k: i64) -> i64 {"), "{after}");
}
