//! B-2026-11-05-29: design.md §17. E0400 carries `inferred_effects`,
//! `declared_effects` and a `derivation` (rule 5) and a fix that declares
//! the effects; a non-exhaustive match is fixed with every missing arm in
//! one edit; `karac query effects` and `query ownership` answer with a
//! `derivation`; `karac query queries` with no target reports the package.

use std::path::{Path, PathBuf};
use std::process::Command;

fn karac(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("karac")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-deriv-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

const CHAIN: &str = "fn inner() { println(\"x\"); }
fn mid() { inner(); }
pub fn top() { mid(); }
fn main() { top(); }
";

#[test]
fn e0400_carries_its_effects_and_derivation_and_fix_declares_them() {
    let dir = scratch("e0400");
    std::fs::write(dir.join("a.kara"), CHAIN).unwrap();
    let out = karac(&dir, &["check", "--output=json", "a.kara"]);
    let doc = json(&out);
    let d = doc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "E0400")
        .expect("an E0400");
    assert_eq!(d["inferred_effects"], serde_json::json!(["writes(Stdout)"]));
    assert_eq!(d["declared_effects"], serde_json::json!([]));
    let reasons: Vec<(u64, &str)> = d["derivation"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["line"].as_u64().unwrap(), s["reason"].as_str().unwrap()))
        .collect();
    assert_eq!(
        reasons,
        vec![
            (3, "top calls mid"),
            (2, "mid calls inner"),
            (1, "inner calls println, which has writes(Stdout)"),
        ]
    );
    assert_eq!(
        d["hints"][0]["diff"]["new"],
        "pub fn top() with writes(Stdout) { mid(); }"
    );

    let fix = karac(&dir, &["fix", "a.kara"]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(dir.join("a.kara")).unwrap();
    assert!(
        fixed.contains("pub fn top() with writes(Stdout) { mid(); }"),
        "{fixed}"
    );
    let check = karac(&dir, &["check", "a.kara"]);
    assert!(
        check.status.success(),
        "{fixed}\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
}

#[test]
fn e0400_fix_appends_to_an_incomplete_clause_once_per_function() {
    let dir = scratch("clause");
    let src = "fn out() { println(\"x\"); eprintln(\"y\"); }
pub fn top() -> i64 with reads(Stdin) { out(); 2 }
struct S { n: i64 }
impl S {
    fn p(self) with reads(Stdin) { out(); }
}
fn main() { println(top()); S { n: 1 }.p(); }
";
    std::fs::write(dir.join("b.kara"), src).unwrap();
    let doc = json(&karac(&dir, &["check", "--output=json", "b.kara"]));
    let top: Vec<_> = doc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] == "E0400" && d["message"].as_str().unwrap().contains("'top'"))
        .collect();
    assert_eq!(top.len(), 2, "one E0400 per undeclared effect: {doc}");
    assert_eq!(
        top[0]["declared_effects"],
        serde_json::json!(["reads(Stdin)"])
    );
    assert_eq!(
        top.iter()
            .filter(|d| d.get("replacement").is_some())
            .count(),
        1,
        "one edit per function, so fix never inserts twice at one place: {doc}"
    );
    let fix = karac(&dir, &["fix", "b.kara"]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(dir.join("b.kara")).unwrap();
    assert!(
        fixed.contains("pub fn top() -> i64 with reads(Stdin) writes(Stderr) writes(Stdout) {"),
        "{fixed}"
    );
    assert!(
        fixed.contains("fn p(self) with reads(Stdin) writes(Stderr) writes(Stdout) {"),
        "{fixed}"
    );
    let check = karac(&dir, &["check", "b.kara"]);
    assert!(
        check.status.success(),
        "{fixed}\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
}

#[test]
fn a_match_missing_several_arms_is_fixed_in_one_pass() {
    let dir = scratch("arms");
    let src = "enum C { Red, Green, Blue(i64) }
fn name(c: C) -> i64 {
    match c {
        C.Red => 1,
    }
}
fn flag(b: bool) -> i64 {
    match b {
        true => 1,
    }
}
fn pair(c: C, b: bool) -> i64 {
    match (c, b) {
        (C.Red, _) => 1,
        (C.Green, true) => 2,
        (C.Blue(_), _) => 3,
    }
}
fn main() { println(name(C.Green) + flag(false) + pair(C.Red, true)); }
";
    std::fs::write(dir.join("m.kara"), src).unwrap();
    let doc = json(&karac(&dir, &["check", "--output=json", "m.kara"]));
    let messages: Vec<&str> = doc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["message"].as_str().unwrap())
        .collect();
    assert!(
        messages.contains(&"non-exhaustive match: patterns `Green`, `Blue(_)` not covered"),
        "{messages:?}"
    );
    let fix = karac(&dir, &["fix", "m.kara"]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(dir.join("m.kara")).unwrap();
    assert!(
        fixed.contains("Green => todo(), Blue(_) => todo()"),
        "{fixed}"
    );
    assert!(fixed.contains("false => todo()"), "{fixed}");
    assert!(fixed.contains("(Green, false) => todo()"), "{fixed}");
    let check = karac(&dir, &["check", "m.kara"]);
    assert!(
        check.status.success(),
        "{fixed}\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
}

#[test]
fn query_effects_and_ownership_answer_with_a_derivation() {
    let dir = scratch("query");
    std::fs::write(dir.join("a.kara"), CHAIN).unwrap();
    let doc = json(&karac(&dir, &["query", "effects", "a.kara.top"]));
    let steps = doc["derivation"].as_array().unwrap();
    assert_eq!(steps.len(), 3, "{doc}");
    assert_eq!(steps[0]["reason"], "top calls mid");
    assert_eq!(steps[0]["file"], "a.kara");

    let src = "struct P { v: Vec[i64] }
fn keep(v: own Vec[i64]) -> Vec[i64] { v }
fn take(p: own P) -> i64 {
    let n = 1;
    let q = keep(p.v);
    q.len() + n
}
fn look(v: Vec[i64]) -> i64 { v.len() }
fn main() { let v = vec![1]; println(look(v)); println(take(P { v: v })); }
";
    std::fs::write(dir.join("o.kara"), src).unwrap();
    let take = json(&karac(&dir, &["query", "ownership", "o.kara.take"]));
    assert_eq!(
        take["derivation"],
        serde_json::json!([{"file": "o.kara", "line": 5, "reason": "`p` is moved here, so the body takes it"}]),
        "{take}"
    );
    let look = json(&karac(&dir, &["query", "ownership", "o.kara.look"]));
    assert_eq!(
        look["derivation"][0]["reason"], "the body never moves `v`, so a borrow will do",
        "{look}"
    );
}

#[test]
fn query_queries_with_no_target_reports_the_package_here() {
    let dir = scratch("queries");
    std::fs::create_dir_all(dir.join("pkg/src")).unwrap();
    std::fs::write(
        dir.join("pkg/kara.toml"),
        "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("pkg/src/main.kara"), "fn main() { println(1); }\n").unwrap();
    let out = karac(&dir.join("pkg"), &["query", "queries"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(json(&out)["queries"].is_array());
    let nested = karac(&dir.join("pkg/src"), &["query", "queries"]);
    assert!(
        nested.status.success(),
        "{}",
        String::from_utf8_lossy(&nested.stderr)
    );

    let none = karac(&dir, &["query", "queries"]);
    assert!(!none.status.success());
    assert!(
        String::from_utf8_lossy(&none.stderr).contains("there is none here"),
        "{}",
        String::from_utf8_lossy(&none.stderr)
    );
}
