//! B-2026-11-08-47: `Slice.split(pred)` (docs/library/collections.md
//! § Slice): the runs between the elements `pred` accepts, which are
//! dropped, with Rust's empty-run semantics, on a `Vec`, an array and a
//! `Slice`; and Rust's `line.split(|&ch| ch == '#')` carried over by
//! `karac fix`.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let dir = std::env::temp_dir().join(format!("karac-slice-split-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(args)
        .arg("m.kara")
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

const SPLIT: &str = r#"fn count_runs(s: Slice[i64]) -> i64 { s.split(|x| x < 0).len() }
fn main() {
    let v: Vec[i64] = [1, 0, 2, 3, 0];
    let parts = v.split(|x| x == 0);
    println(parts.len());
    for p in parts.iter() {
        println(p.len());
    }
    let words: Vec[String] = ["a".to_string(), "|".to_string(), "b".to_string()];
    let runs = words.split(|w| w == "|");
    println(f"{runs.len()} {runs[0][0]} {runs[1][0]}");
    let arr = [5, -1, 6, 7];
    println(arr.split(|x| x < 0).len());
    let w: Vec[i64] = [1, -2, 3, -4, -5, 6];
    println(count_runs(w[1..]));
    let mut seen = 0;
    let r = w.split(|x| { seen = seen + 1; x < 0 });
    println(f"{seen} {r.len()} {r[2].len()}");
    let e: Vec[i64] = Vec.new();
    println(e.split(|x| x > 0).len());
}
"#;

const WANT: &str = "3\n1\n2\n0\n2 a b\n2\n4\n6 4 0\n1\n";

#[test]
fn split_keeps_the_runs_between_separators_empty_ones_too() {
    let check = karac("check", &["check"], SPLIT);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run", "run"]
    } else {
        &["__mir-run"]
    };
    for b in backends {
        let out = karac(&format!("run-{}", b.trim_start_matches('_')), &[b], SPLIT);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            WANT,
            "{b}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn rusts_split_by_a_predicate_is_carried_over_by_fix() {
    // Kata 2018's Rust template arm.
    let src = "fn fits(line: &Vec<char>, word: &Vec<char>) -> bool {
    for seg in line.split(|&ch| ch == '#') {
        if seg.len() == word.len() && seg.iter().zip(word.iter()).all(|(a, b)| *a == ' ' || a == b) {
            return true;
        }
    }
    false
}
fn main() {
    let line: Vec[char] = ['a', '#', ' ', 'c', '#'];
    let w: Vec[char] = ['b', 'c'];
    println(fits(line, w));
}
";
    let dir = std::env::temp_dir().join(format!("karac-slice-split-fix-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("m.kara");
    std::fs::write(&file, src).unwrap();
    for _ in 0..3 {
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg("fix")
            .arg(&file)
            .output()
            .expect("karac fix");
    }
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(fixed.contains("line.split(|ch| ch == '#')"), "{fixed}");
    let check = karac("fixed-check", &["check"], &fixed);
    assert!(
        check.status.success(),
        "{fixed}\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let out = karac("fixed-run", &["__mir-run"], &fixed);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "true\n");
}

#[cfg(feature = "llvm")]
#[test]
fn the_built_binary_splits_the_same() {
    let dir = std::env::temp_dir().join(format!("karac-slice-split-build-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), SPLIT).unwrap();
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["build", "m.kara", "-o", "m.bin"])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    if !built.status.success() {
        let err = String::from_utf8_lossy(&built.stderr).into_owned();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            err.contains("archive")
                && std::env::var("KARAC_REQUIRE_RUNTIME_ARCHIVE").as_deref() != Ok("1"),
            "{err}"
        );
        eprintln!("skipped the built leg: {err}");
        return;
    }
    let out = Command::new(dir.join("m.bin"))
        .stdin(Stdio::null())
        .output()
        .expect("run");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(String::from_utf8_lossy(&out.stdout), WANT);
}
