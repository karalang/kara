//! B-2026-11-19-43: inside a generic item a projection of its own
//! parameter (`I.Item` under `I: Iterator`) is as rigid as the parameter:
//! only an `I.Item` is an `I.Item`, unless a `where I.Item = T` equality
//! fixes it. It unified with any type, so `let s: bool = x` with
//! `x: I.Item` checked and the instance failed as invalid MIR ("assigning
//! i64 to _9 of type bool").

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-rigid-projection-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

fn refused(name: &str, src: &str, at: &str) {
    let path = scratch(name, src);
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    assert!(
        text(&check).contains(&format!(
            "{at}: expected 'bool', found 'I.Item': inside this item `I.Item` stands for \
             whatever type the caller picks"
        )),
        "{}",
        text(&check)
    );
}

#[test]
fn a_projection_value_is_not_a_concrete_type_after_next() {
    refused(
        "next.kara",
        "fn count[I: Iterator](it: own I) -> i64 {
    let mut it = it;
    while let Some(x) = it.next() {
        let s: bool = x;
    }
    0
}
fn main() { println(count([1, 2, 3].into_iter())); }
",
        "4:23",
    );
}

#[test]
fn a_projection_value_is_not_a_concrete_type_in_a_for() {
    refused(
        "for.kara",
        "fn count[I: Iterator](iter: own I) -> i64 {
    for x in iter {
        let s: bool = x;
    }
    0
}
fn main() { println(count([1, 2, 3].into_iter())); }
",
        "3:23",
    );
}

#[test]
fn a_projection_is_itself_or_what_a_where_clause_equates_it_with() {
    let path = scratch(
        "fixed.kara",
        "fn first[I: Iterator](iter: own I) -> I.Item where I.Item: Default {
    let mut it = iter;
    let y: I.Item = match it.next() {
        Some(x) => x,
        None => I.Item.default(),
    };
    y
}
fn total[I: Iterator](iter: own I) -> i64 where I.Item = i64 {
    let mut t: i64 = 0;
    for x in iter {
        let v: i64 = x;
        t = t + v;
    }
    t
}
fn main() {
    println(first([4, 5].into_iter()));
    println(total([1, 2, 3].into_iter()));
}
",
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "4\n6\n",
        "{}",
        text(&run)
    );
}
