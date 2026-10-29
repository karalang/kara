//! A `String` method takes text as `Str` and characters and counts by value
//! (docs/library/strings.md), so a capture passed to one is read, not moved,
//! and the closure stays `Fn`. The closure-kind walker knew only `contains`,
//! `starts_with` and `ends_with`, and only on a receiver that is a captured
//! name, so `|s| s.replace(old, new)` (B-2026-10-29-2) and
//! `|s| s.to_uppercase().contains(upper)` (B-2026-10-28-28) were typed
//! `OnceFn` and refused by `map`. `extend` takes `own I` and still consumes.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-strargs-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn check_and_run(name: &str, src: &str) -> String {
    let (ok, err) = check(name, src);
    assert!(ok, "{name}: {err}");
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn a_capture_passed_to_replace_keeps_the_closure_fn() {
    let src = "fn main() {
    let cells: Vec[String] = [\"a-b\".to_string(), \"c-d-e\".to_string()].to_vec();
    let old = \"-\".to_string();
    let new = \"+\".to_string();
    let out: Vec[String] = cells.iter().map(|s| s.replace(old, new)).collect();
    println(out.join(\",\"));
    println(old);
}
";
    assert_eq!(check_and_run("replace", src), "a+b,c+d+e\n-\n");
}

#[test]
fn a_capture_passed_to_a_method_of_a_call_s_result_keeps_the_closure_fn() {
    let src = "fn hits(cells: Vec[String], pat: Str) -> Vec[bool] {
    let upper = pat.to_uppercase();
    cells.iter().map(|s| s.to_uppercase().contains(upper)).collect()
}

fn main() {
    let cells: Vec[String] = [\"Acme Co\".to_string(), \"globex\".to_string(), \"Zo co\".to_string()].to_vec();
    println(hits(cells, \"co\"));
}
";
    assert_eq!(check_and_run("upper", src), "[true, false, true]\n");
}

#[test]
fn every_str_argument_method_reads_its_capture() {
    for (name, body) in [
        ("replacen", "s.replacen(old, \"+\", 1).len()"),
        ("split", "s.split(old).count()"),
        ("find", "if s.find(old).is_some() { 1 } else { 0 }"),
        (
            "strip_prefix",
            "if s.strip_prefix(old).is_some() { 1 } else { 0 }",
        ),
        (
            "trim_contains",
            "if s.trim().contains(old) { 1 } else { 0 }",
        ),
    ] {
        let src = format!(
            "fn main() {{
    let cells: Vec[String] = [\"-a\".to_string(), \"b\".to_string()].to_vec();
    let old = \"-\".to_string();
    let n: Vec[i64] = cells.iter().map(|s| {body}).collect();
    println(n.len());
    println(old);
}}
"
        );
        assert_eq!(check_and_run(name, &src), "2\n-\n", "{name}");
    }
}

#[test]
fn a_capture_extended_into_a_string_still_makes_the_closure_once_callable() {
    let src = "fn main() {
    let cells: Vec[String] = [\"a\".to_string()].to_vec();
    let more: Vec[String] = [\"b\".to_string()].to_vec();
    let n: Vec[i64] = cells.iter().map(|s| { let mut t = s.to_string(); t.extend(more); t.len() }).collect();
    println(n.len());
}
";
    let (ok, err) = check("extend", src);
    assert!(!ok, "check must refuse:\n{err}");
    assert!(err.contains("consumes captured binding 'more'"), "{err}");
}
