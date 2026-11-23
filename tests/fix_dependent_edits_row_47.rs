//! One `karac fix` round does not apply an edit whose premise another edit
//! of the same round removes (B-2026-11-21-47). `let mut list =
//! g.get(a).unwrap_or(Vec.new()); list.push(b); g[a] = list;` offered
//! `.cloned()` in the `let` and `.clone()` at `g[a] = list`, the second
//! because `list` was still a reference; with the first applied `list` owns
//! its `Vec`, and the `.clone()` copied it for nothing with no lint left to
//! say so. And a `flat_map` closure whose value is `opt.unwrap_or(<a
//! temporary>)` gets the two edits that make it check, not an `.iter()`
//! that is refused in turn.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Every call gets a directory of its own, whatever names two tests pass.
static CALL: AtomicUsize = AtomicUsize::new(0);

fn scratch(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-fix-dependent-edits-{name}-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    dir
}

fn karac(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(dir.join("main.kara"))
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

/// `karac fix`, then the text it left, which must check clean, and what the
/// mended program prints.
fn mend(name: &str, src: &str) -> (String, String, String) {
    let dir = scratch(name, src);
    let fix = karac(&dir, &["fix"]);
    assert!(fix.status.success(), "{name}: {}", text(&fix));
    let mended = std::fs::read_to_string(dir.join("main.kara")).unwrap();
    let check = karac(&dir, &["check"]);
    assert!(check.status.success(), "{name}: {}", text(&check));
    let run = karac(&dir, &["__mir-run"]);
    let out = String::from_utf8_lossy(&run.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    (text(&fix), mended, out)
}

/// The row's first program: only `.cloned()` is written, and the store
/// moves the `Vec` the binding now owns.
#[test]
fn a_clone_whose_binding_another_fix_makes_owned_is_not_written() {
    let (said, mended, out) = mend(
        "list",
        "fn main() {\n    let mut g: Map[i64, Vec[i64]] = Map.new();\n    let a = 1;\n    let b = 2;\n    let mut list = g.get(a).unwrap_or(Vec.new());\n    list.push(b);\n    g[a] = list;\n    println(g[a].len());\n}\n",
    );
    assert!(said.contains("applied 1 fix(es)"), "{said}");
    assert!(
        mended.contains("    let mut list = g.get(a).cloned().unwrap_or(Vec.new());\n    list.push(b);\n    g[a] = list;\n"),
        "{mended}"
    );
    assert_eq!(out, "1\n");
}

/// With nothing rewriting the binding's `let`, the clone at the use is
/// written in the same round as an unrelated fix elsewhere: one pass
/// (`KARAC_FIX_PASSES=1`) writes both.
#[test]
fn a_clone_whose_binding_no_fix_touches_is_written_in_the_same_round() {
    let dir = scratch(
        "kept",
        "fn main() {\n    let mut g: Map[i64, Vec[i64]] = Map.new();\n    g[1] = [7].to_vec();\n    let mut out: Vec[Vec[i64]] = Vec.new();\n    let list = g.get(1).unwrap();\n    out.push(list);\n    let t: i64;\n    t = 1;\n    t = t + list.len() + out.len();\n    println(t);\n}\n",
    );
    let fix = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("fix")
        .arg(dir.join("main.kara"))
        .env("KARAC_FIX_PASSES", "1")
        .output()
        .unwrap();
    assert!(text(&fix).contains("applied 2 fix(es)"), "{}", text(&fix));
    let mended = std::fs::read_to_string(dir.join("main.kara")).unwrap();
    assert!(
        mended.contains("    let list = g.get(1).unwrap();\n"),
        "{mended}"
    );
    assert!(mended.contains("out.push(list.clone());"), "{mended}");
    assert!(mended.contains("    let mut t: i64;\n"), "{mended}");
    let check = karac(&dir, &["check"]);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&dir, &["__mir-run"]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "3\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The row's second program: the closure's value borrows the map's item or
/// a default that dies with the closure. One fix of two edits copies the
/// item out and iterates the value.
#[test]
fn a_flat_map_over_an_unwrap_or_default_is_cloned_and_iterated_by_value() {
    let src = "fn main() {\n    let mut graph: Map[i64, Vec[i64]] = Map.new();\n    graph[1] = [2, 3].to_vec();\n    let frontier: Vec[i64] = [1, 5].to_vec();\n    let next: Vec[i64] = frontier.iter().flat_map(|v| graph.get(v).unwrap_or(Vec.new())).collect();\n    println(next.len());\n}\n";
    let dir = scratch("flat-map-json", src);
    let json = karac(&dir, &["check", "--output=json"]);
    let doc: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let diags = doc["diagnostics"].as_array().unwrap();
    assert_eq!(diags.len(), 1, "{doc}");
    let message = diags[0]["message"].as_str().unwrap();
    assert!(
        message.contains(
            "copy the item out with `.cloned()` and iterate the value with `.into_iter()`"
        ),
        "{message}"
    );
    let edits: Vec<&str> = diags[0]["fix_diff"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["text"].as_str().unwrap())
        .collect();
    assert_eq!(edits, [".cloned()", ".into_iter()"], "{doc}");
    let _ = std::fs::remove_dir_all(&dir);

    let (_, mended, out) = mend("flat-map", src);
    assert!(
        mended.contains(
            "frontier.iter().flat_map(|v| graph.get(v).cloned().unwrap_or(Vec.new()).into_iter()).collect()"
        ),
        "{mended}"
    );
    assert_eq!(out, "2\n");
}

/// A closure whose value is a borrowed collection with no default to die
/// keeps the one-edit `.iter()`.
#[test]
fn a_flat_map_over_a_borrowed_collection_still_gets_iter() {
    let (_, mended, out) = mend(
        "plain",
        "fn main() {\n    let mut graph: Map[i64, Vec[i64]] = Map.new();\n    graph[1] = [2, 3].to_vec();\n    let frontier: Vec[i64] = [1].to_vec();\n    let next: Vec[i64] = frontier.iter().flat_map(|v| graph.get(v).unwrap()).collect();\n    println(next.len());\n}\n",
    );
    assert!(
        mended.contains("flat_map(|v| graph.get(v).unwrap().iter())"),
        "{mended}"
    );
    assert_eq!(out, "2\n");
}
