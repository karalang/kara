//! The gap porting apps/service/yamux_server found (B-2026-11-27-60): a
//! Rust `thread::spawn` inside a loop, its handle pushed onto a `Vec`, got
//! no fix, since a spawn's group was only made for a statement of the
//! function's own body. The group is now made before the statement of the
//! body that holds the spawn, and later spawns in the same body share it.

use std::path::PathBuf;
use std::process::{Command, Output};

fn scratch(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-yamux-gaps-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    (dir, file)
}

fn karac(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

/// `karac fix` twice (the second pass must find nothing), then the fixed
/// text and its run.
fn fixed(tag: &str, src: &str) -> (String, String) {
    let (dir, file) = scratch(tag, src);
    let first = karac(&dir, &["fix", "main.kara"]);
    let second = karac(&dir, &["fix", "main.kara"]);
    let text = std::fs::read_to_string(&file).unwrap();
    let run = karac(&dir, &["__mir-run", "main.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    let again = String::from_utf8_lossy(&second.stdout).into_owned()
        + &String::from_utf8_lossy(&second.stderr);
    assert!(
        again.contains("no fixable diagnostics"),
        "second pass: {again}\n{text}"
    );
    assert!(
        run.status.success(),
        "fix: {}\n{text}\nrun: {}",
        String::from_utf8_lossy(&first.stderr),
        String::from_utf8_lossy(&run.stderr)
    );
    (text, String::from_utf8_lossy(&run.stdout).into_owned())
}

#[test]
fn fix_spawn_pushed_in_a_loop_gets_a_group_before_the_loop() {
    let src = r#"
use std::thread;

fn work(name: String, i: usize) -> usize {
    name.len() * 10 + i
}

fn main() {
    let mut handles = Vec::new();
    for i in 0..3 {
        let name = format!("task-{}", i);
        handles.push(thread::spawn(move || work(name, i)));
    }
    let mut total = 0;
    for h in handles {
        total += h.join().unwrap();
    }
    println!("{}", total);
}
"#;
    let (text, out) = fixed("loop", src);
    assert!(
        text.contains("    let group = TaskGroup.new();\n    for i in 0..3 {"),
        "{text}"
    );
    assert!(text.contains("handles.push(group.spawn("), "{text}");
    assert_eq!(out, "183\n");
}

#[test]
fn fix_later_spawns_in_a_body_share_the_first_group() {
    let src = r#"
use std::thread;

fn main() {
    let a = thread::spawn(|| 1);
    let words = vec!["x", "yy"];
    let b = thread::spawn(move || words.len());
    let c = std::thread::spawn(|| 3);
    println!("{} {} {}", a.join().unwrap(), b.join().unwrap(), c.join().unwrap());
}
"#;
    let (text, out) = fixed("share", src);
    assert_eq!(text.matches("TaskGroup.new()").count(), 1, "{text}");
    assert!(
        text.contains("    let group = TaskGroup.new();\n    let a = group.spawn(|| 1);"),
        "{text}"
    );
    assert!(text.contains("let b = group.spawn("), "{text}");
    assert!(text.contains("let c = group.spawn(|| 3);"), "{text}");
    assert_eq!(out, "1 2 3\n");
}

#[test]
fn fix_spawn_in_a_nested_block_of_a_loop_gets_the_group_outside() {
    let src = r#"
use std::thread;

fn main() {
    let mut sums = Vec::new();
    for i in 1..4 {
        if i % 2 == 1 {
            sums.push(thread::spawn(move || i * i));
        }
    }
    let got: Vec<String> = sums.into_iter().map(|h| h.join().unwrap().to_string()).collect();
    println!("{}", got.join(","));
}
"#;
    let (text, out) = fixed("nested", src);
    assert!(
        text.contains("    let group = TaskGroup.new();\n    for i in 1..4 {"),
        "{text}"
    );
    assert_eq!(out, "1,9\n");
}
