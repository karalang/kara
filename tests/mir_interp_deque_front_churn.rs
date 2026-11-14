//! A `VecDeque` worked from the front, two `pop_front`s, a `push_front`
//! and a `push_back` a round (kata 1535's game), ran in quadratic time
//! under the MIR interpreter: `pop_front` dropped the empty slots in
//! front as soon as they outnumbered the elements, and the `push_front`
//! that followed opened as many again, so every round moved the whole
//! deque twice. 120000 rounds over 60000 values took 77 s in a debug build;
//! they take 1.4 s now. B-2026-11-14-4.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SRC: &str = r#"fn main() {
    let mut line: VecDeque[i64] = (0..60000).collect();
    let mut check = 0;
    for _ in 0..120000 {
        let a = line.pop_front().unwrap();
        let b = line.pop_front().unwrap();
        let (winner, loser) = if a > b { (a, b) } else { (b, a) };
        check = (check * 31 + winner) % 1000000007;
        line.push_front(winner);
        line.push_back(loser);
    }
    println(f"{line.len()} {line.front().unwrap()} {check}");
}
"#;

#[test]
fn deque_front_churn_is_linear_in_the_interpreter() {
    let path = std::env::temp_dir().join(format!("karac-deque-churn-{}.kara", std::process::id()));
    std::fs::write(&path, SRC).expect("write");
    let start = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let took = start.elapsed();
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "60000 59999 315751356\n"
    );
    // A debug build ran this in 1.4 s with the fix and 77 s without it.
    assert!(took < Duration::from_secs(30), "took {took:?}");
}
