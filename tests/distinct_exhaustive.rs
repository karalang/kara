//! B-2026-11-18-33: a distinct type's constructor pattern with a
//! destructuring sub-pattern (`Pt((a, b)) => a + b`) was reported
//! non-exhaustive. f452630364 (B-2026-11-18-18) made a distinct column match
//! over its base's constructors, which fixed this too; these pin the nested
//! shapes, and that a missing case names its witness in the distinct's form.

use std::process::Command;

fn run(tag: &str, sub: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-distinct-exh-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(sub)
        .arg(&f)
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_destructuring_distinct_constructor_pattern_is_exhaustive() {
    let src = "distinct type Pt = (i64, i64);\ndistinct type Flag = bool;\ndistinct type Pair = (bool, i64);\nfn g(p: Pt) -> i64 { match p { Pt((a, b)) => a + b } }\nfn h(f: Flag) -> i64 { match f { Flag(true) => 1, Flag(false) => 0 } }\nfn k(p: Pair) -> i64 { match p { Pair((true, n)) => n, Pair((false, _)) => 0 } }\nfn main() { println(g(Pt((3, 4)))); println(h(Flag(true))); println(k(Pair((true, 5)))); }\n";
    let checked = run("ok", "check", src);
    assert!(!checked.contains("error"), "{checked}");
    let ran = run("ok-run", "__mir-run", src);
    assert_eq!(ran, "7\n1\n5\n", "{ran}");
}

#[test]
fn a_missing_case_under_a_distinct_constructor_names_the_witness() {
    let out = run(
        "bad",
        "check",
        "distinct type Flag = bool;\ndistinct type Pair = (bool, i64);\nfn h(f: Flag) -> i64 { match f { Flag(true) => 1 } }\nfn k(p: Pair) -> i64 { match p { Pair((true, n)) => n } }\nfn main() { println(h(Flag(true))); println(k(Pair((true, 5)))); }\n",
    );
    assert!(out.contains("`Flag(false)` not covered"), "{out}");
    assert!(out.contains("`Pair((false, _))` not covered"), "{out}");
}
