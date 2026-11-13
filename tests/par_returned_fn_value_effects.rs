//! A `par` or `par for` calling an un-annotated local that holds a returned
//! function value was refused as "`unknown (a call through a value)`", even
//! when the function's return type declares `with ()`: only an annotated
//! `let` recorded its declared clause (B-2026-11-13-19, the remainder of
//! B-2026-10-27-8). `let f = mk();` now takes the clause `mk`'s written
//! return type declares. One whose return type declares none still conflicts.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], tag: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-par-returned-fn-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

#[test]
fn par_calls_an_unannotated_returned_fn_value_with_a_declared_clause() {
    let src = "fn mk() -> Fn() -> i64 + CrossTask with () { || 1 }\n\
               fn main() {\n    let f = mk();\n    let (a, b) = par { f(), f() };\n    \
               println(f\"{a + b}\");\n    let xs = [1, 2, 3];\n    \
               let r = par for i in 0..3 { f() + xs[i] };\n    println(r);\n}\n";
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, "declared", src);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "2\n[2, 3, 4]\n",
            "{lane:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn par_calling_a_returned_fn_value_with_no_clause_still_conflicts() {
    let src = "fn mk() -> Fn() -> i64 + CrossTask { || 1 }\n\
               fn main() { let f = mk(); let (a, b) = par { f(), f() }; println(f\"{a + b}\"); }\n";
    let out = karac(&["check"], "undeclared", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains("unknown (a call through a value)"),
        "{stderr}"
    );
}
