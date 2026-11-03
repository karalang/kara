//! Rust's `&` sigil had no fix in any of its three places. `for &(x, y) in
//! v.iter()` and `|&x| ..` said only "Expected pattern, found `&`", and
//! the parse failure cascaded into errors about statements outside `main`;
//! `f(&x)` and `let r = &x;` said "Expected expression, found `&`"
//! (B-2026-11-03-27, kata 1042). Each now has a machine-applicable fix: a
//! pattern and an argument drop the `&` (an argument's `&mut x` becomes
//! the call-site marker `mut x`), and a borrow becomes `ref x` or
//! `mut ref x`.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// Runs `karac fix` on `src` and returns the fixed text and what
/// `karac __mir-run` prints from it.
fn fixed(tag: &str, src: &str) -> (String, String) {
    let path =
        std::env::temp_dir().join(format!("karac-rust-amp-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).expect("write");
    let fix = karac(&["fix"], &path);
    assert!(
        fix.status.success(),
        "{tag}: fix: {}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let text = std::fs::read_to_string(&path).expect("read");
    let check = karac(&["check"], &path);
    let run = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        check.status.success(),
        "{tag}: check after fix: {}\n{text}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(
        run.status.success(),
        "{tag}: run: {}\n{text}",
        String::from_utf8_lossy(&run.stderr)
    );
    (text, String::from_utf8_lossy(&run.stdout).into_owned())
}

/// The diagnostics `karac check --output=json` reports on `src`, as
/// (message, replacement text) pairs.
fn diagnostics(tag: &str, src: &str) -> Vec<(String, Option<String>)> {
    let path = std::env::temp_dir().join(format!(
        "karac-rust-amp-check-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = karac(&["check", "--output=json"], &path);
    let _ = std::fs::remove_file(&path);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("check --output=json");
    json["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| {
            (
                d["message"].as_str().unwrap_or("").to_string(),
                d["replacement"]["text"].as_str().map(str::to_string),
            )
        })
        .collect()
}

#[test]
fn rust_amp_pattern_in_a_for_loop_is_one_diagnostic_with_a_fix() {
    let src = "fn main() {\n    let paths = vec![(1, 2), (3, 4)];\n    let mut total = 0;\n    for &(x, y) in paths.iter() {\n        total += x * y;\n    }\n    println(f\"{total}\");\n}\n";
    let found = diagnostics("for", src);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].0.contains("`&` before a pattern is Rust syntax"),
        "{found:?}"
    );
    assert_eq!(found[0].1.as_deref(), Some(""), "{found:?}");
    let (text, out) = fixed("for", src);
    assert!(text.contains("for (x, y) in paths.iter()"), "{text}");
    assert_eq!(out, "14\n");
}

#[test]
fn rust_amp_patterns_in_closures_match_arms_and_mut_drop_the_sigil() {
    let (text, out) = fixed(
        "pats",
        "fn main() {\n    let v = vec![3, 1, 2];\n    let big: Vec[i64] = v.iter().filter(|&x| x > 1).collect();\n    let pairs = vec![(1, 2), (5, 6)];\n    let firsts: Vec[i64] = pairs.iter().map(|&(a, _)| a).collect();\n    let mut w = vec![1, 2];\n    let mut total = 0;\n    for &mut z in w.iter_mut() {\n        total += z;\n    }\n    let t = match v[0] {\n        3 => 30,\n        _ => 0,\n    };\n    println(f\"{big} {firsts} {total} {t}\");\n}\n",
    );
    assert!(text.contains(".filter(|x| x > 1)"), "{text}");
    assert!(text.contains(".map(|(a, _)| a)"), "{text}");
    assert!(text.contains("for z in w.iter_mut()"), "{text}");
    assert_eq!(out, "[3, 2] [1, 5] 3 30\n");
}

#[test]
fn rust_amp_on_an_argument_drops_and_amp_mut_becomes_the_mut_marker() {
    let (text, out) = fixed(
        "args",
        "fn total(v: ref Vec[i64]) -> i64 {\n    let mut s = 0;\n    for x in v.iter() {\n        s += x;\n    }\n    s\n}\n\nfn bump(n: mut ref i64) {\n    *n += 1;\n}\n\nfn main() {\n    let v = vec![3, 1, 2];\n    let mut n = 0;\n    bump(&mut n);\n    bump(& mut n);\n    println(f\"{total(&v)} {n}\");\n}\n",
    );
    assert!(text.contains("bump(mut n);\n    bump(mut n);"), "{text}");
    assert!(text.contains("{total(v)}"), "{text}");
    assert_eq!(out, "6 2\n");
}

#[test]
fn rust_amp_borrow_expression_becomes_ref_and_mut_ref() {
    let (text, out) = fixed(
        "borrow",
        "fn main() {\n    let v = vec![3, 1, 2];\n    let r = &v;\n    let mut n = 5;\n    let w = &mut n;\n    *w += 1;\n    let m = match &v[0] {\n        3 => 30,\n        _ => 0,\n    };\n    println(f\"{r} {n} {m}\");\n}\n",
    );
    assert!(text.contains("let r = ref v;"), "{text}");
    assert!(text.contains("let w = mut ref n;"), "{text}");
    assert!(text.contains("match ref v[0]"), "{text}");
    assert_eq!(out, "[3, 1, 2] 6 30\n");
}
