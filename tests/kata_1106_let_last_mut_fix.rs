//! Kata 1106's frames arm writes to the top frame of a stack the way Rust
//! does: `let top = frames.last_mut().unwrap();` and then
//! `top.seen_true = true;`. Kāra has no `last_mut`, and the rewrite
//! (B-2026-11-08-46) covered only `match` and `if let` on it, so a `let`
//! was advice alone. When the rest of the block names neither the vector
//! nor a new binding of the same name, and every use of the binding is
//! the object of a field, an index or a method call, the `let` is dropped
//! and each use becomes `frames[frames.len() - 1]` (B-2026-11-10-51).

use std::process::{Command, Stdio};

/// `src` after `karac fix` until nothing changes, and its `check` stderr.
fn fixed(tag: &str, src: &str) -> (String, String) {
    let path =
        std::env::temp_dir().join(format!("karac-kata1106-{tag}-{}.kara", std::process::id()));
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

#[test]
fn kata_1106_let_last_mut_unwrap_is_rewritten() {
    let src = "struct Frame {
    seen_true: bool,
    seen_false: bool,
}

fn main() {
    let mut frames: Vec[Frame] = [Frame { seen_true: false, seen_false: false }];
    for v in [true, false, true].iter() {
        frames.push(Frame { seen_true: false, seen_false: false });
        if v {
            let top = frames.last_mut().unwrap();
            if v {
                top.seen_true = true;
            } else {
                top.seen_false = true;
            }
        }
    }
    let n = frames.iter().filter(|f| f.seen_true).count();
    println(f\"{n} of {frames.len()}\");
}
";
    let (text, check) = fixed("let-last-mut", src);
    assert!(!text.contains("last_mut"), "{text}");
    assert!(!text.contains("let top"), "{text}");
    assert!(
        text.contains("frames[frames.len() - 1].seen_true = true;"),
        "{text}"
    );
    assert!(
        check.contains("All checks passed") || check.is_empty(),
        "{check}"
    );
    let out = karac::mir::lower::run_source(&text)
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .output;
    assert_eq!(out, "2 of 4\n");
}

#[test]
fn kata_1106_let_last_mut_with_the_vector_used_after_keeps_the_advice() {
    let src = "fn main() {
    let mut v: Vec[i64] = [1, 2, 3];
    let top = v.last_mut().unwrap();
    v.push(4);
    *top += 1;
    println(f\"{v}\");
}
";
    let (text, check) = fixed("vector-used", src);
    assert!(
        text.contains("last_mut"),
        "the vector is named after the let: {text}"
    );
    assert!(check.contains("last_mut"), "{check}");
}
