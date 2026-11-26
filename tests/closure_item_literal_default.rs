//! An item closure's parameter that holds what an earlier `map` closure
//! returned, `.map(|x| { sum += x; sum }).fold(0i32, |low, s| low.min(s))`,
//! is refused as the `i64` that `sum`'s literal kept, and the refusal is
//! fixed on `sum` (`sum: i32`), as a refusal of `sum` itself was
//! (B-2026-11-26-8). It was refused with no explanation and no fix, so the
//! seed's fix left the program refused.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

/// `karac fix` then `karac check` on `src`: the fixed text, and whether it
/// checks clean.
fn fix_then_check(src: &str) -> (String, bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-item-literal-param-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let _ = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("fix")
        .arg(&f)
        .output()
        .unwrap();
    let fixed = std::fs::read_to_string(&f).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        fixed,
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn runs(src: &str, want: &str) {
    let dir = std::env::temp_dir().join(format!(
        "karac-item-literal-param-run-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{src}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{src}");
}

fn check_out(src: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "karac-item-literal-param-check-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

const MAIN: &str = "fn main() { println(lowest([1, -3, 2].to_vec())); }\n";

fn fixes_and_runs(body: &str, want_text: &str) {
    let src = format!("fn lowest(nums: Vec[i32]) -> i32 {{ {body} }}\n{MAIN}");
    let (fixed, ok, out) = fix_then_check(&src);
    assert!(fixed.contains(want_text), "{src}\nfixed to:\n{fixed}");
    assert!(ok, "{fixed}\n{out}");
    runs(&fixed, "-2\n");
}

#[test]
fn a_fold_item_from_a_map_tail_names_the_binding() {
    let src = format!(
        "fn lowest(nums: Vec[i32]) -> i32 {{ let mut sum = 0; \
         let low = nums.iter().map(|x| {{ sum += x; sum }}).fold(0i32, |low, s| low.min(s)); low }}\n{MAIN}"
    );
    let out = check_out(&src);
    assert!(
        out.contains("`sum` is an 'i64' because its literal kept the default")
            && out.contains("declare it `sum: i32`"),
        "{out}"
    );
}

#[test]
fn the_seed_and_the_binding_are_both_fixed() {
    fixes_and_runs(
        "let mut sum = 0; let low = nums.iter().map(|x| { sum += x; sum }).fold(0, |low, s| low.min(s)); low",
        "let mut sum: i32 = 0;",
    );
    fixes_and_runs(
        "let mut sum = 0; let low = nums.iter().map(|x| { sum += x; sum }).fold(0i32, |low, s| low.min(s)); low",
        "let mut sum: i32 = 0;",
    );
}

#[test]
fn an_item_through_a_filter_into_for_each_is_fixed_too() {
    fixes_and_runs(
        "let mut sum = 0; let mut low: i32 = 0; \
         nums.iter().map(|x| { sum += x; sum }).filter(|v| v < 100).for_each(|s| { low = low.min(s); }); low",
        "let mut sum: i32 = 0;",
    );
}
