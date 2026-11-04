//! design.md § `#[must_use]` mandate for the standard library, categories
//! 2, 3 and 5 (B-2026-11-04-71): with the library compiled from source, a
//! discarded iterator adapter (its own struct, `IterMap[..]`), a discarded
//! `MutexGuard` and a discarded `trim` warn under `karac check`, and the note
//! spells the attribute as Kāra writes it. A user iterator is not the
//! mandate's and stays silent.

use std::process::{Command, Stdio};

fn check(tag: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "karac-must-use-library-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn library_guard_adapter_and_trim_discards_warn() {
    let out = check(
        "warn",
        "fn main() {
    let m = Mutex.new(1);
    m.lock();
    let v = [1, 2];
    v.iter();
    v.iter().map(|x| x + 1);
    v.iter().filter(|x| x > 1);
    let s: String = \" a \".to_string();
    s.trim();
    s.trim_end();
    let kept = m.lock();
    let n: i64 = v.iter().map(|x| x + 1).sum();
    println(f\"{kept.value} {n} {s.trim()}\");
}
",
    );
    for want in [
        "3:5: discarded `MutexGuard` value",
        "the mutex is released when this guard drops; bind it to a name to keep the lock",
        "5:5: discarded `VecIter` value",
        "6:5: discarded `IterMap` value",
        "7:5: discarded `IterFilter` value",
        "discarding the iterator drops every adapter without running it",
        "9:5: discarded return value of `String.trim`",
        "10:5: discarded return value of `String.trim_end`",
        "#[must_use(\"...\")]",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    assert_eq!(out.matches("warning[must_use]").count(), 6, "{out}");
    assert!(!out.contains("#[must_use = "), "{out}");
}

#[test]
fn a_user_iterator_discard_is_not_the_library_mandate() {
    let out = check(
        "user",
        "struct Count { n: i64 }
impl Iterator for Count {
    type Item = i64;
    fn next(mut ref self) -> Option[i64] { None }
}
fn main() {
    let c = Count { n: 1 };
    c;
    println(\"ok\");
}
",
    );
    assert!(!out.contains("warning[must_use]"), "{out}");
    assert!(out.contains("All checks passed"), "{out}");
}
