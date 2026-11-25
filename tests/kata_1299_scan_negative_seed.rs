//! B-2026-11-25-18 (kata 1299): `arr.iter().rev().scan(-1, |m, x| ..)` over `i32`
//! items, collected into a `Vec[i32]`, a running maximum as LeetCode's Rust
//! users start one. The state's starting literal keeps the default `i64`
//! beside the fixed items, so the `collect` is refused, as `scan(0, ..)` is
//! (tests/scan_state_literal_open.rs); but where `0` gets the refusal's note
//! naming the seed and the `0i32` fix, the negated `-1` got only the bare
//! "`C: FromIterator[i64]` is not satisfied", and `karac fix` had nothing.
//! It now gets both, `-1i32`.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-k1299-{name}-{}", std::process::id()));
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

const RUNNING_MAX: &str = "fn main() {
    let arr: Vec[i32] = [17, 18, 5, 4, 6, 1];
    let mut res: Vec[i32] = arr
        .iter()
        .rev()
        .scan(-1, |m, x| { let r = m; let m = m.max(x); Some((m, r)) })
        .collect();
    res.reverse();
    println(res);
}
";

#[test]
fn a_negated_scan_seed_is_named_and_fixed_with_its_suffix() {
    let f = program("seed", RUNNING_MAX);
    let (ok, out) = karac(&["check"], &f);
    assert!(!ok, "{out}");
    assert!(
        out.contains("the `scan`'s state is an 'i64' because its starting value `-1`")
            && out.contains("write `-1i32`"),
        "{out}"
    );
    let (ok, out) = karac(&["fix"], &f);
    assert!(ok, "{out}");
    let fixed = std::fs::read_to_string(&f).unwrap();
    assert!(fixed.contains(".scan(-1i32, |m, x|"), "{fixed}");
    let (ok, out) = karac(&["run"], &f);
    assert!(ok, "{out}");
    assert_eq!(out, "[18, 6, 6, 6, 1, -1]\n");
}

#[test]
fn a_negated_scan_seed_out_of_an_unsigned_range_gets_no_suffix() {
    let f = program(
        "unsigned",
        "fn main() {
    let arr: Vec[u32] = [3, 1, 2];
    let res: Vec[u32] = arr.iter().scan(-1, |m, x| { let m = m + 1; Some((m, m)) }).collect();
    println(res);
}
",
    );
    let (ok, out) = karac(&["check"], &f);
    assert!(!ok, "{out}");
    assert!(
        out.contains("negative integer literal -1 cannot initialize unsigned type 'u32'")
            && !out.contains("write `-1u32`"),
        "{out}"
    );
}
