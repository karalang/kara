//! B-2026-11-25-43: a local bound directly from a named function or a
//! method path (`let f = sq;`, `let g = i64.abs;`) has an empty environment
//! that is in sight, so it is `CrossTask` with no marker (core semantics
//! §6.3). Each form was refused with E_NOT_CROSS_TASK; an annotation
//! erases the binding, so `let f: Fn(i64) -> i64 = sq;` stays refused.

use std::path::PathBuf;
use std::process::Command;

const FORMS: &[(&str, &str, &str)] = &[
    (
        "fn_par_for",
        "fn sq(x: i64) -> i64 { x * x }\nfn main() {\n    let f = sq;\n    let r = par for x in [1, 2, 3, 4].iter() { f(*x) };\n    println(r);\n}\n",
        "[1, 4, 9, 16]\n",
    ),
    (
        "method_path_par_for",
        "fn main() {\n    let g = i64.abs;\n    let r = par for x in [-1, 2, -3].iter() { g(*x) };\n    println(r);\n}\n",
        "[1, 2, 3]\n",
    ),
    (
        "fn_par_block",
        "fn sq(x: i64) -> i64 { x * x }\nfn main() {\n    let f = sq;\n    let (a, b) = par { f(2), f(3) };\n    println(a + b);\n}\n",
        "13\n",
    ),
    (
        "method_path_par_block",
        "fn main() {\n    let g = i64.abs;\n    let (a, b) = par { g(-2), g(3) };\n    println(a + b);\n}\n",
        "5\n",
    ),
    (
        "fn_spawned",
        "import std.sync.{TaskGroup};\nfn sq7() -> i64 { 49 }\nfn main() {\n    let f = sq7;\n    let mut r = 0;\n    {\n        let tg = TaskGroup.new();\n        let h = tg.spawn(f);\n        r = h.join();\n    }\n    println(r);\n}\n",
        "49\n",
    ),
];

const ERASED: &str = "fn sq(x: i64) -> i64 { x * x }\nfn main() {\n    let f: Fn(i64) -> i64 = sq;\n    let r = par for x in [1, 2, 3, 4].iter() { f(*x) };\n    println(r);\n}\n";

fn program(name: &str, src: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-fn-item-xtask-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    file
}

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn fn_item_and_method_path_locals_cross_tasks_on_the_interpreter() {
    for (name, src, expected) in FORMS {
        let file = program(&format!("interp-{name}"), src);
        let f = file.to_str().unwrap();
        let (ok, out) = karac(&["check", f]);
        assert!(ok, "{name}: {out}");
        let (ok, out) = karac(&["__mir-run", f]);
        assert!(ok, "{name}: {out}");
        assert_eq!(out, *expected, "{name}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn fn_item_and_method_path_locals_cross_tasks_compiled() {
    for (name, src, expected) in FORMS {
        let file = program(&format!("run-{name}"), src);
        let (ok, out) = karac(&["run", file.to_str().unwrap()]);
        assert!(ok, "{name}: {out}");
        assert_eq!(out, *expected, "{name}");
    }
}

#[test]
fn a_fn_item_local_annotated_as_an_erased_fn_is_still_not_cross_task() {
    let file = program("erased", ERASED);
    let (ok, out) = karac(&["check", file.to_str().unwrap()]);
    assert!(!ok, "{out}");
    assert!(out.contains("E_NOT_CROSS_TASK"), "{out}");
}
