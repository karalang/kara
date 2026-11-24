//! B-2026-11-24-3 (kata 2535): B-2026-11-15-34's seed check read the closure's
//! result only as a direct `Some(e)`, so kata 2535's digit walk,
//! `successors(Some(x), |r| if r >= 10 { Some(r / 10) } else { None })`
//! with `x` a `ref i64` from `nums.iter()`, still got the bare E0509 at the
//! closure. The check now reads every branch of an `if` or `match`.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn checked(name: &str, src: &str) -> (std::path::PathBuf, String, bool) {
    let dir = std::env::temp_dir().join(format!("karac-succbranch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let check = karac(&["check", f.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&check.stdout).into_owned()
        + &String::from_utf8_lossy(&check.stderr);
    (f, text, check.status.success())
}

#[test]
fn a_borrowed_successors_seed_under_an_if_is_named_and_fixed() {
    let src = "fn main() {
    let nums = [15, 6, 1234].to_vec();
    let digit_sum: i64 = nums
        .iter()
        .flat_map(|x| successors(Some(x), |r| if r >= 10 { Some(r / 10) } else { None }).map(|r| r % 10))
        .sum();
    println(digit_sum);
}
";
    let (f, text, ok) = checked("if", src);
    assert!(!ok, "{text}");
    assert!(
        text.contains("the seed makes `successors`'s item a `ref i64`"),
        "{text}"
    );
    assert!(text.contains("`Some(*x)`"), "{text}");
    assert!(!text.contains("E0509"), "{text}");
    let p = f.to_str().unwrap();
    assert!(karac(&["fix", p]).status.success());
    let after = std::fs::read_to_string(&f).unwrap();
    assert!(after.contains("successors(Some(*x), |r|"), "{after}");
    let run = karac(&["run", "--interp", p]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "22\n");
}

#[test]
fn a_borrowed_successors_seed_under_a_match_is_named() {
    let src = "fn main() {
    let v = [5].to_vec();
    for x in v.iter() {
        let w: Vec[i64] = successors(Some(x), |r| match r {
            0 => None,
            _ => Some(r - 1),
        }).collect();
        println(f\"{w}\");
    }
}
";
    let (_, text, ok) = checked("match", src);
    assert!(!ok, "{text}");
    assert!(
        text.contains("the seed makes `successors`'s item a `ref i64`"),
        "{text}"
    );
}
