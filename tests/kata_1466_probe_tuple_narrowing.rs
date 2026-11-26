//! B-2026-11-26-85: a table lookup by a tuple literal narrowed a wider
//! number into the key's slot unchecked. Kata 1466's stack arm held its
//! cities in a `Vec[i64]` (`[0].to_vec()`, the `i32`s it pushes widened)
//! and asked `given.contains((city, next))` of a `Set[(i32, i32)]`: check
//! passed, the interpreter answered right, and the compiled lanes read the
//! `i64` as an `i32` and found nothing. The needle's numbers are now
//! checked against the key's slots as a pushed tuple's are, and the
//! narrowing is refused as it is for a bare needle or `Vec.contains`.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-probe-tuple-narrowing-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

/// `karac check`'s success and output, and `karac __mir-run`'s stdout.
fn check_and_run(tag: &str, src: &str) -> (bool, String, String) {
    let (dir, path) = fixture(tag, src);
    let check = karac().arg("check").arg(&path).output().unwrap();
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        check.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        ),
        String::from_utf8_lossy(&run.stdout).into_owned(),
    )
}

const NARROW: &str = "implicit coercion from 'i64' to 'i32' would narrow";

/// Kata 1466's form: a city popped from a `Vec[i64]` stack, looked up
/// with an `i32` neighbour in a `Set[(i32, i32)]`.
#[test]
fn a_set_lookup_by_a_tuple_refuses_a_narrowed_number() {
    let src = "fn main() {
    let mut given: Set[(i32, i32)] = Set.new();
    given.insert((0, 1));
    let mut stack = [0].to_vec();
    let next: i32 = 1;
    stack.push(next);
    let city = stack[0];
    println(f\"{given.contains((city, next))}\");
}
";
    let (ok, diags, _) = check_and_run("set", src);
    assert!(!ok, "check accepted a narrowed tuple needle:\n{diags}");
    assert!(diags.contains(NARROW), "{diags}");
    assert!(diags.contains("given.contains((city, next))"), "{diags}");
}

/// The other lookups by key: `Map.get`, `contains_key` and `remove`,
/// `SortedSet.contains`, and `Set.remove`, each with the `i64` in the
/// tuple's second slot.
#[test]
fn every_lookup_by_a_tuple_key_refuses_a_narrowed_number() {
    let src = "fn main() {
    let wide: i64 = 1;
    let mut m: Map[(i32, i32), i32] = Map.new();
    m.insert((0, 1), 5);
    let mut s: Set[(i32, i32)] = Set.new();
    s.insert((0, 1));
    let mut t: SortedSet[(i32, i32)] = SortedSet.new();
    t.insert((0, 1));
    println(f\"{m.get((0, wide)).is_some()} {m.contains_key((0, wide))}\");
    println(f\"{t.contains((0, wide))} {s.remove((0, wide))} {m.remove((0, wide)).is_some()}\");
}
";
    let (ok, diags, _) = check_and_run("keys", src);
    assert!(!ok, "check accepted a narrowed tuple key:\n{diags}");
    assert_eq!(diags.matches(NARROW).count(), 5, "{diags}");
}

/// A tuple needle of the key's own types, its literals, a widening into a
/// wider slot and the narrowing written out with `as` all check, and look
/// up what they name.
#[test]
fn a_tuple_needle_of_the_keys_types_still_checks() {
    let src = "fn main() {
    let mut given: Set[(i32, i32)] = Set.new();
    given.insert((0, 1));
    let city: i64 = 0;
    let next: i32 = 1;
    let mut wide: Set[(i64, i64)] = Set.new();
    wide.insert((0, 1));
    let m: Map[(i32, i32), i32] = [((0, 1), 7)].into_iter().collect();
    println(f\"{given.contains((city as i32, next))} {given.contains((0, 1))} {given.contains((0, 2))}\");
    println(f\"{wide.contains((city, next))} {m.get((0, next)) ?? 0}\");
}
";
    let (ok, diags, out) = check_and_run("clean", src);
    assert!(ok, "expected a clean check, got:\n{diags}");
    assert_eq!(out, "true true false\ntrue 7\n");
}
