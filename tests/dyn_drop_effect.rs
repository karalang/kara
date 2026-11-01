//! A `dyn` value's drop runs the drop glue of the type coerced into it, so
//! it has that glue's effects (core-semantics §12 default 3). The effect
//! pass saw no parts in a `dyn` and charged its drop nothing, so a `par`
//! branch dropping a `dyn T` whose concrete `Drop` prints passed beside a
//! printing branch (B-2026-11-01-39). The drop is now charged the glue of
//! every type the program coerces to a `dyn` of that trait: a returned
//! `dyn`, a locally coerced one, a `Vec[dyn T]`, and two such drops conflict
//! with `writes(Stdout)`, while a `dyn` whose types drop silently does not.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-dyn-drop-effect-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

const PRELUDE: &str = "struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f\"d{self.id}\"); } }
fn mk(i: i64) -> R { R { id: i } }
trait T { fn go(self) -> i64; }
impl T for R { fn go(self) -> i64 { self.id } }
fn mkr() -> dyn T { mk(1) }
";

#[test]
fn dropping_a_dyn_whose_type_prints_conflicts_with_a_printing_branch() {
    for (tag, par) in [
        (
            "returned",
            "par { { let d = mkr(); 0 }, { println(\"x\"); 2 } }",
        ),
        (
            "coerced",
            "par { { let d: dyn T = mk(1); 0 }, { println(\"x\"); 2 } }",
        ),
        (
            "vec",
            "par { { let v: Vec[dyn T] = vec![mk(1)]; 0 }, { println(\"x\"); 2 } }",
        ),
        (
            "two-drops",
            "par { { let d = mkr(); 0 }, { let e = mkr(); 0 } }",
        ),
    ] {
        let src = format!(
            "{PRELUDE}fn main() {{\n    let (a, b) = {par};\n    println(f\"{{a}} {{b}}\");\n}}\n"
        );
        let path = temp(tag, &src);
        let check = karac(&["check", path.to_str().unwrap()]);
        let _ = std::fs::remove_file(&path);
        let err = String::from_utf8_lossy(&check.stderr);
        assert!(
            !check.status.success()
                && err.contains(
                    "branches 1 and 2 of the `par` conflict: `writes(Stdout)` and `writes(Stdout)`"
                ),
            "{tag}: {err}"
        );
    }
}

#[test]
fn dropping_a_dyn_whose_types_drop_silently_is_no_conflict() {
    let src = "trait T { fn go(self) -> i64; }
struct Q { id: i64 }
impl T for Q { fn go(self) -> i64 { self.id } }
fn mkq() -> dyn T { Q { id: 2 } }
fn main() {
    let (a, b) = par { { let d = mkq(); 0 }, { println(\"x\"); 2 } };
    let (c, e) = par { { let w: Vec[dyn T] = vec![mkq()]; 0 }, { println(\"y\"); 1 } };
    println(f\"{a} {b} {c} {e} {mkq().go()}\");
}
";
    let path = temp("silent", src);
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        (
            run.status.code(),
            String::from_utf8_lossy(&run.stdout).as_ref()
        ),
        (Some(0), "x\ny\n0 2 0 1 2\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
