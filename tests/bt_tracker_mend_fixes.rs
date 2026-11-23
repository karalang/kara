//! Four `karac fix` repairs the bt_tracker app needed by hand, each a
//! Rust habit refused with no machine-applicable fix:
//!
//! - `let n: i64 = s.parse().map_err(f)?`: the untargeted `s.parse()` was an
//!   `Option`, so `map_err` stopped the check before the `let` named the
//!   target; it is typed as the `Result` its fix makes (B-2026-11-23-61).
//! - `u8.from_str_radix(h, 16).ok()?`: the call is already an `Option`, so
//!   the fix drops `.ok()` (B-2026-11-23-62).
//! - a bare `for` over a bare parameter filling a local map: the map holds
//!   borrows, refused at its use; the fix takes the parameter `own` and
//!   iterates it `.into_iter()` (B-2026-11-23-63).
//! - `let d = [("ip", 1)]` passed on as `Vec[(Str, i64)]`: the literals
//!   defaulted to `String`; the fix annotates the `let` (B-2026-11-23-64).

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// `karac check` refuses `src` with a message holding `says`; `karac fix`
/// repairs it so that it checks clean, contains `want`, and prints `out`
/// under `__mir-run`.
fn fixed_to(tag: &str, src: &str, says: &str, want: &str, out: &str) {
    let path = std::env::temp_dir().join(format!(
        "karac-bt-tracker-mend-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let before = karac(&["check"], &path);
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&before.stdout),
        String::from_utf8_lossy(&before.stderr)
    );
    assert!(!before.status.success(), "{tag}: check accepted it");
    assert!(said.contains(says), "{tag}: {said}");
    let fix = karac(&["fix"], &path);
    assert!(
        fix.status.success(),
        "{tag}: fix: {}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let text = std::fs::read_to_string(&path).expect("read");
    let after = karac(&["check"], &path);
    let run = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        after.status.success(),
        "{tag}: still refused after the fix:\n{}\n{text}",
        String::from_utf8_lossy(&after.stdout)
    );
    assert!(text.contains(want), "{tag}: {text}");
    assert_eq!(String::from_utf8_lossy(&run.stdout), out, "{tag}");
}

#[test]
fn a_parse_whose_let_names_the_target_through_map_err_gets_the_typed_call() {
    fixed_to(
        "parse",
        G1,
        "here only a later use names it (`i64`)",
        "let n: i64 = i64.parse(digits).map_err(",
        "Ok(42)\n",
    );
}

#[test]
fn ok_on_a_call_that_is_already_an_option_is_dropped() {
    fixed_to(
        "ok",
        G2,
        "this is already an `Option`",
        "let b = u8.from_str_radix(h, 16)?;",
        "Some(255)\n",
    );
}

#[test]
fn a_map_filled_from_a_bare_parameter_loop_gets_own_and_into_iter() {
    fixed_to(
        "fill",
        G3,
        "`map` holds borrows: `v` is an element of `pairs`",
        "for (k, v) in pairs.into_iter() {",
        "1\n",
    );
}

#[test]
fn a_literal_list_let_passed_as_a_str_tuple_list_gets_annotated() {
    fixed_to(
        "lits",
        G4,
        "Nothing at `d`'s `let` names `Str`",
        "let mut d: Vec[(Str, i64)] = [",
        "3\n",
    );
}

const G1: &str = "fn num(digits: Str) -> Result[i64, String] {\n    let n: i64 = digits.parse().map_err(|_| f\"bad integer {digits}\")?;\n    Ok(n)\n}\n\nfn main() {\n    println(f\"{num(\"42\")}\");\n}\n";

const G2: &str = "fn byte(h: Str) -> Option[u8] {\n    let b = u8.from_str_radix(h, 16).ok()?;\n    Some(b)\n}\n\nfn main() {\n    println(f\"{byte(\"ff\")}\");\n}\n";

const G3: &str = "enum Value {\n    Int(i64),\n    Dict(SortedMap[String, Value]),\n}\n\nfn dict(pairs: Vec[(Str, Value)]) -> Value {\n    let mut map = SortedMap.new();\n    for (k, v) in pairs {\n        map.insert(k.to_string(), v);\n    }\n    Value.Dict(map)\n}\n\nfn main() {\n    match dict([(\"a\", Value.Int(1))]) {\n        Value.Dict(m) => println(f\"{m.len()}\"),\n        Value.Int(_) => println(\"int\"),\n    }\n}\n";

const G4: &str = "fn count(pairs: Vec[(Str, i64)]) -> i64 {\n    pairs.len()\n}\n\nfn main() {\n    let mut d = [(\"ip\", 1), (\"port\", 2)];\n    d.push((\"peer id\", 3));\n    println(f\"{count(d)}\");\n}\n";
