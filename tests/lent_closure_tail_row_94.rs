//! A closure that hands back the item `iter()` lent it, where the slot takes
//! the value: Rust's `[a, b].iter().filter_map(|child| child)`. A `Copy`
//! item is read as its value, as a fn's tail and a `let` read it; any other
//! gets one message at the tail saying so, with `.clone()` as its edit
//! where the item is `Clone`. Both were refused with a bare "expected
//! 'Option[_]', found 'ref Option[..]'" and no help (B-2026-11-18-94).

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Every call gets a directory of its own, whatever names two tests pass.
static CALL: AtomicUsize = AtomicUsize::new(0);

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-lent-closure-tail-{name}-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn karac(name: &str, src: &str, cmd: &str) -> std::process::Output {
    let dir = scratch(&format!("{name}-{cmd}"));
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `karac check` accepts `src`, and the interpreter and, built with the
/// backend, the compiled program and its ASan build print `want`.
fn runs_everywhere(name: &str, src: &str, want: &str) {
    let check = karac(name, src, "check");
    assert!(check.status.success(), "{name}: check: {}", text(&check));
    let cmds: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run", "__mir-llvm-asan"]
    } else {
        &["__mir-run"]
    };
    for cmd in cmds {
        let out = karac(name, src, cmd);
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        // Only a failed link may skip: a refusal, a runtime error or a
        // sanitizer report is the program's own.
        if *cmd != "__mir-run" {
            let linked = common::link_or_skip(if out.status.success() {
                Ok(())
            } else {
                Err(err.clone())
            });
            if linked.is_none() {
                continue;
            }
        }
        assert!(out.status.success(), "{name}: {cmd}: {err}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}: {cmd}");
    }
}

/// An `Option[i64]` item is `Copy`: `filter_map(|child| child)` over
/// `iter()` reads it as its value, behind a `map` too, and the collection
/// is still there afterwards.
#[test]
fn a_closure_handing_back_a_lent_copy_item_returns_its_value() {
    runs_everywhere(
        "copy",
        "fn main() {\n    let a: Option[i64] = Some(1);\n    let b: Option[i64] = None;\n    let c: Option[i64] = Some(3);\n    let xs: Vec[i64] = [a, b, c].iter().filter_map(|child| child).collect();\n    println(f\"{xs.len()} {xs[0] + xs[1]}\");\n    let v: Vec[Option[i64]] = [Some(5), None, Some(7)].to_vec();\n    let ys: Vec[i64] = v.iter().map(|child| child).filter_map(|c| c).collect();\n    let zs: Vec[Option[i64]] = v.iter().map(|child| child).collect();\n    println(f\"{ys[0] + ys[1]} {zs.len()} {v.len()}\");\n}\n",
        "2 4\n12 3 3\n",
    );
}

const LENT_STRINGS: &str = "fn main() {\n    let a: Option[String] = Some(\"x\".to_string());\n    let b: Option[String] = None;\n    let v: Vec[Option[String]] = [a, b].to_vec();\n    let xs: Vec[String] = v.iter().filter_map(|child| child).collect();\n    println(f\"{xs.len()} {xs[0]} {v.len()}\");\n}\n";

/// An `Option[String]` item is not `Copy`: one error, at the tail, that
/// names the borrow and both ways out, and carries `.clone()`.
#[test]
fn a_closure_handing_back_a_lent_string_item_is_told_so_once_with_the_clone() {
    let out = karac("string", LENT_STRINGS, "check");
    let shown = text(&out);
    assert!(!out.status.success(), "{shown}");
    assert!(
        shown.contains(":5:55: expected 'Option[_]', found 'ref Option[String]': the closure hands back the item it was lent, and the slot takes the value"),
        "{shown}"
    );
    assert!(
        shown.contains("help: copy it out with `.clone()`, or iterate by value (`into_iter()` in place of `iter()`) when the collection is not used afterwards"),
        "{shown}"
    );
    // The call is not refused a second time as a whole.
    assert!(shown.contains("1 error(s) found."), "{shown}");
    assert!(!shown.contains("expected 'Fn("), "{shown}");
}

/// `karac fix` writes the `.clone()`, and the program then checks and runs.
#[test]
fn karac_fix_clones_the_lent_item_and_the_program_runs() {
    let dir = scratch("fix");
    let file = dir.join("main.kara");
    std::fs::write(&file, LENT_STRINGS).unwrap();
    let karac = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(args)
            .arg(&file)
            .output()
            .unwrap()
    };
    let json = karac(&["check", "--output=json"]);
    let doc: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let d = &doc["diagnostics"][0];
    assert_eq!(d["replacement"]["text"], ".clone()", "{d}");
    let fix = karac(&["fix"]);
    assert!(fix.status.success(), "{}", text(&fix));
    let mended = std::fs::read_to_string(&file).unwrap();
    assert!(
        mended.contains("v.iter().filter_map(|child| child.clone()).collect()"),
        "{mended}"
    );
    let check = karac(&["check"]);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "1 x 2\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// An item with no `.clone()` gets the message and the `into_iter()` way
/// out, and no edit.
#[test]
fn a_lent_item_that_is_not_clone_gets_the_help_and_no_edit() {
    let dir = scratch("noclone");
    let file = dir.join("main.kara");
    std::fs::write(
        &file,
        "struct Tok {\n    n: i64,\n}\nfn main() {\n    let w: Vec[Option[Tok]] = [Some(Tok { n: 1 })];\n    let zs: Vec[Tok] = w.iter().filter_map(|c| c).collect();\n    println(zs.len());\n}\n",
    )
    .unwrap();
    let json = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "--output=json"])
        .arg(&file)
        .output()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let diags = doc["diagnostics"].as_array().unwrap();
    assert_eq!(diags.len(), 1, "{doc}");
    let d = &diags[0];
    let message = d["message"].as_str().unwrap();
    assert!(
        message.contains("found 'ref Option[Tok]': the closure hands back the item it was lent"),
        "{d}"
    );
    assert!(
        message.contains("help: iterate by value (`into_iter()` in place of `iter()`)"),
        "{d}"
    );
    assert!(!message.contains(".clone()"), "{d}");
    assert!(
        d.get("replacement").is_none() && d.get("fix_diff").is_none(),
        "{d}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The value way out the help names: `into_iter()` moves the items out.
#[test]
fn into_iter_hands_the_closure_the_item_itself() {
    runs_everywhere(
        "into-iter",
        "fn main() {\n    let v: Vec[Option[String]] = [Some(\"x\".to_string()), None].to_vec();\n    let xs: Vec[String] = v.into_iter().filter_map(|child| child).collect();\n    println(f\"{xs.len()} {xs[0]}\");\n}\n",
        "1 x\n",
    );
}
