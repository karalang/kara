//! Kata 2046: writing a field of a `shared struct` that was not declared
//! `mut` (`node.val = v;` in a linked-list arm that rewrites values in
//! place) was refused with "shared struct field 'ListNode.val' is not
//! declared mut" and no fix. A shared struct declared in the program now
//! gets the fix at its declaration, `mut` before the field's name, once
//! however many writes there are; a `sync struct` field, which may never
//! be `mut`, keeps its own message and no fix.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn kata_2046_shared_field_write_fixes_to_a_mut_field() {
    let src = "shared struct ListNode {
    val: i64,
    mut next: Option[ListNode],
}

fn main() {
    let tail = ListNode { val: 2, next: None };
    let head = ListNode { val: 1, next: Some(tail) };
    head.val = 5;
    tail.val += 4;
    println(f\"{head.val} {tail.val}\");
}
";
    let file = write("k2046-fix", src);
    let check = karac(&["check", &file]);
    let msg = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(
        msg.contains("shared struct field 'ListNode.val' is not declared mut; declare it `mut val` in `ListNode`"),
        "{msg}"
    );
    let _ = karac(&["fix", &file]);
    let fixed = std::fs::read_to_string(&file).unwrap();
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(
        check.status.success(),
        "{}\n{fixed}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(
        fixed.contains("    mut val: i64,\n"),
        "one `mut`, not two:\n{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "5 6\n");
}

#[test]
fn kata_2046_sync_field_write_has_no_mut_fix() {
    let src = "sync struct Counter {
    limit: i64,
}

fn main() {
    let c = Counter { limit: 3 };
    c.limit = 4;
    println(f\"{c.limit}\");
}
";
    let file = write("k2046-sync", src);
    let dry = karac(&["fix", "--dry-run", &file]);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    let msg = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(msg.contains("cannot be written"), "{msg}");
    let plan = String::from_utf8_lossy(&dry.stdout).into_owned();
    assert!(!plan.contains("`mut `"), "{plan}");
}
