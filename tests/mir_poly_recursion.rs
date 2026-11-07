//! Polymorphic recursion is refused by `karac check` at the recursive
//! call, from the call graph, so the check always ends (design.md §8,
//! B-2026-11-06-12). Before, check and run made one instance per nesting
//! depth and never finished. The programs are the `corpus/core` pins.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `karac check` on `src`, killed after a minute: `None` if it ran that long.
fn check(tag: &str, src: &str) -> Option<(bool, String)> {
    let dir = std::env::temp_dir().join(format!("karac-poly-rec-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break Some(s);
        }
        if start.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        use std::io::Read;
        let _ = e.read_to_string(&mut err);
    }
    let _ = std::fs::remove_dir_all(&dir);
    status.map(|s| (s.success(), err))
}

fn refused(tag: &str, src: &str, line: usize, want: &str) {
    let Some((ok, err)) = check(tag, src) else {
        panic!("{tag}: check ran for a minute");
    };
    assert!(!ok, "{tag}: check must refuse");
    assert!(err.contains("polymorphic recursion"), "{tag}: {err}");
    assert!(err.contains(want), "{tag}: expected `{want}` in: {err}");
    assert!(
        err.contains(&format!("p.kara:{line}:")),
        "{tag}: expected the refusal at line {line}: {err}"
    );
}

#[test]
fn poly_recursion_at_a_vec_is_refused_at_the_call() {
    refused(
        "vec",
        include_str!("../corpus/core/err_poly_recursion_vec_grows/source.kara"),
        9,
        "`deep` calls itself with `Vec[T]` in place of `T`",
    );
}

#[test]
fn poly_recursion_at_some_is_refused_at_the_call() {
    refused(
        "some",
        include_str!("../corpus/core/err_poly_recursion_some_grows/source.kara"),
        7,
        "`deep` calls itself with `Option[T]` in place of `T`",
    );
}

#[test]
fn poly_recursion_through_another_generic_is_refused() {
    refused(
        "mutual",
        include_str!("../corpus/core/err_poly_recursion_mutual_grows/source.kara"),
        8,
        "`ping` calls `pong` with `(T, i64)`",
    );
}

#[test]
fn poly_recursion_through_trait_dispatch_is_refused() {
    refused(
        "dispatch",
        include_str!("../corpus/core/err_poly_recursion_through_dispatch/source.kara"),
        17,
        "`grow` calls `helper` with `Vec[Vec[T]]`",
    );
}

#[test]
fn recursion_that_needs_finitely_many_instances_checks() {
    for (tag, src) in [
        (
            "same",
            include_str!("../corpus/core/ok_recursion_same_or_other_type/source.kara"),
        ),
        (
            "peels",
            include_str!("../corpus/core/ok_recursion_through_dispatch_peels/source.kara"),
        ),
    ] {
        let Some((ok, err)) = check(tag, src) else {
            panic!("{tag}: check ran for a minute");
        };
        assert!(ok, "{tag}: check must accept: {err}");
    }
}
