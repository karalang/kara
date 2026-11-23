//! An empty `[]` inside a tuple or a `Some` / `Ok` / `Err` payload names no
//! element type of its own. As an element of a list literal it takes a
//! sibling's, or the slot's through `.to_vec()`, as a bare `[]` element
//! already did; with neither, it is reported where it is written. It typed
//! as `Vec[Error]` with nothing reported, and the program stopped at the
//! internal "a type that failed to check is not lowered yet"
//! (B-2026-11-21-49).

mod common;

use std::process::Command;

fn karac(name: &str, src: &str, cmd: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-empty-list-in-tuple-{name}-{cmd}-{}",
        std::process::id()
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
fn empty_part_runs_everywhere(name: &str, src: &str, want: &str) {
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

/// `karac check`'s output for `src`, which it refuses.
fn empty_part_refused(name: &str, src: &str) -> String {
    let out = karac(name, src, "check");
    assert!(!out.status.success(), "{name}: accepted");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The row's line, from kata 2392's harness: the slot of the `let` reaches
/// the literal through `.to_vec()`.
#[test]
fn an_empty_list_in_a_tuple_takes_the_slot_through_to_vec() {
    empty_part_runs_everywhere(
        "slot",
        "fn main() {\n    let xs: Vec[(i64, Vec[i64])] = [(1, [1, 2]), (2, [])].to_vec();\n    for (k, v) in xs.iter() {\n        println(f\"{k} {v.len()}\");\n    }\n    let one: Vec[(i32, Vec[i32])] = [(2, [])].to_vec();\n    let n: i32 = one[0].0;\n    println(f\"{n} {one[0].1.len()}\");\n    let os: Vec[Option[Vec[i64]]] = [Some([1]), Some([]), None].to_vec();\n    println(os.len());\n}\n",
        "1 2\n2 0\n2 0\n3\n",
    );
}

/// With no slot, a sibling that names the type gives it, whichever comes
/// first.
#[test]
fn an_empty_list_in_a_tuple_takes_a_siblings_type() {
    empty_part_runs_everywhere(
        "sibling",
        "fn main() {\n    let a = [(1, [1, 2]), (2, [])];\n    let b = [(2, []), (1, [1, 2])].to_vec();\n    let c = [Some([]), Some([7]), None];\n    println(f\"{a[1].1.len()} {b[0].1.len()} {b[1].1[1]} {c.len()}\");\n}\n",
        "0 0 2 3\n",
    );
}

/// With neither, the `[]` is reported where it is written, with the
/// `Vec.new()` a later use can type; `karac fix` applies it and the program
/// then checks and runs.
#[test]
fn an_empty_list_no_slot_or_sibling_types_is_reported_and_mended() {
    let src = "fn f(v: Vec[i64]) -> i64 {\n    v.len()\n}\nfn main() {\n    let t = (2, []);\n    println(f(t.1));\n}\n";
    let shown = empty_part_refused("alone", src);
    assert!(
        shown.contains("error[E_EMPTY_PREFIX_LITERAL_NEEDS_ANNOTATION]")
            && shown.contains("main.kara:5:17:"),
        "{shown}"
    );
    assert!(!shown.contains("not lowered yet"), "{shown}");
    let shown = empty_part_refused(
        "lone-element",
        "fn main() {\n    let a = [(2, [])];\n    println(a.len());\n}\n",
    );
    assert!(
        shown.contains("error[E_EMPTY_PREFIX_LITERAL_NEEDS_ANNOTATION]")
            && shown.contains("main.kara:2:18:"),
        "{shown}"
    );

    let dir = std::env::temp_dir().join(format!(
        "karac-empty-list-in-tuple-fix-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let fix = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("fix")
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let mended = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(mended, src.replace("(2, [])", "(2, Vec.new())"));
    empty_part_runs_everywhere("mended", &mended, "0\n");
}
