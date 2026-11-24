//! B-2026-11-24-40 (kata 2501): `(0..5).scan(v, |x, _| Some((x * x, x)))` with `v`
//! the `ref i64` that iterating a `Set` gives. The seed makes the state a
//! `ref i64`, so the closure's new `x * x` cannot be the next state, and
//! the refusal was a bare E0200 at the closure, "expected 'Option[(ref
//! i64, _)]', found 'Option[(i64, ref i64)]'", naming neither the seed nor
//! the `*v` that mends it. It is now at the seed, with that fix, as
//! `successors`'s is (B-2026-11-15-34).

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-k2501-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

fn karac(args: &[&str], f: &PathBuf) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(f)
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn a_borrowed_scan_seed_is_named_and_fixed_with_a_deref() {
    let f = program(
        "seed",
        "fn main() {
    let present: Set[i64] = [2, 4, 16, 3].to_vec().iter().cloned().collect();
    let mut best = 0;
    for v in present.iter() {
        let n = (0..5)
            .scan(v, |x, _| Some((if x <= 100000 { x * x } else { x }, x)))
            .take_while(|x| present.contains(x))
            .count();
        best = best.max(n);
    }
    println(best);
}
",
    );
    let (ok, out) = karac(&["check"], &f);
    assert!(!ok, "{out}");
    assert!(
        out.contains("the seed makes `scan`'s state a `ref i64`"),
        "{out}"
    );
    assert!(out.contains("start from the value: `*v`"), "{out}");
    assert!(!out.contains("expected 'Option[(ref i64"), "{out}");
    let (ok, out) = karac(&["fix"], &f);
    assert!(ok, "fix: {out}");
    let fixed = std::fs::read_to_string(&f).unwrap();
    assert!(fixed.contains(".scan(*v, |x, _|"), "{fixed}");
    let (ok, out) = karac(&["run", "--interp"], &f);
    assert!(ok, "run: {out}");
    assert_eq!(out, "3\n");
}

#[test]
fn an_owned_scan_seed_keeps_its_own_refusal() {
    let f = program(
        "owned",
        "fn main() {
    let v: i64 = 2;
    let w: Vec[i64] = (0..3).scan(v, |x, _| Some((x * 2, true))).collect();
    println(f\"{w}\");
}
",
    );
    let (ok, out) = karac(&["check"], &f);
    assert!(!ok, "{out}");
    assert!(!out.contains("the seed makes `scan`'s state"), "{out}");
}
