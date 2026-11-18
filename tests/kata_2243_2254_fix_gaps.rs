//! Two `karac fix` gaps met carrying LeetCode templates through katas 2243
//! and 2254 (B-2026-11-17-146, B-2026-11-17-147).
//!
//! 2243: `let mut s = s; .. s = next;` with `s` a bare parameter was fixed
//! to `s = ref next`, a borrow of a loop-local `String` that the next
//! round cloned; the fix is now the parameter's `own`, which the same pass
//! already writes for its give-away.
//!
//! 2254: `(end_minute as usize).min(v.len() - 1)` with `end_minute: i32`
//! was refused "`min` expects an argument of type `usize`, got `i64`" with
//! no fix; the `usize` meeting the length's `i64` now counts as the mix
//! that retypes the cast to `as i64`.

use std::process::Command;

const REASSIGN: &str = "fn digit_sum(s: String, k: i64) -> String {
    let mut s = s;
    while s.len() > k {
        let mut next = String.new();
        for chunk in s.bytes().chunks(k) {
            let sum: i64 = chunk.iter().map(|b| (b - b'0') as i64).sum();
            next.push_str(f\"{sum}\");
        }
        s = next;
    }
    s
}

fn main() {
    println(digit_sum(\"11111222223\".to_string(), 3));
}
";

const MIN_USIZE: &str = "fn last(v: String, end_minute: i32) -> String {
    let end = (end_minute as usize).min(v.len() - 1);
    v[0..end + 1].to_string()
}

fn main() {
    println(last(\"12345\".to_string(), 9));
    println(last(\"12345\".to_string(), 2));
}
";

fn karac(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

/// Fixes `src` until nothing is left, then checks and runs it.
fn carried(tag: &str, src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("t.kara"), src).unwrap();
    for _ in 0..4 {
        let fixed = karac(&dir, &["fix", "t.kara"]);
        assert!(fixed.status.success(), "{fixed:?}");
    }
    let after = std::fs::read_to_string(dir.join("t.kara")).unwrap();
    let check = karac(&dir, &["check", "t.kara"]);
    assert!(check.status.success(), "{after}\n{check:?}");
    let run = karac(&dir, &["__mir-run", "t.kara"]);
    assert!(run.status.success(), "{after}\n{run:?}");
    let _ = std::fs::remove_dir_all(&dir);
    (after, String::from_utf8_lossy(&run.stdout).into_owned())
}

#[test]
fn kata_2243_reassigned_parameter_copy_takes_own() {
    let (after, out) = carried("2243-own", REASSIGN);
    assert!(after.contains("fn digit_sum(s: own String"), "{after}");
    assert!(after.contains("        s = next;"), "{after}");
    assert!(!after.contains("ref next"), "{after}");
    assert_eq!(out, "135\n");
}

#[test]
fn kata_2254_usize_cast_meeting_a_length_in_min_is_retyped() {
    let dir = std::env::temp_dir().join(format!("karac-2254-dry-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("t.kara"), MIN_USIZE).unwrap();
    let dry = karac(&dir, &["fix", "--dry-run", "t.kara"]);
    let text = String::from_utf8_lossy(&dry.stdout);
    assert!(text.contains("`usize` → `i64`"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
    let (after, out) = carried("2254-min", MIN_USIZE);
    assert!(
        after.contains("(end_minute as i64).min(v.len() - 1)"),
        "{after}"
    );
    assert_eq!(out, "12345\n123\n");
}
