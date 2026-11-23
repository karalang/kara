//! `let (l, r) = segments.floor(x).unwrap();` over a `SortedMap[i64, i64]`
//! binds two borrows of the map, and each later `remove` / `insert` is
//! E0516 while they live. Both are `Copy`, so the copy is the whole remedy,
//! and `karac fix` writes it: `let (l, r) = (*l, *r);` after the statement
//! (B-2026-11-21-46). A part whose copy would be a deep clone gets no edit.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Every call gets a directory of its own, whatever names two tests pass.
static CALL: AtomicUsize = AtomicUsize::new(0);

fn scratch(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-held-tuple-borrow-{name}-{}-{}",
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

fn diagnostics(dir: &Path) -> Vec<serde_json::Value> {
    let out = karac(dir, &["check", "--output=json"]);
    let doc: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&out)));
    doc["diagnostics"].as_array().cloned().unwrap_or_default()
}

const SPLIT: &str = "fn main() {\n    let mut segments: SortedMap[i64, i64] = SortedMap.new();\n    segments.insert(0, 10);\n    let x = 4;\n    let (l, r) = segments.floor(x).unwrap();\n    segments.remove(l);\n    segments.insert(l, x);\n    segments.insert(x, r);\n    println(f\"{segments.len()} {l} {r}\");\n}\n";

/// The row's program: three E0516s, each carrying the one rebinding, which
/// `karac fix` writes once; the program then checks clean and runs.
#[test]
fn a_tuple_of_copy_borrows_is_rebound_to_its_values_by_fix() {
    let dir = scratch("split", SPLIT);
    let diags = diagnostics(&dir);
    assert_eq!(diags.len(), 3, "{diags:?}");
    for d in &diags {
        assert_eq!(d["code"], "E0516", "{d}");
        assert_eq!(
            d["replacement"]["text"], "\n    let (l, r) = (*l, *r);",
            "{d}"
        );
    }
    let fix = karac(&dir, &["fix"]);
    assert!(fix.status.success(), "{}", text(&fix));
    assert!(text(&fix).contains("applied 1 fix(es)"), "{}", text(&fix));
    let mended = std::fs::read_to_string(dir.join("main.kara")).unwrap();
    assert!(
        mended.contains(
            "    let (l, r) = segments.floor(x).unwrap();\n    let (l, r) = (*l, *r);\n    segments.remove(l);\n"
        ),
        "{mended}"
    );
    let check = karac(&dir, &["check"]);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&dir, &["__mir-run"]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "2 0 10\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Only the names bound to a borrow are rebound: a `_` part and an owned
/// part are left as they are, and one held name is rebound on its own.
#[test]
fn only_the_names_that_hold_a_borrow_are_rebound() {
    let dir = scratch(
        "one",
        "fn main() {\n    let mut m: SortedMap[i64, i64] = SortedMap.new();\n    m.insert(1, 2);\n    let (k, _) = m.first().unwrap();\n    m.insert(k + 1, 5);\n    println(f\"{m.len()} {k}\");\n}\n",
    );
    let diags = diagnostics(&dir);
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(
        diags[0]["replacement"]["text"], "\n    let k = *k;",
        "{diags:?}"
    );
    let fix = karac(&dir, &["fix"]);
    assert!(fix.status.success(), "{}", text(&fix));
    let check = karac(&dir, &["check"]);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&dir, &["__mir-run"]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "2 1\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `String` value's copy is a deep clone, which is the author's to choose:
/// the pattern gets no edit, and `karac fix` leaves the file.
#[test]
fn a_tuple_holding_a_borrow_that_is_not_copy_gets_no_edit() {
    let src = "fn main() {\n    let mut m: SortedMap[i64, String] = SortedMap.new();\n    m.insert(0, \"a\".to_string());\n    let (k, v) = m.floor(4).unwrap();\n    m.remove(k);\n    println(f\"{m.len()} {k} {v}\");\n}\n";
    let dir = scratch("string", src);
    let diags = diagnostics(&dir);
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0]["code"], "E0516", "{diags:?}");
    assert!(
        diags[0].get("replacement").is_none() && diags[0].get("fix_diff").is_none(),
        "{diags:?}"
    );
    let _ = karac(&dir, &["fix"]);
    assert_eq!(std::fs::read_to_string(dir.join("main.kara")).unwrap(), src);
    let _ = std::fs::remove_dir_all(&dir);
}
