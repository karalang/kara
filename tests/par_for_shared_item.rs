//! The item of a `par for` enters every iteration's task, so its type must
//! be cross-task safe (core semantics §11.3). A `shared` element, iterated by
//! view (`v.iter()`), by value (`for x in v`) or consumed (`v.into_iter()`),
//! passed `karac check`, and the iterations then counted the handle's
//! references from many workers at once: ASan showed a heap-use-after-free
//! and a double free, TSan a race on the count (B-2026-11-21-15). The item
//! pattern's bindings are not captures, so only the capture walk ran and the
//! item's type was never asked. A `sync` element and a plain one still pass.

use std::process::{Command, Stdio};

fn check(tag: &str, src: &str) -> (bool, String) {
    let path = std::env::temp_dir().join(format!(
        "karac-par-for-shared-item-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout),
    )
}

fn shared_item(iterable: &str, body: &str) -> String {
    format!(
        "shared struct S {{ n: i64, mut xs: Vec[i64] }}\n\
         fn main() {{\n    \
         let s = S {{ n: 1, xs: Vec.new() }};\n    \
         let mut v: Vec[S] = Vec.new();\n    \
         let mut i = 0;\n    \
         while i < 64 {{ v.push(s); i = i + 1; }}\n    \
         let r = par for x in {iterable} {{ {body} }};\n    \
         println(r.len());\n}}\n"
    )
}

fn assert_refused(tag: &str, src: &str, what: &str) {
    let (ok, text) = check(tag, src);
    assert!(!ok, "{tag}: check accepted a shared par for item:\n{text}");
    assert!(text.contains("E_NOT_CROSS_TASK"), "{tag}:\n{text}");
    assert!(
        text.contains(&format!("the item `x` of this `par for` ({what})")),
        "{tag}:\n{text}"
    );
    assert!(
        text.contains("declare `S` as a `sync` type"),
        "{tag}:\n{text}"
    );
}

#[test]
fn a_shared_item_by_view_is_refused() {
    assert_refused(
        "iter",
        &shared_item("v.iter()", "x.xs.push(1); 1"),
        "of type `ref S`, which holds the `shared` type `S`",
    );
}

#[test]
fn a_shared_item_by_value_is_refused() {
    assert_refused(
        "value",
        &shared_item("v", "let y = x.clone(); y.n"),
        "a `shared` handle of type `S`",
    );
}

#[test]
fn a_consumed_shared_item_is_refused() {
    assert_refused(
        "into-iter",
        &shared_item(
            "v.into_iter()",
            "let mut k: Vec[S] = Vec.new(); k.push(x); k.len()",
        ),
        "a `shared` handle of type `S`",
    );
}

/// The refusal names the `sync` form of a type with `mut` fields.
#[test]
fn a_shared_item_refusal_shows_the_sync_form() {
    let (_, text) = check("note", &shared_item("v.iter()", "x.xs.push(1); 1"));
    assert!(
        text.contains("+ sync struct S { n: i64, xs: Mutex[Vec[i64]] }"),
        "{text}"
    );
}

/// A `sync` element and a plain one may be a `par for`'s item, and the
/// program runs.
#[test]
fn a_sync_or_plain_item_still_checks_and_runs() {
    let src = "sync struct S { n: i64 }\n\
               fn main() {\n    \
               let s = S { n: 1 };\n    \
               let mut v: Vec[S] = Vec.new();\n    \
               let mut i = 0;\n    \
               while i < 8 { v.push(s); i = i + 1; }\n    \
               let r = par for x in v.iter() { x.n };\n    \
               let w: Vec[i64] = [1, 2, 3];\n    \
               let q = par for y in w.iter() { y * 2 };\n    \
               let mut t = 0;\n    \
               for c in r.iter() { t = t + c; }\n    \
               for c in q.iter() { t = t + c; }\n    \
               println(t);\n}\n";
    let (ok, text) = check("sync", src);
    assert!(ok, "{text}");
    let path = std::env::temp_dir().join(format!(
        "karac-par-for-sync-item-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["run", "--interp"])
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "20\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A generic item enters the iterations only where its type parameter is
/// bounded `CrossTask`, as a captured value of one does: a caller may give
/// an unbounded `T` a `shared` handle.
#[test]
fn a_generic_item_needs_a_cross_task_bound() {
    let body = |bound: &str| {
        format!(
            "fn total[T: {bound}](xs: ref Vec[T], f: Fn(T) -> i64 + CrossTask) -> Vec[i64] {{\n    \
             let r: Vec[i64] = par for x in xs.iter() {{ f(*x) }};\n    \
             r\n}}\n\
             fn main() {{\n    \
             let v: Vec[i64] = [1, 2, 3];\n    \
             println(total(v, |x| x * 2).len());\n}}\n"
        )
    };
    let (ok, text) = check("generic-unbounded", &body("Copy"));
    assert!(!ok, "{text}");
    assert!(
        text.contains("the item `x` of a `par for` (type `ref T`) cannot cross a task boundary")
            && text.contains("declare `T: CrossTask`"),
        "{text}"
    );
    let (ok, text) = check("generic-bounded", &body("Copy + CrossTask"));
    assert!(ok, "{text}");
}
