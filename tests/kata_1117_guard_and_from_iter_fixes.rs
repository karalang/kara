//! Kata 1117's polling arm resets the molecule under its lock with
//! `g.value = Molecule { .. }`, and its harness builds a string with
//! `String.from_iter(atoms.iter())` over a `Vec[char]`.
//!
//! - `g.value` is the guard's `mut ref` field, and plain `=` does not
//!   write through a `mut ref` (design.md § Auto-deref): a local's
//!   `r = v` was refused with the `*r = v` fix, but the field was asked
//!   for `let mut g` and then written through. It is now refused the same
//!   way, with `*g.value = v`.
//! - `from_iter`'s own bound wants `char` items where `iter()` yields
//!   `ref char`: the error said `.cloned()` but offered no fix, which a
//!   method whose bound names its receiver already had.

use std::process::{Command, Stdio};

/// `src` after `karac fix` until nothing changes, and its `check` stderr.
fn fixed(tag: &str, src: &str) -> (String, String) {
    let path =
        std::env::temp_dir().join(format!("karac-kata1117-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    for _ in 0..6 {
        let before = std::fs::read_to_string(&path).unwrap();
        let _ = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["fix", path.to_str().unwrap()])
            .stdin(Stdio::null())
            .output()
            .expect("karac");
        if std::fs::read_to_string(&path).unwrap() == before {
            break;
        }
    }
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let out = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    (out, String::from_utf8_lossy(&check.stderr).into_owned())
}

fn clean(check: &str) -> bool {
    check.contains("All checks passed") || check.is_empty()
}

#[test]
fn kata_1117_assigning_a_guards_value_writes_through_with_a_star() {
    let src = "import std.sync.Mutex;

struct Molecule {
    hydrogens: i64,
    out: i64,
}

fn main() {
    let m: Mutex[Molecule] = Mutex.new(Molecule { hydrogens: 2, out: 3 });
    {
        let g = m.lock();
        g.value = Molecule { hydrogens: 0, out: 0 };
    }
    let g = m.lock();
    println(f\"{g.value.hydrogens} {g.value.out}\");
}
";
    let (text, check) = fixed("guard", src);
    assert!(
        text.contains("*g.value = Molecule { hydrogens: 0, out: 0 };"),
        "{text}"
    );
    assert!(
        text.contains("        let g = m.lock();\n        *g.value"),
        "{text}"
    );
    assert!(clean(&check), "{check}\n{text}");
    let out = karac::mir::lower::run_source(&text)
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .output;
    assert_eq!(out, "0 0\n");
}

#[test]
fn kata_1117_from_iter_over_borrowed_items_gets_cloned() {
    let src = "fn main() {
    let atoms: Vec[char] = ['H', 'O', 'H'];
    let s = String.from_iter(atoms.iter());
    println(s);
}
";
    let (text, check) = fixed("from-iter", src);
    assert!(
        text.contains("String.from_iter(atoms.iter().cloned())"),
        "{text}"
    );
    assert!(clean(&check), "{check}\n{text}");
    let out = karac::mir::lower::run_source(&text)
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .output;
    assert_eq!(out, "HOH\n");
}
