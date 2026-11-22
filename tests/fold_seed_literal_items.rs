//! B-2026-11-22-3: a `fold` seed of number literals, alone or in a tuple,
//! meets an item of `[1, 2, 3].iter()`, a `ref` to a number literal that
//! nothing has fixed yet. The item reads as its number, as it does into a
//! seed of an already typed list: `(x, p + q)` and `|p, x| x` were refused
//! with expected `i64`, found `ref i64` (kata 2771). The tuple form came
//! with 449dd49566 (B-2026-11-21-57), the lone seed before it.

use std::process::{Command, Stdio};

fn run(name: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-fold-seed-items-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_tuple_seed_takes_an_item_of_a_literal_list() {
    let (ok, out, err) = run(
        "tuple",
        r#"fn main() {
    let a = [1, 2, 3];
    let b = [3, 2, 1];
    let (s, t) = a.iter().zip(b.iter()).fold((0, 0), |(p, q), (x, y)| (x, p + q + y));
    println(f"{s} {t}");
    let w = [1, 2, 3, 4];
    println(f"{w.iter().fold((0, 0), |(p, q), x| (x, p + q)).1}");
}
"#,
    );
    assert!(ok, "refused: {err}");
    assert_eq!(out, "3 9\n6\n");
}

#[test]
fn a_lone_seed_takes_an_item_of_a_literal_list() {
    let (ok, out, err) = run(
        "lone",
        r#"fn main() {
    let w = [1, 2, 3, 4];
    println(f"{w.iter().fold(0, |p, x| x)}");
    let t = (0, 0);
    println(f"{w.iter().fold(t, |(p, q), x| (x, p + q)).1}");
    let f = [1.5, 2.5];
    println(f"{f.iter().fold(0.0, |a, x| x)}");
}
"#,
    );
    assert!(ok, "refused: {err}");
    assert_eq!(out, "4\n6\n2.5\n");
}

#[test]
fn an_integer_seed_still_refuses_a_float_item() {
    let (ok, _, err) = run(
        "kinds",
        r#"fn main() {
    let f = [1.5, 2.5];
    println(f"{f.iter().fold(0, |a, x| x)}");
}
"#,
    );
    assert!(!ok);
    assert!(err.contains("found 'ref f64'"), "{err}");
}
