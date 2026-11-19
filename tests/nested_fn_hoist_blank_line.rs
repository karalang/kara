//! `karac fix` moves Rust's nested `fn` to the top level. When the `fn`
//! opened its block, the blank line after it stayed behind, so the block
//! opened on a blank line (kata 1627's `impl Solution`); the blank line
//! now goes with it. A blank line between two statements stays.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn fixed(tag: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-hoistblank-{tag}-{}", std::process::id()));
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
    assert!(
        check.status.success(),
        "{after}\n{}",
        String::from_utf8_lossy(&check.stdout)
    );
    let _ = std::fs::remove_dir_all(&dir);
    after
}

#[test]
fn a_nested_fn_opening_its_block_takes_the_blank_line_after_it() {
    let src = "fn count(n: i64) -> i64 {\n    fn step(k: i64) -> i64 {\n        k * 7\n    }\n\n    let m = n + 1;\n    step(n) + step(m)\n}\n\nfn main() {\n    println(count(2));\n}\n";
    let after = fixed("opens", src);
    assert!(
        after.contains("fn count(n: i64) -> i64 {\n    let m = n + 1;\n"),
        "{after}"
    );
}

#[test]
fn a_nested_fn_between_statements_leaves_one_blank_line() {
    let src = "fn count(n: i64) -> i64 {\n    let m = n + 1;\n\n    fn step(k: i64) -> i64 {\n        k * 7\n    }\n\n    step(n) + step(m)\n}\n\nfn main() {\n    println(count(2));\n}\n";
    let after = fixed("between", src);
    assert!(
        after.contains("    let m = n + 1;\n\n    step(n) + step(m)\n"),
        "{after}"
    );
}

#[test]
fn a_nested_fn_right_after_a_statement_keeps_the_blank_line_after_it() {
    let src = "fn count(n: i64) -> i64 {\n    let m = n + 1;\n    fn step(k: i64) -> i64 {\n        k * 7\n    }\n\n    step(n) + step(m)\n}\n\nfn main() {\n    println(count(2));\n}\n";
    let after = fixed("after", src);
    assert!(
        after.contains("    let m = n + 1;\n\n    step(n) + step(m)\n"),
        "{after}"
    );
}
