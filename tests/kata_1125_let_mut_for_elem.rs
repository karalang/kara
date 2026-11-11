//! Kata 1125's Map arm grows teams with `for (m, team) in best.clone() {
//! let mut next = team; next.push(p); best[grown] = next; }`: Rust's way
//! to change a copy of an element it owns. A bare `for` borrows (§4.6), so
//! `next` is a `ref` to the element. `karac fix` put a `.clone()` at the
//! move, `best[grown] = next.clone()`, which left `next.push(p)` writing
//! through the `ref`: an error nothing fixed. The move of such a `let mut`
//! copy now takes the loop's `.into_iter()` when the collection is not used
//! after the loop, as a move of `team` itself does, and otherwise a
//! `.clone()` where `next` is bound, which the write through it asks for too.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kata_1125_let_mut_for_elem_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The text `karac fix` leaves once it has nothing left to apply, and
/// what the program prints.
fn mend(name: &str, src: &str) -> (String, String) {
    let dir = scratch(name);
    let file = dir.join("m.kara");
    std::fs::write(&file, src).unwrap();
    for _ in 0..6 {
        karac(&dir, &["fix", "m.kara"]);
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (_, out) = karac(&dir, &["__mir-run", "m.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (text, out)
}

const OVER_A_TEMPORARY: &str = "fn grow(best: Map[i64, Vec[i64]], p: i64) -> Map[i64, Vec[i64]] {
    let mut out: Map[i64, Vec[i64]] = Map.new();
    for (m, team) in best.clone() {
        let mut next = team;
        next.push(p);
        out[m + 1] = next;
    }
    out
}

fn main() {
    let mut best: Map[i64, Vec[i64]] = Map.new();
    best[0] = [7];
    let grown = grow(best, 3);
    println(f\"{grown[1]} {best[0]}\");
}
";

#[test]
fn a_let_mut_copy_of_a_for_element_over_a_temporary_takes_into_iter() {
    let (text, out) = mend("temporary", OVER_A_TEMPORARY);
    assert!(
        text.contains("for (m, team) in best.clone().into_iter() {"),
        "{text}"
    );
    assert!(text.contains("let mut next = team;"), "{text}");
    assert!(text.contains("out[m + 1] = next;"), "{text}");
    assert_eq!(out, "[7, 3] [7]\n", "{text}");
}

const OVER_A_BORROWED_MAP: &str =
    "fn grow(best: Map[i64, Vec[i64]], p: i64) -> Map[i64, Vec[i64]] {
    let mut out: Map[i64, Vec[i64]] = Map.new();
    for (m, team) in best {
        let mut next = team;
        next.push(p);
        out[m + 1] = next;
    }
    out[best.len() + 5] = [];
    out
}

fn main() {
    let mut best: Map[i64, Vec[i64]] = Map.new();
    best[0] = [7];
    let grown = grow(best, 3);
    println(f\"{grown[1]} {grown.len()} {best[0]}\");
}
";

#[test]
fn a_let_mut_copy_of_a_for_element_over_a_borrowed_map_clones_where_bound() {
    let (text, out) = mend("borrowed", OVER_A_BORROWED_MAP);
    assert!(text.contains("let mut next = team.clone();"), "{text}");
    assert!(text.contains("out[m + 1] = next;"), "{text}");
    assert!(text.contains("for (m, team) in best {"), "{text}");
    assert_eq!(out, "[7, 3] 2 [7]\n", "{text}");
}

#[test]
fn a_let_mut_copy_of_a_for_element_written_through_names_the_copy() {
    let src = "fn main() {
    let rows: Vec[Vec[i64]] = [[1], [2, 3]];
    for row in rows {
        let mut longer = row;
        longer.push(0);
        println(f\"{longer}\");
    }
    println(f\"{rows}\");
}
";
    let dir = scratch("message");
    std::fs::write(dir.join("m.kara"), src).unwrap();
    let (_, json) = karac(&dir, &["check", "--output=json", "m.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        json.contains("Write `.clone()` where it is bound to change a copy of its own"),
        "{json}"
    );
    let (text, out) = mend("written", src);
    assert!(text.contains("let mut longer = row.clone();"), "{text}");
    assert_eq!(out, "[1, 0]\n[2, 3, 0]\n[[1], [2, 3]]\n", "{text}");
}
