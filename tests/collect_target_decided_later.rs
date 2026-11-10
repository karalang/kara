//! A `collect()` whose target a later use decides (B-2026-11-09-56): a
//! binding lent to a `ref` parameter holds the value, not a reference, so
//! `collect` builds the parameter's type, and its `FromIterator` bound is
//! checked against it. And a closure whose body ends in `collect()` builds
//! what its uses decide, bound by a `let` (B-2026-11-09-40) or called where
//! it is written (B-2026-11-10-18).

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Checks clean and prints `stdout` on the MIR interpreter.
fn runs(name: &str, src: &str, stdout: &str) {
    let dir = std::env::temp_dir().join(format!("karac-collect-later-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        stdout,
        "{}",
        text(&run)
    );
}

#[test]
fn a_let_bound_collect_lent_to_a_borrowing_parameter_builds_its_type() {
    runs(
        "lent.kara",
        "fn count(v: Vec[char]) -> i64 { v.len() }
fn total(v: ref Vec[i64]) -> i64 { v.iter().sum() }
struct C { n: i64 }
impl C { fn count(self, v: Vec[char]) -> i64 { v.len() + self.n } }
fn main() {
    let cs = \"a b c\".chars().collect();
    let ns = (1..4).collect();
    let ds = \"xy\".chars().collect();
    let c = C { n: 10 };
    println(f\"{count(cs)} {total(ns)} {c.count(ds)}\");
}
",
        "5 6 12\n",
    );
}

#[test]
fn a_closure_ending_in_collect_builds_what_its_uses_decide() {
    runs(
        "closure.kara",
        "fn take(v: Vec[i32]) -> i64 { v.len() }
fn main() {
    let a = Vec[1, 2];
    let b = Vec[3];
    let to32 = |v: ref Vec[i64]| v.iter().map(|x| x as i32).collect();
    println(f\"{take(to32(a))} {take(to32(b))}\");
    let inc = |v: ref Vec[i64]| { v.iter().map(|x| x + 1).collect() };
    let r: Vec[i64] = inc(a);
    println(f\"{r[0]} {r.len()}\");
}
",
        "2 1\n2 2\n",
    );
}

#[test]
fn a_mapped_collect_lent_to_a_borrowing_parameter_builds_its_type() {
    runs(
        "mapped.kara",
        "fn count(v: Vec[i64]) -> i64 { v.len() }
fn words(v: Vec[String]) -> i64 { v.len() }
fn main() {
    let ns = Vec[1, 2].iter().map(|p| p + 1).collect();
    let ws = \"a b c\".split(\" \").map(|p| p.to_string()).collect();
    println(f\"{count(ns)} {words(ws)}\");
}
",
        "2 3\n",
    );
}

#[test]
fn a_collect_a_later_use_decides_still_checks_its_item_type() {
    let dir = std::env::temp_dir().join(format!("karac-collect-later-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, src) in [
        (
            "lent_bad.kara",
            "fn words(v: Vec[String]) -> i64 { v.len() }
fn main() {
    let ws = \"a b\".split(\" \").collect();
    println(f\"{words(ws)}\");
}
",
        ),
        (
            "let_bad.kara",
            "fn main() {
    let ws = \"a b\".split(\" \").collect();
    let w: Vec[String] = ws;
    println(f\"{w.len()}\");
}
",
        ),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, src).unwrap();
        let check = karac(&["check"], &path);
        assert!(!check.status.success(), "{name}: {}", text(&check));
        assert!(
            text(&check).contains(
                "`Vec[String]` implements `FromIterator[String]`, not `FromIterator[Str]`"
            ),
            "{name}: {}",
            text(&check)
        );
    }
}

#[test]
fn a_collect_used_before_the_use_that_decides_it_asks_for_the_binding_type() {
    let dir = std::env::temp_dir().join(format!("karac-collect-later-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("early.kara");
    std::fs::write(
        &path,
        "fn take(v: Vec[String]) -> i64 { v.len() }
fn main() {
    let ws = \"a b\".split(\" \").map(|p| p.to_string()).collect();
    let c = ws[0].clone();
    println(f\"{c} {take(ws)} {ws.len()}\");
}
",
    )
    .unwrap();
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    assert!(
        text(&check)
            .contains("cannot infer what this `collect` builds where it is first used, at 4:13"),
        "{}",
        text(&check)
    );
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{}", text(&fix));
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(fixed.contains("let ws: Vec[String] = "), "{fixed}");
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "a 2 2\n",
        "{}",
        text(&run)
    );
}

#[test]
fn a_closure_called_where_it_is_written_builds_what_the_call_decides() {
    runs(
        "called.kara",
        "fn take(v: Vec[i32]) -> i64 { v.len() }
fn main() {
    let a = Vec[1, 2, 2];
    let s: Set[i64] = (|v: ref Vec[i64]| v.iter().map(|x| x + 1).collect())(a);
    let n = take((|v: ref Vec[i64]| v.iter().map(|x| x as i32).collect())(a));
    println(f\"{s.len()} {n}\");
}
",
        "2 3\n",
    );
}
