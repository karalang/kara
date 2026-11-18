//! B-2026-11-17-109: a local whose type a later use decides is `Copy` when
//! the solved type is. `let mut x = None; x = Some(5); (x, x)`, a
//! `[None; 3]` array, and kata 1182's `scan` state seeded with `[None; 4]`
//! were refused as a use after a move (E0500), and `karac fix` added a
//! needless `.clone()`. The span-keyed type table kept the signature's `T`
//! (or a variable the closure body solved later) from when the value was
//! checked, while the value's node had the solved `Option[i64]`; the
//! ownership pass read the span table. `expr_type` now prefers a node type
//! with no parameter or variable left over a span type that has one.

use std::process::{Command, Output};

fn karac(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `karac check` and `karac run --interp` on `src`.
fn check_and_run(tag: &str, src: &str) -> (Output, Output) {
    let dir = std::env::temp_dir().join(format!("karac-late-copy-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let p = path.to_str().unwrap();
    let check = karac(&["check", p]);
    let run = karac(&["run", "--interp", p]);
    let _ = std::fs::remove_dir_all(&dir);
    (check, run)
}

#[test]
fn a_none_local_solved_later_is_copy() {
    let (check, run) = check_and_run(
        "none",
        "fn main() {\n    let mut x = None;\n    x = Some(5);\n    let y = (x, x);\n    \
println(y.0.unwrap() + y.1.unwrap());\n}\n",
    );
    assert!(check.status.success(), "{}", text(&check));
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "10\n",
        "{}",
        text(&run)
    );
}

#[test]
fn a_repeat_array_of_none_solved_later_is_copy() {
    let (check, run) = check_and_run(
        "repeat",
        "fn main() {\n    let mut c = [None; 3];\n    c[0] = Some(5);\n    let e = c;\n    \
let d = (c, e);\n    println(d.0.len() + d.1.len());\n}\n",
    );
    assert!(check.status.success(), "{}", text(&check));
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "6\n",
        "{}",
        text(&run)
    );
}

/// Kata 1182's chain arm: the state is handed out as both the next state
/// and the item, with no `.clone()`.
#[test]
fn a_scan_state_seeded_with_none_is_copy() {
    let (check, run) = check_and_run(
        "scan",
        r#"fn main() {
    let s = "abca";
    let firsts: Vec[Array[Option[i64], 4]] = s
        .char_indices()
        .map(|(i, ch)| (i, (ch as u32 - 'a' as u32) as i64))
        .scan([None; 4], |last, (i, c)| {
            let mut last = last;
            last[c] = Some(i);
            Some((last, last))
        })
        .collect();
    for f in firsts {
        println(f"{f[0]} {f[1]} {f[2]}");
    }
}
"#,
    );
    assert!(check.status.success(), "{}", text(&check));
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "Some(0) None None\nSome(0) Some(1) None\nSome(0) Some(1) Some(2)\nSome(3) Some(1) Some(2)\n",
        "{}",
        text(&run)
    );
}

/// The solved type decides: a `Vec` solved the same way still moves.
#[test]
fn a_vec_of_none_solved_later_still_moves() {
    let (check, _) = check_and_run(
        "vec",
        "fn main() {\n    let mut a = Vec.filled(2, None);\n    a[0] = Some(1);\n    \
let b = (a, a);\n    println(b.0.len());\n}\n",
    );
    assert_eq!(check.status.code(), Some(1), "{}", text(&check));
    assert!(
        text(&check).contains("value 'a' moved here, used again here"),
        "{}",
        text(&check)
    );
}
