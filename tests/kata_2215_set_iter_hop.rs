//! Kata 2215 (find the difference of two arrays), the Rust template:
//! B-2026-11-17-126. Rust's `s1.difference(&s2).cloned().collect()` chains
//! on the iterator Rust's set operations give; Kāra's `difference` gives a
//! set, and the check said only "no method 'cloned' on type 'Set'", with no
//! fix, while a `SortedSet`'s `.map(..)` read as a typo of `max`. The
//! iterator adaptors and terminals on a set or map now name the `.iter()`
//! hop, or `.into_iter()` on a call's result, as `Vec`'s do, and `karac fix`
//! inserts it.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(tag: &str, src: &str, args: &[&str]) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2215-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(Path::new(&dir))
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let fixed = std::fs::read_to_string(dir.join("cell.kara")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        fixed,
    )
}

const DIFFERENCE: &str = "fn main() {\n    let s1: Set[i64] = [1, 2, 3].iter().collect();\n    let s2: Set[i64] = [2, 4].iter().collect();\n    let mut only: Vec[i64] = s1.difference(s2).cloned().collect();\n    only.sort();\n    println(f\"{only}\");\n}";

#[test]
fn kata_2215_set_difference_cloned_gets_into_iter() {
    let (ok, out, _) = karac("help", DIFFERENCE, &["check"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("no method 'cloned' on type 'Set': iterator adaptors/terminals require an explicit `.into_iter()`"),
        "{out}"
    );
    assert!(
        out.contains("Kāra's `difference` gives a `Set`, not Rust's iterator"),
        "{out}"
    );
    let (_, _, fixed) = karac("fix", DIFFERENCE, &["fix"]);
    assert!(
        fixed.contains("s1.difference(s2).into_iter().cloned().collect()"),
        "{fixed}"
    );
    let (ok, out, _) = karac("again", &fixed, &["check", "--output=json"]);
    assert!(
        ok && out.contains("\"diagnostics\":[]"),
        "one round leaves nothing: {out}"
    );
    let (ok, out, _) = karac("run", &fixed, &["__mir-run"]);
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "[1, 3]", "{out}");
}

#[test]
fn kata_2215_sorted_collections_take_the_iter_hop() {
    // a named set or map takes `.iter()`, not an edit-distance neighbour
    let src = "fn main() {\n    let s: SortedSet[i64] = [3, 1].iter().collect();\n    let m: SortedMap[i64, i64] = SortedMap.new();\n    let doubled: Vec[i64] = s.map(|x| x * 2).collect();\n    let n = m.filter(|(k, v)| k > v).count();\n    println(f\"{doubled} {n}\");\n}";
    let (ok, out, _) = karac("sorted", src, &["check"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("no method 'map' on type 'SortedSet': iterator adaptors/terminals require an explicit `.iter()` — write `s.iter().map(...)`"),
        "{out}"
    );
    assert!(!out.contains("did you mean 'max'"), "{out}");
    assert!(
        out.contains("no method 'filter' on type 'SortedMap': iterator adaptors/terminals require an explicit `.iter()`"),
        "{out}"
    );
    let (_, _, fixed) = karac("sorted-fix", src, &["fix"]);
    let (ok, out, _) = karac("sorted-run", &fixed, &["__mir-run"]);
    assert!(ok, "{fixed}\n{out}");
    assert_eq!(out.trim(), "[2, 6] 0", "{out}");
}

#[test]
fn kata_2215_sorted_min_max_stay_the_collections() {
    // `min` / `max` are a sorted set's own: no hop
    let src = "fn main() {\n    let s: SortedSet[i64] = [3, 1].iter().collect();\n    println(f\"{s.min().unwrap()} {s.max().unwrap()}\");\n}";
    let (ok, out, _) = karac("minmax", src, &["__mir-run"]);
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "1 3", "{out}");
}
