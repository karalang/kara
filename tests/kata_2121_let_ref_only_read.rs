//! Kata 2121's struct arm bound a group's indices as `let p =
//! self.positions;` inside a `ref self` method: a move out of the borrowed
//! receiver. The refusal's one machine fix was `.clone()`, a copy of the
//! whole list on every call, though core-semantics.md §4.6 names `let p =
//! ref self.positions;` as the other fix and nothing after the `let`
//! needed a value of its own. An immutable `let` whose binding is
//! afterwards only read (a receiver, an index or field base, an operand,
//! an interpolation, a bare `for`'s collection) now gets the `ref` borrow
//! as its fix; any other use keeps `.clone()`.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// Writes `src`, runs `karac fix` on it, and returns the fixed text.
fn fixed(tag: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-k2121-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let file = path.to_str().unwrap();
    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let text = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    text
}

#[test]
fn kata_2121_only_read_field_gets_ref() {
    let text = fixed(
        "read",
        "struct Group {
    positions: Vec[i64],
}

impl Group {
    fn spread(ref self) -> i64 {
        let p = self.positions;
        let mut total = 0;
        for x in p {
            total += x;
        }
        f\"{p.len()}\".len() + total + p[0] + p.first().unwrap_or(0)
    }
}

fn main() {
    let g = Group { positions: [3, 4, 5].to_vec() };
    println(g.spread());
}
",
    );
    assert!(text.contains("let p = ref self.positions;"), "{text}");
    let out = karac::mir::lower::run_source(&text)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "19\n");
}

#[test]
fn kata_2121_field_passed_on_keeps_clone() {
    let text = fixed(
        "passed",
        "struct Group {
    positions: Vec[i64],
}

fn keep(v: own Vec[i64]) -> i64 {
    v.len()
}

impl Group {
    fn spread(ref self) -> i64 {
        let p = self.positions;
        keep(p)
    }
}

fn main() {
    let g = Group { positions: [3, 4, 5].to_vec() };
    println(g.spread());
}
",
    );
    assert!(text.contains("let p = self.positions.clone();"), "{text}");
}

#[test]
fn kata_2121_field_returned_keeps_clone() {
    let text = fixed(
        "returned",
        "struct Group {
    positions: Vec[i64],
}

impl Group {
    fn copy(ref self) -> Vec[i64] {
        let p = self.positions;
        p
    }
}

fn main() {
    let g = Group { positions: [3, 4, 5].to_vec() };
    println(g.copy().len());
}
",
    );
    assert!(text.contains("let p = self.positions.clone();"), "{text}");
}

#[test]
fn kata_2121_field_moved_by_into_iter_keeps_clone() {
    let text = fixed(
        "into-iter",
        "struct Group {
    positions: Vec[i64],
}

impl Group {
    fn total(ref self) -> i64 {
        let p = self.positions;
        p.into_iter().sum()
    }
}

fn main() {
    let g = Group { positions: [3, 4, 5].to_vec() };
    println(g.total());
}
",
    );
    assert!(text.contains("let p = self.positions.clone();"), "{text}");
}
