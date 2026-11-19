//! Kata 1244's sorted arm reads a player's old score with `let now = match
//! self.scores.get(player_id) { Some(old) => { self.drop_score(old); old +
//! score } None => score };`. `get` returns `Option[ref i64]`, so `old`
//! borrows `self` across the `mut ref self` call and E0516 is right. A bare
//! `match` scrutinee gets `.cloned()` as a fix-it (B-2026-11-15-49), but a
//! `match` that is a `let`'s initializer got none: the fix read the `let`
//! and looked for a borrow ending at its `;` (B-2026-11-20-81). It now
//! gets the same `.cloned()`.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

const BOARD: &str = "struct Board {
    scores: Map[i64, i64],
    counts: SortedMap[i64, i64],
}

impl Board {
    fn drop_score(mut ref self, score: i64) {
        let left = self.counts[score] - 1;
        if left == 0 {
            self.counts.remove(score);
        } else {
            self.counts[score] = left;
        }
    }

    fn add_score(mut ref self, player: i64, score: i64) {
        let now = match self.scores.get(player) {
            Some(old) => {
                self.drop_score(old);
                old + score
            }
            None => score,
        };
        self.scores[player] = now;
        self.counts[now] = self.counts.get(now).unwrap_or(0) + 1;
    }
}

fn main() {
    let mut b = Board { scores: Map.new(), counts: SortedMap.new() };
    b.add_score(1, 5);
    b.add_score(2, 5);
    b.add_score(1, 4);
    println(f\"{b.scores[1]} {b.counts.len()} {b.counts[5]} {b.counts[9]}\");
}
";

#[test]
fn kata_1244_let_bound_match_on_a_borrowed_copy_value_gets_cloned_fix() {
    let path =
        std::env::temp_dir().join(format!("karac-k1244-let-match-{}.kara", std::process::id()));
    std::fs::write(&path, BOARD).unwrap();
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(stderr.contains("[E0516]"), "{stderr}");
    let fix = karac(&["fix", file]);
    assert!(fix.status.success(), "{fix:?}");
    let text = std::fs::read_to_string(file).unwrap();
    assert!(
        text.contains("let now = match self.scores.get(player).cloned() {"),
        "{text}"
    );
    let run = karac(&["__mir-run", file]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "9 2 1 1\n", "{run:?}");
    let _ = std::fs::remove_file(&path);
}
