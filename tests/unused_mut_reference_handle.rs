//! `unused_mut` over a binding that holds a reference handle
//! (B-2026-11-10-19): a write through a `mut ref T` of a non-`Copy` `T`,
//! by a `mut` marker, a field store or a mutating method, lands in the
//! referent and needs no `let mut`, so the lint warns and `karac fix` drops
//! it with the output unchanged. A rebind keeps its `mut`, and so does a
//! `let mut` of a `mut ref` to a `Copy` value or of a `ref`, which makes the
//! binding a copy whose writes are its own.

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
    let dir = std::env::temp_dir().join(format!("karac-unused-mut-ref-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

fn warned(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.starts_with("warning[unused_mut]"))
        .filter_map(|l| l.split('`').nth(1).map(str::to_string))
        .collect()
}

#[test]
fn a_let_mut_of_a_mut_ref_written_only_through_it_is_warned_and_fixed() {
    let path = scratch(
        "handle.kara",
        "struct Node { n: i64, kids: Vec[i64] }
fn grow(x: mut ref Node) { x.n += 1; }
fn main() {
    let mut m: Map[String, Node] = Map.new();
    let mut a = m.entry(\"a\").or_insert(Node { n: 0, kids: Vec.new() });
    grow(mut a);
    let mut b = m.entry(\"b\").or_insert(Node { n: 0, kids: Vec.new() });
    b.n += 5;
    b.kids.push(1);
    let mut c = m.entry(\"c\").or_insert(Node { n: 0, kids: Vec.new() });
    c = m.entry(\"d\").or_insert(Node { n: 3, kids: Vec.new() });
    c.n += 1;
    println(f\"{m[\"a\"].n} {m[\"b\"].n} {m[\"b\"].kids.len()} {m[\"c\"].n} {m[\"d\"].n}\");
}
",
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    assert_eq!(warned(&text(&check)), ["a", "b"], "{}", text(&check));
    let before = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&before.stdout),
        "1 5 1 0 4\n",
        "{}",
        text(&before)
    );
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{}", text(&fix));
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(
        src.contains("let a = m.entry") && src.contains("let b = m.entry"),
        "{src}"
    );
    assert!(src.contains("let mut c = m.entry"), "{src}");
    let check = karac(&["check"], &path);
    assert!(warned(&text(&check)).is_empty(), "{}", text(&check));
    let after = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&after.stdout),
        "1 5 1 0 4\n",
        "{}",
        text(&after)
    );
}

#[test]
fn a_let_mut_that_copies_through_a_reference_keeps_its_mut() {
    let path = scratch(
        "copies.kara",
        "fn bump(x: mut ref i64) { x += 1; }
fn first(v: ref Array[i64, 3]) -> i64 {
    let mut left = v;
    left[0] = 9;
    left[0] + v[0]
}
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut r = m.entry(\"a\").or_insert(0);
    bump(mut r);
    let mut q = m.entry(\"b\").or_insert(0);
    q += 2;
    println(f\"{m[\"a\"]} {m[\"b\"]} {r} {q} {first([1, 2, 3])}\");
}
",
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    assert!(warned(&text(&check)).is_empty(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "0 0 1 2 10\n",
        "{}",
        text(&run)
    );
}

#[test]
fn a_shared_binding_lent_whole_as_mut_keeps_its_mut() {
    let path = scratch(
        "shared_lend.kara",
        "shared struct T { v: i64, mut next: Option[T] }
fn advance(t: mut ref T) { let n = T { v: t.v + 1, next: None }; *t = n; }
fn main() {
    let head = T { v: 1, next: None };
    let mut tail = head;
    advance(mut tail);
    let mut cur = T { v: 5, next: None };
    cur.next = Some(T { v: 6, next: None });
    println(f\"{tail.v} {cur.v}\");
}
",
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    assert_eq!(warned(&text(&check)), ["cur"], "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "2 5\n",
        "{}",
        text(&run)
    );
}
