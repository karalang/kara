//! B-2026-11-19-89: Rust's turbofish on a type in an expression
//! (`HashSet::<char>::new()`, `std::collections::HashSet::<char>::new()`)
//! was fixed by dropping the list, which lost the element type, or not at
//! all (`E0002`). The fix now keeps the list in brackets, Kāra's spelling:
//! `HashSet[char].new()`.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Runs `karac fix` twice over `src` (the type rename is a second round),
/// then answers the fixed text and what `__mir-run` prints for it.
fn fixed(src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-type-turbofish-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let check = Command::new(karac)
        .arg("check")
        .arg(&f)
        .output()
        .expect("karac");
    let checked = String::from_utf8_lossy(&check.stderr).to_string()
        + &String::from_utf8_lossy(&check.stdout);
    assert!(!checked.contains("E0002"), "{checked}");
    for _ in 0..2 {
        Command::new(karac)
            .arg("fix")
            .arg(&f)
            .output()
            .expect("karac");
    }
    let text = std::fs::read_to_string(&f).unwrap();
    let run = Command::new(karac)
        .arg("__mir-run")
        .arg(&f)
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        text,
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr),
    )
}

#[test]
fn bare_type_turbofish_keeps_its_list_in_brackets() {
    let (text, out) =
        fixed("fn main() { let mut s = HashSet::<i64>::new(); s.insert(3); println(s.len()); }\n");
    assert!(text.contains("Set[i64].new()"), "{text}");
    assert_eq!(out, "1\n");
}

#[test]
fn std_path_type_turbofish_keeps_its_list_in_brackets() {
    let (text, out) = fixed(
        "fn main() { let mut s = std::collections::HashSet::<char>::new(); s.insert('a'); \
         println(s.len()); }\n",
    );
    assert!(text.contains("Set[char].new()"), "{text}");
    assert!(!text.contains("std"), "{text}");
    assert_eq!(out, "1\n");
}

#[test]
fn nested_type_turbofish_closes_both_lists() {
    let (text, out) = fixed(
        "fn main() { let mut m = std::collections::HashMap::<String, Vec<i64>>::new(); \
         m.insert(\"a\", [1]); println(m.len()); let v = Vec::<Vec<i64>>::new(); \
         println(v.len()); }\n",
    );
    assert!(text.contains("Map[String, Vec[i64]].new()"), "{text}");
    assert!(text.contains("Vec[Vec[i64]].new()"), "{text}");
    assert_eq!(out, "1\n0\n");
}

#[test]
fn type_turbofish_on_a_unit_variant_names_the_enum() {
    let (text, out) = fixed("fn main() { let o = Option::<i64>::None; println(o.is_none()); }\n");
    assert!(text.contains("Option[i64].None"), "{text}");
    assert_eq!(out, "true\n");
}
