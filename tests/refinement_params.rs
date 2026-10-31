//! A parameter of a refinement type follows its base type's parameter rules
//! exactly: a bare one borrows (a copy for a copy base), `own` takes
//! ownership, and the refinement only adds its predicate at construction
//! (B-2026-10-31-14). A generic refinement (`NonEmpty[T]`) is instantiated
//! at each use, its `try_from` and its predicate included.

use std::path::PathBuf;
use std::process::{Command, Output};

fn program(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-refinement-params-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .current_dir(path.parent().unwrap())
        .env("KARAC_AUTO_PAR", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn all(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `karac check` passes and the MIR interpreter prints `expected`; under
/// `llvm`, `karac run` prints it too.
fn checks_and_prints(tag: &str, src: &str, expected: &str) {
    let path = program(tag, src);
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{}", all(&out));
    let mut cmds = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        cmds.push("run");
    }
    for cmd in cmds {
        let out = karac(&[cmd], &path);
        assert!(out.status.success(), "{cmd}: {}", all(&out));
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{cmd}");
    }
}

const NON_EMPTY: &str = "type NonEmpty[T] = Vec[T] where self.len() > 0;\n\n";

fn total_of(param: &str) -> String {
    format!(
        "{NON_EMPTY}fn total(rows: {param}) -> i64 {{\n    let mut t = 0;\n    \
         for r in rows {{\n        t = t + r;\n    }}\n    t\n}}\n\n\
         fn main() {{\n    let v: Vec[i64] = [1, 2, 3].to_vec();\n    \
         match NonEmpty.try_from(v) {{\n        Ok(rows) => println(f\"{{total(rows)}}\"),\n        \
         Err(_) => println(\"empty\"),\n    }}\n}}\n"
    )
}

#[test]
fn a_bare_generic_refinement_parameter_borrows_like_its_base() {
    checks_and_prints("generic-bare", &total_of("NonEmpty[i64]"), "6\n");
}

#[test]
fn an_own_generic_refinement_parameter_takes_its_argument() {
    checks_and_prints("generic-own", &total_of("own NonEmpty[i64]"), "6\n");
}

#[test]
fn a_refinement_of_a_copy_base_is_a_copy_parameter() {
    checks_and_prints(
        "copy-base",
        "type Pos = i64 where self > 0;\n\nfn twice(p: Pos) -> i64 {\n    p * 2\n}\n\n\
         fn main() {\n    match Pos.try_from(3) {\n        Ok(p) => println(twice(p)),\n        \
         Err(_) => println(\"no\"),\n    }\n}\n",
        "6\n",
    );
}

#[test]
fn a_bare_refinement_parameter_calls_its_bases_methods() {
    checks_and_prints(
        "base-method",
        "type Big = Vec[i64] where self.len() > 0;\n\nfn sum(v: Big) -> i64 {\n    v.len()\n}\n\n\
         fn main() {\n    match Big.try_from([1, 2].to_vec()) {\n        \
         Ok(b) => println(f\"{sum(b)}\"),\n        Err(_) => println(\"no\"),\n    }\n}\n",
        "2\n",
    );
}

#[test]
fn a_generic_refinement_checks_its_predicate_at_each_instance() {
    checks_and_prints(
        "generic-strings",
        &format!(
            "{NON_EMPTY}fn first(rows: NonEmpty[String]) -> String {{\n    rows[0].clone()\n}}\n\n\
             fn count(rows: own NonEmpty[String]) -> i64 {{\n    rows.len()\n}}\n\n\
             fn main() {{\n    let none: Vec[String] = Vec.new();\n    \
             match NonEmpty.try_from(none) {{\n        Ok(_) => println(\"some\"),\n        \
             Err(e) => println(e),\n    }}\n    \
             let v: Vec[String] = [\"a\".to_string(), \"b\".to_string()].to_vec();\n    \
             match NonEmpty.try_from(v) {{\n        Ok(rows) => {{\n            \
             println(first(rows));\n            println(count(rows));\n        }}\n        \
             Err(e) => println(e),\n    }}\n    let w: Vec[i64] = [4].to_vec();\n    \
             let n = w as NonEmpty[i64];\n    println(n.len());\n}}\n"
        ),
        "value does not satisfy refinement `NonEmpty`\na\n2\n1\n",
    );
}
