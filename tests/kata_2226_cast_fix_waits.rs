//! A cast lint's fix waits for a pass with no error (B-2026-11-17-140).
//!
//! Kata 2226's LeetCode template, carried by `karac fix`: one pass applied
//! the E0200 fix that retypes `0` to `0i32` together with a `redundant_cast`
//! fix judged while `mid` was still i64, so `(c / mid) as i64` lost its cast
//! just as `mid` became i32, and the `.sum()` into i64 overflowed i32 at run
//! time. The cast fixes now wait, like `redundant_clone` (B-2026-11-14-3).

use std::process::Command;

const SRC: &str = "fn best(candies: Vec[i32], k: i64) -> i32 {
    let mut (lo, hi) = (0, *candies.iter().max().unwrap());
    while lo < hi {
        let mid = lo + (hi - lo + 1) / 2;
        let cnt: i64 = candies.iter().map(|c| (c / mid) as i64).sum();
        if cnt >= k {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

fn main() {
    let mut v: Vec[i32] = Vec.new();
    for _ in 0..3 {
        v.push(2000000000);
    }
    println(f\"{best(v, 1)}\");
}
";

fn karac(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn kata_2226_cast_fix_waits_for_the_retype() {
    let dir = std::env::temp_dir().join(format!("karac-2226-cast-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("c.kara"), SRC).unwrap();
    let dry = karac(&dir, &["fix", "--dry-run", "c.kara"]);
    let text = String::from_utf8_lossy(&dry.stdout);
    assert!(text.contains("would apply 1 fix(es)"), "{text}");
    assert!(!text.contains("` as i64` → ``"), "{text}");
    for _ in 0..2 {
        let fixed = karac(&dir, &["fix", "c.kara"]);
        assert!(fixed.status.success(), "{fixed:?}");
    }
    let after = std::fs::read_to_string(dir.join("c.kara")).unwrap();
    assert!(after.contains("(0i32, *candies"), "{after}");
    assert!(after.contains("(c / mid) as i64"), "{after}");
    let check = karac(&dir, &["check", "c.kara"]);
    assert!(check.status.success(), "{check:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
