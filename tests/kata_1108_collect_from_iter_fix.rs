//! Kata 1108's split arm is Rust's one line,
//! `address.split('.').collect::<Vec<&str>>().join("[.]")`. A turbofish on
//! a `collect` whose result is the receiver of a further call had advice
//! alone (B-2026-10-29-8), since the type cannot come from the use and
//! `Vec[T].from_iter(..)` did not lower then; it does now, and it names the
//! type at the call without moving anything out of the chain, so the fix
//! writes `Vec[Str].from_iter(address.split(".")).join("[.]")`, Rust's
//! `&str`, `HashSet` and `BTreeMap` spelled as Kāra's `Str`, `Set` and
//! `SortedMap` (B-2026-11-10-42). The same rewrite of a `String` target
//! gives `String.from_iter(it)`, which checked and then stopped in the MIR
//! builder with "a call with no resolved callee"; it now reaches the
//! library's `impl FromIterator[char] for String` (B-2026-11-10-43).

use std::process::{Command, Stdio};

/// `src` after `karac fix` until nothing changes, and its `check` stderr.
fn fixed(tag: &str, src: &str) -> (String, String) {
    let path =
        std::env::temp_dir().join(format!("karac-kata1108-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    for _ in 0..6 {
        let before = std::fs::read_to_string(&path).unwrap();
        let _ = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["fix", path.to_str().unwrap()])
            .stdin(Stdio::null())
            .output()
            .expect("karac");
        if std::fs::read_to_string(&path).unwrap() == before {
            break;
        }
    }
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let out = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    (out, String::from_utf8_lossy(&check.stderr).into_owned())
}

#[test]
fn kata_1108_split_collect_join_is_rewritten() {
    let src = "fn defang_ip_addr(address: String) -> String {
    address.split('.').collect::<Vec<&str>>().join(\"[.]\")
}

fn main() {
    println(defang_ip_addr(\"255.100.50.0\"));
}
";
    let (text, check) = fixed("split", src);
    assert!(
        text.contains("Vec[Str].from_iter(address.split(\".\")).join(\"[.]\")"),
        "{text}"
    );
    assert!(
        check.contains("All checks passed") || check.is_empty(),
        "{check}"
    );
    let out = karac::mir::lower::run_source(&text)
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .output;
    assert_eq!(out, "255[.]100[.]50[.]0\n");
}

#[test]
fn kata_1108_collect_into_a_set_map_or_string_mid_chain_is_rewritten() {
    let src = "fn main() {
    let v: Vec[i64] = [3, 1, 3, 2];
    let distinct = v.iter().cloned().collect::<HashSet<i64>>().len();
    let firsts = v.iter().cloned().map(|x| (x, x * 10)).collect::<BTreeMap<i64, i64>>().len();
    let back = \"abc\".chars().rev().collect::<String>().to_uppercase();
    println(f\"{distinct} {firsts} {back}\");
}
";
    let (text, check) = fixed("targets", src);
    for want in [
        "Set[i64].from_iter(v.iter().cloned()).len()",
        "SortedMap[i64, i64].from_iter(v.iter().cloned().map(|x| (x, x * 10))).len()",
        "String.from_iter(\"abc\".chars().rev()).to_uppercase()",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    assert!(
        check.contains("All checks passed") || check.is_empty(),
        "{check}"
    );
    let out = karac::mir::lower::run_source(&text)
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .output;
    assert_eq!(out, "3 3 CBA\n");
}

#[test]
fn kata_1108_string_from_iter_runs() {
    let src = "fn main() {
    let s = \"stressed\";
    let t = String.from_iter(s.chars().rev());
    let u: String = String.from_iter(s.chars().filter(|c| c != 's'));
    println(f\"{t} {u} {t.len()}\");
}
";
    let out = karac::mir::lower::run_source(src)
        .unwrap_or_else(|e| panic!("{e}"))
        .output;
    assert_eq!(out, "desserts treed 8\n");
}
