//! Comparing a library lookup with an owned `Option` (`m.get(k) == Some(5)`)
//! is refused, as the library side is an `Option[ref T]` (B-2026-10-12-22).
//! For a `Copy` item the error carries the `.cloned()` that copies the item
//! out, so `karac fix` makes it (B-2026-11-01-10): on either side of `==`
//! and `!=`, after a `Map.get` and a `Vec.get`, and on an ordering. A
//! non-`Copy` item compared with `Some(place)` gets `Some(ref place)`
//! (B-2026-11-04-61); any other non-`Copy` shape gets `.cloned()` too
//! (B-2026-11-07-54). A LITERAL in the constructor takes the viewed
//! element type instead, so `m.get(1) == Some(5)` itself is accepted
//! (B-2026-11-08-22); the fix is for a place.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-option-ref-compare-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn karac_fix_clones_the_viewed_side_of_an_option_compare() {
    let src = "fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1, 5);
    let v: Vec[i64] = vec![5, 6];
    let o: Option[i64] = Some(5);
    let n: i64 = 5;
    let s: i64 = 6;
    println(f\"{m.get(1) == Some(n)} {Some(s) != m.get(1)} {v.get(1) == o.map(|x| x + 1)} {m.get(2) == None}\");
    println(f\"{m.get(1) < o}\");
}
";
    let path = temp("fix", src);
    let file = path.to_str().unwrap();

    let check = karac(&["check", "--output=json", file]);
    let json = String::from_utf8_lossy(&check.stdout);
    assert_eq!(json.matches("\"text\":\".cloned()\"").count(), 4, "{json}");
    assert!(json.contains("`.cloned()` copies the item out"), "{json}");

    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let out = std::fs::read_to_string(&path).unwrap();
    for want in [
        "m.get(1).cloned() == Some(n)",
        "Some(s) != m.get(1).cloned()",
        "v.get(1).cloned() == o.map(",
        "m.get(2) == None",
        "m.get(1).cloned() < o",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        (
            run.status.code(),
            String::from_utf8_lossy(&run.stdout).as_ref()
        ),
        (Some(0), "true true true true\nfalse\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn karac_fix_views_a_some_place_compared_with_a_non_copy_lookup() {
    // B-2026-11-04-61: a non-`Copy` item gets `Some(ref e)`, an
    // `Option[ref T]` built in place, and not the `.clone()` the argument's
    // move would otherwise ask for.
    let src = "struct L { key: String, value: String }
fn has(m: SortedMap[String, String], l: L) -> bool { m.get(l.key) == Some(l.value) }
fn main() {
    let mut m: SortedMap[String, String] = SortedMap.new();
    m.insert(\"zone\".to_string(), \"a\".to_string());
    let s = \"a\".to_string();
    let l = L { key: \"zone\".to_string(), value: \"a\".to_string() };
    println(f\"{has(m, l)} {Some(s) == m.get(\"zone\")} {m.get(\"x\") != Some(s)}\");
}
";
    let path = temp("view", src);
    let file = path.to_str().unwrap();

    let check = karac(&["check", "--output=json", file]);
    let json = String::from_utf8_lossy(&check.stdout);
    assert_eq!(json.matches("\"text\":\"ref \"").count(), 3, "{json}");
    assert!(json.contains("`Some(ref e)` views"), "{json}");
    assert!(!json.contains("borrow_projection_copy"), "{json}");

    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let out = std::fs::read_to_string(&path).unwrap();
    for want in [
        "m.get(l.key) == Some(ref l.value)",
        "Some(ref s) == m.get(",
        "!= Some(ref s)",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        (
            run.status.code(),
            String::from_utf8_lossy(&run.stdout).as_ref()
        ),
        (Some(0), "true true true\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn a_non_copy_item_compare_with_a_temporary_gets_cloned() {
    // B-2026-11-07-54 (design.md § `*` and `ref` in expressions): the
    // look-through is per operand, so `Option[ref T] == Option[T]` stays
    // refused for any `T`, with `.cloned()` on the viewing side as the
    // repair where no `Some(ref e)` view reaches the other side.
    let src = "fn main() {
    let mut ms: Map[i64, String] = Map.new();
    ms.insert(1, \"x\".to_string());
    println(f\"{ms.get(1) == Some(\"x\".to_string())}\");
}
";
    let path = temp("non-copy", src);
    let file = path.to_str().unwrap();
    let check = karac(&["check", "--output=json", file]);
    let json = String::from_utf8_lossy(&check.stdout);
    assert!(
        json.contains("cannot compare 'Option[ref String]' and 'Option[String]'"),
        "{json}"
    );
    assert_eq!(json.matches("\"text\":\".cloned()\"").count(), 1, "{json}");
    assert!(!json.contains("\"text\":\"ref \""), "{json}");
    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let out = std::fs::read_to_string(&path).unwrap();
    assert!(out.contains("ms.get(1).cloned() == Some("), "{out}");
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        (
            run.status.code(),
            String::from_utf8_lossy(&run.stdout).as_ref()
        ),
        (Some(0), "true\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
