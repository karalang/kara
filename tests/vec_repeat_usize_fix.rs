//! B-2026-11-14-13: `vec![false; n as usize]` checks with only a
//! deprecation warning, but the warning's fix wrote
//! `Vec.filled(n as usize, false)`, which is refused (`Vec.filled` takes
//! an `i64` length), and the refusal set off the `usize` pass's fixes on
//! every other `as usize`. So one round of `karac fix` turned a checking
//! program into a refused one. The fix now casts such a count to `i64`
//! as it moves it, and reads both operands between the brackets: a
//! parenthesized operand's span leaves its `(` out, and
//! `vec![0; (a + 1) as usize]` became the unparsable
//! `Vec.filled(a + 1) as usize, 0)`, which `karac fix` skipped.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-vec-repeat-usize-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

#[test]
fn every_round_of_the_vec_repeat_fix_still_checks() {
    let src = "fn main() {\n    let a: i64 = 2;\n    let n: i32 = 4;\n    \
               let e: Vec[i32] = [1, 2];\n    \
               let mut flags = vec![false; n as usize];\n    \
               flags[e[1] as usize] = true;\n    \
               let w = vec![0; (a + 1) as usize];\n    \
               let x = vec![(a); a as usize];\n    \
               println(f\"{flags.len()} {flags[2]} {w.len()} {x}\");\n}\n";
    let (dir, path) = fixture("rounds", src);
    let want = "4 true 3 [2, 2]\n";
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), want);
    for round in 1..=3 {
        karac().arg("fix").arg(&path).output().unwrap();
        let fixed = std::fs::read_to_string(&path).unwrap();
        let check = karac().arg("check").arg(&path).output().unwrap();
        assert!(
            check.status.success(),
            "round {round}:\n{fixed}\n{}",
            String::from_utf8_lossy(&check.stderr)
        );
        if round == 1 {
            for edit in [
                "let mut flags = Vec.filled(n as i64, false);",
                "flags[e[1] as usize] = true;",
                "let w = Vec.filled((a + 1) as i64, 0);",
                "let x = Vec.filled(a as i64, (a));",
            ] {
                assert!(fixed.contains(edit), "expected `{edit}` in:\n{fixed}");
            }
        }
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        !fixed.contains("vec!") && !fixed.contains("as i64"),
        "{fixed}"
    );
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{fixed}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A bare `[v; n]` where a `Vec` is wanted is refused with the same
/// rewrite, its `usize` count cast to `i64` too.
#[test]
fn a_bare_repeat_literal_for_a_vec_moves_a_usize_count_as_i64() {
    let src = "fn main() {\n    let a: i64 = 2;\n    \
               let v: Vec[bool] = [false; (a + 1) as usize];\n    \
               println(f\"{v.len()}\");\n}\n";
    let (dir, path) = fixture("bare", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("write `Vec.filled((a + 1) as i64, false)`"),
        "{err}"
    );
    karac().arg("fix").arg(&path).output().unwrap();
    let check = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "3\n");
    let _ = std::fs::remove_dir_all(&dir);
}
