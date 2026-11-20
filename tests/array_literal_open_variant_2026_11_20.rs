//! B-2026-11-21-4: a list literal whose elements leave a type slot open
//! (`Err(0)` names no `T`) took the slot from an earlier element only when
//! the open element came first. `[Err(1), Ok(5), Err(0)]` decided `T` at
//! `Ok(5)`, then refused `Err(0)` as "expected `Result[i64, i64]`, found
//! `Result[T, i64]`", while `[Err(1), Ok(5)]` and an annotated array were
//! accepted. Each later open element now joins as a branch of an `if` does.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn run(src: &str) -> (bool, String, String) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-openlit-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", "main.kara"])
        .current_dir(&dir)
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
fn open_result_after_a_decided_one_joins() {
    let (ok, out, err) = run(r#"fn main() {
    let mut rv: Vec[Result[i64, i64]] = [Err(1), Ok(5), Err(0), Ok(2)].to_vec();
    rv.sort();
    let a = [Ok(5), Err(0), Err(1)];
    let b: Result[i64, i64] = a[2];
    let mut o = "";
    for x in rv.iter() { match x { Ok(v) => o = o + f"Ok({v}) ", Err(e) => o = o + f"Err({e}) " } }
    println(f"{o}{a.len()} {b.is_err()}");
}
"#);
    assert!(ok, "{err}");
    assert_eq!(out, "Ok(2) Ok(5) Err(0) Err(1) 3 true\n");
}

#[test]
fn open_option_after_a_decided_one_joins() {
    let (ok, out, err) = run(r#"fn main() {
    let v = [None, Some(5), None, Some(1)];
    let n: Option[i64] = v[2];
    println(f"{v.len()} {n.is_none()} {v[3] ?? 0}");
}
"#);
    assert!(ok, "{err}");
    assert_eq!(out, "4 true 1\n");
}
