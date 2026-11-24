//! `m.entry(k)` on a map whose keys are references lends `k`, as `insert`
//! does: the MIR builder borrows the key, and `karac check` read it as a
//! move, so a local key was "used after a move" and an element key was
//! "moved out of its collection" (B-2026-11-23-12). A temporary key is
//! refused where it is written, as for `insert`.

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Every call gets a directory of its own, whatever names two tests pass.
static CALL: AtomicUsize = AtomicUsize::new(0);

fn karac(name: &str, src: &str, cmd: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-entry-key-lent-{name}-{cmd}-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
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

/// `karac check` accepts `src`, and the interpreter and, built with the
/// backend, the compiled program and its ASan build print `want`.
fn runs_everywhere(name: &str, src: &str, want: &str) {
    let check = karac(name, src, "check");
    assert!(
        check.status.success(),
        "{name}: check: {}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
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

/// `karac check` refuses `src` with `code`, and its message holds `needle`.
fn refused_with(name: &str, src: &str, code: &str, needle: &str) {
    let out = karac(name, src, "check");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{name}: accepted");
    assert!(text.contains(&format!("error[{code}]")), "{name}: {text}");
    assert!(text.contains(needle), "{name}: {text}");
}

/// The row's two programs: a local key used again after the `entry`, and an
/// element key counted in a loop; and a `SortedMap`.
#[test]
fn entry_lends_a_local_key_and_an_element_key_to_a_map_of_references() {
    runs_everywhere(
        "lent",
        "fn main() {\n    let a = \"a\".to_string();\n    let mut m: Map[ref String, i64] = Map.new();\n    *m.entry(a).or_insert(0) += 1;\n    *m.entry(a).or_insert(0) += 1;\n    println(f\"{m.len()} {m[a]} {a}\");\n    let words: Vec[String] = [\"a\".to_string(), \"b\".to_string(), \"a\".to_string()];\n    let mut n: Map[ref String, i64] = Map.new();\n    for i in 0..3 {\n        *n.entry(words[i]).or_insert(0) += 1;\n    }\n    println(f\"{n.len()} {n[words[0]]}\");\n    let mut s: SortedMap[ref String, i64] = SortedMap.new();\n    s.entry(a).or_insert(5);\n    println(f\"{s.len()} {a}\");\n}\n",
        "1 2 a\n2 2\n1 a\n",
    );
}

/// A temporary key would be dropped at the `;` while the map keeps a
/// reference to it: refused where it is written, with the view to store.
#[test]
fn entry_refuses_a_temporary_key_into_a_map_of_references() {
    refused_with(
        "temp",
        "fn main() {\n    let w = \"a\".to_string();\n    let mut m: Map[ref String, i64] = Map.new();\n    *m.entry(w.clone()).or_insert(0) += 1;\n    println(m.len());\n}\n",
        "E0200",
        "the `Map[ref String, i64]` keeps this argument as one of its entries",
    );
}

/// The lent key is borrowed while the map lives: it cannot be dropped
/// first, written or moved.
#[test]
fn an_entry_key_lent_to_a_map_is_borrowed_while_the_map_lives() {
    for (name, body, needle) in [
        (
            "declared-after",
            "    let mut m: Map[ref String, i64] = Map.new();\n    let a = \"a\".to_string();\n    *m.entry(a).or_insert(0) += 1;\n    println(m.len());\n",
            "cannot drop `a` while `m` still holds a borrow of it",
        ),
        (
            "written",
            "    let mut a = \"a\".to_string();\n    let mut m: Map[ref String, i64] = Map.new();\n    *m.entry(a).or_insert(0) += 1;\n    a.push('x');\n    println(m.len());\n",
            "cannot mutably borrow `a` while `m` still holds a borrow of it",
        ),
        (
            "moved",
            "    let a = \"a\".to_string();\n    let mut m: Map[ref String, i64] = Map.new();\n    *m.entry(a).or_insert(0) += 1;\n    let b = a;\n    println(f\"{m.len()} {b}\");\n",
            "cannot move `a` while `m` still holds a borrow of it",
        ),
    ] {
        refused_with(name, &format!("fn main() {{\n{body}}}\n"), "E0516", needle);
    }
}

/// A map that owns its keys still takes the key by move.
#[test]
fn entry_still_moves_a_key_into_a_map_of_owned_keys() {
    refused_with(
        "owned",
        "fn main() {\n    let a = \"a\".to_string();\n    let mut m: Map[String, i64] = Map.new();\n    *m.entry(a).or_insert(0) += 1;\n    println(a);\n}\n",
        "E0500",
        "value 'a' moved here, used again here",
    );
}
