//! Kata 1181's arms. The ★ arm takes each phrase's first word with
//! `match p.split_once(" ") { Some((w, _)) => w, None => p }` over a
//! `ref String` `p`: a `ref String` is a `Str` only as a call argument
//! (design.md §8), so the arms disagree, and the refusal had no fix and no
//! hint, since the `.as_str()` edit covered only an explicit `ref x` arm.
//! Now a bare place (a name, a field, an index, a call) takes `.as_str()`,
//! and `if let` / `else` gets the fix `if` / `else` already had.
//! B-2026-11-17-69.
//!
//! The chain arm ends `(0..n).flat_map(..).collect::<BTreeSet<_>>()
//! .into_iter().collect()`. The fix names the collection as
//! `SortedSet.from_iter(it)`, but a parenthesized expression keeps its inner
//! span, so the call opened inside the parentheses:
//! `(SortedSet.from_iter(0..n).flat_map(..))`, refused as no `flat_map` on
//! `SortedSet`. Now the edit starts before each `(` that closes inside the
//! receiver; the `parse::<T>()` fix took the same span and is mended too.
//! B-2026-11-17-70.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let fixed = std::fs::read_to_string(dir.join("cell.kara")).unwrap_or_default();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        fixed,
    )
}

fn cell(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-kata-1181-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    dir
}

fn modes() -> Vec<&'static str> {
    let mut m = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        m.push("__mir-llvm-run");
    }
    m
}

/// Refused, fixed in one round to contain every line of `want`, then clean
/// and running to `expected` under every backend.
fn mends_to(tag: &str, src: &str, want: &[&str], expected: &str) {
    let dir = cell(tag, src);
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(!ok, "refused as written: {out}");
    let (_, out, fixed) = karac(&dir, &["fix"]);
    assert!(out.contains("applied"), "{out}");
    for w in want {
        assert!(fixed.contains(w), "`{w}` in:\n{fixed}");
    }
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(ok, "{out}\n{fixed}");
    for mode in modes() {
        let (ok, out, _) = karac(&dir, &[mode]);
        assert!(ok, "{mode}: {out}");
        assert_eq!(out, expected, "{mode}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `None => p` over a `ref String` beside a `Str` arm: `p.as_str()`.
#[test]
fn kata_1181_bare_ref_string_match_arm_takes_as_str() {
    let src = "fn firsts(phrases: Vec[String]) -> Vec[String] {\n    \
               let mut out: Vec[String] = Vec.new();\n    for p in phrases.iter() {\n        \
               let first = match p.split_once(\" \") {\n            Some((w, _)) => w,\n            \
               None => p,\n        };\n        out.push(first.to_string());\n    }\n    out\n}\n\n\
               fn main() {\n    \
               println(f\"{firsts([\"a b\", \"c\", \"d e f\"].iter().map(|s| s.to_string()).collect())}\");\n}\n";
    mends_to("match", src, &["None => p.as_str(),"], "[a, c, d]\n");
}

/// The same in `if let` / `else`, which had neither the fix nor the hint.
#[test]
fn kata_1181_bare_ref_string_if_let_arm_takes_as_str() {
    let src = "fn ends(phrases: Vec[String]) -> Vec[String] {\n    \
               let mut out: Vec[String] = Vec.new();\n    for p in phrases.iter() {\n        \
               let last = if let Some((_, w)) = p.rsplit_once(\" \") { w } else { p };\n        \
               out.push(last.to_string());\n    }\n    out\n}\n\n\
               fn main() {\n    \
               println(f\"{ends([\"a b\", \"c\", \"d e f\"].iter().map(|s| s.to_string()).collect())}\");\n}\n";
    let dir = cell("iflet", src);
    let (_, out, _) = karac(&dir, &["check"]);
    assert!(
        out.contains("a `ref String` is a `Str` only as a call argument"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    mends_to("iflet2", src, &["else { p.as_str() }"], "[b, c, f]\n");
}

/// A parenthesized head of a receiver goes inside `from_iter(..)`; a
/// parenthesis around the whole receiver, or an outer call's, stays
/// outside, as `turbofish_on_parse_is_rewritten_to_an_associated_call`
/// pins.
#[test]
fn kata_1181_turbofish_fix_wraps_a_parenthesized_receiver() {
    let src = "fn evens(n: i64) -> Vec[i64] {\n    \
               (0..n).map(|i| i * 2 % 6).collect::<SortedSet<_>>().into_iter().collect()\n}\n\n\
               fn count(n: i64) -> i64 {\n    \
               sum_of((0..n).map(|i| i % 3).collect::<Vec<_>>().len())\n}\n\n\
               fn sum_of(x: i64) -> i64 {\n    x + 100\n}\n\n\
               fn parsed(s: String) -> i64 {\n    \
               (s.trim()).parse::<i64>().unwrap() + ((s.trim())).parse::<i64>().unwrap()\n}\n\n\
               fn main() {\n    println(f\"{evens(5)} {count(4)} {parsed(\" 21 \".to_string())}\");\n}\n";
    mends_to(
        "paren",
        src,
        &[
            "SortedSet.from_iter((0..n).map(|i| i * 2 % 6)).into_iter()",
            "sum_of(Vec.from_iter((0..n).map(|i| i % 3)).len())",
            "(i64.parse(s.trim())).unwrap() + (i64.parse((s.trim()))).unwrap()",
        ],
        "[0, 2, 4] 104 42\n",
    );
}

/// Beside string literals alone, which are `String`s by default, a bare
/// `ref String` arm keeps the clone advice: the owned arms win.
#[test]
fn kata_1181_bare_ref_string_among_literals_keeps_clone_advice() {
    let src = "enum E { A, B(String) }\n\n\
               fn name(e: E) -> String {\n    let msg = match e {\n        E.A => \"plain\",\n        \
               E.B(m) => m,\n    };\n    msg\n}\n\n\
               fn main() {\n    println(f\"{name(E.A)} {name(E.B(\"x\".to_string()))}\");\n}\n";
    let dir = cell("literals", src);
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(!ok, "{out}");
    assert!(out.contains("Write `.clone()` to take a copy"), "{out}");
    assert!(!out.contains(".as_str()"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
