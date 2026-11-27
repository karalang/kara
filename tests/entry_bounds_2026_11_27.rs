//! B-2026-11-26-79: an index on a counter that only falls from a start the
//! same vector just checked needs no upper check (`src/mir/entry_bounds.rs`),
//! which lets kata 1409's turn compile to a `memmove`. These pin that the
//! proof changes no answer: the turn prints the same on every lane, and a
//! loop the proof must not cover (its counter starts one past the checked
//! place) still stops with `index_out_of_bounds` compiled as interpreted.

use std::path::PathBuf;
use std::process::Command;

const TURN: &str = "fn main() {
    let mut p: Vec[i32] = (1..=6).collect();
    let mut s = 0;
    for q in [3i32, 6, 6, 1, 4] {
        let at = p.iter().position(|x| x == q).unwrap();
        let x = p[at];
        let mut i = at;
        while i > 0 {
            p[i] = p[i - 1];
            i -= 1;
        }
        p[0] = x;
        s = s * 10 + at;
    }
    for v in p.iter() {
        print(f\"{v} \");
    }
    println(f\"{s}\");
}
";

// By hand: [1..6] -> 3 at 2 -> [3,1,2,4,5,6]; 6 at 5 -> [6,3,1,2,4,5];
// 6 at 0; 1 at 2 -> [1,6,3,2,4,5]; 4 at 4 -> [4,1,6,3,2,5].
const TURN_EXPECTED: &str = "4 1 6 3 2 5 25024\n";

const PAST: &str = "fn main() {
    let mut p: Vec[i32] = (1..=4).collect();
    let at = p.iter().position(|x| x == 4).unwrap();
    let x = p[at];
    let mut i = at + 1;
    while i > 0 {
        p[i] = p[i - 1];
        i -= 1;
    }
    p[0] = x;
    println(f\"{p[0]}\");
}
";

fn program(name: &str, src: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-entry-bounds-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    file
}

fn karac(args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_AUTO_PAR", "0")
        .args(args)
        .output()
        .expect("karac");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_turn_over_a_checked_start_prints_the_same_interpreted() {
    let f = program("turn-interp", TURN);
    let (code, out, err) = karac(&["run", "--interp", f.to_str().unwrap()]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, TURN_EXPECTED);
}

#[test]
fn a_counter_past_the_checked_place_still_stops_interpreted() {
    let f = program("past-interp", PAST);
    let (code, _, err) = karac(&["run", "--interp", f.to_str().unwrap()]);
    assert_eq!(code, Some(101), "{err}");
    assert!(err.contains("out of bounds"), "{err}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_turn_over_a_checked_start_prints_the_same_compiled() {
    let f = program("turn-llvm", TURN);
    let (code, out, err) = karac(&["__mir-llvm-run", f.to_str().unwrap()]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, TURN_EXPECTED);
}

#[cfg(feature = "llvm")]
#[test]
fn a_counter_past_the_checked_place_still_stops_compiled() {
    let f = program("past-llvm", PAST);
    let (code, _, err) = karac(&["__mir-llvm-run", f.to_str().unwrap()]);
    assert_eq!(code, Some(101), "{err}");
    assert!(err.contains("out of bounds"), "{err}");
}
