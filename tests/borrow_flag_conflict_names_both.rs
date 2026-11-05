//! B-2026-11-04-81, design.md §11: a conflicting access to a `shared`
//! field through another handle panics with a message that names the field
//! and the source spans of both accesses: the borrow holding the flag in
//! the message, the conflicting access as the abort's place. The MIR
//! interpreter and the compiled program print the same report, and a
//! borrow held across a suspension keeps its place too.

use std::process::{Command, Stdio};

const ALIAS: &str = "shared struct Bag { mut items: Vec[i64] }
fn add_all(src: Bag, dst: Bag) {
    for x in src.items {
        dst.items.push(x);
    }
}
fn main() {
    let b = Bag { items: Vec.new() };
    b.items.push(1);
    println(\"start\");
    add_all(b, b);
    println(\"unreachable\");
}
";

const CO_ALIAS: &str = "shared struct Bag { mut items: Vec[i64] }
fn add_all(src: Bag, dst: Bag) {
    for x in src.items {
        sleep_ms(1);
        dst.items.push(x);
    }
}
fn main() {
    let b = Bag { items: Vec.new() };
    b.items.push(1);
    println(\"start\");
    add_all(b, b);
    println(\"unreachable\");
}
";

fn run(tag: &str, src: &str, args: &[&str]) -> (Option<i32>, String, String) {
    let dir =
        std::env::temp_dir().join(format!("karac-flag-conflict-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(args)
        .arg("m.kara")
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const ALIAS_REPORT: &str = "runtime error: already borrowed: `Bag.items` is borrowed through another handle at m.kara:3:5 (core semantics §6.2)\n  at m.kara:4:9\n";

#[test]
fn the_interpreter_names_the_field_and_both_accesses() {
    let (code, out, err) = run("interp", ALIAS, &["run", "--interp"]);
    assert_eq!(code, Some(101), "{err}");
    assert_eq!(out, "start\n");
    assert!(err.starts_with(ALIAS_REPORT), "{err}");
}

#[test]
fn the_interpreter_names_the_field_across_a_suspension() {
    let (code, out, err) = run("co-interp", CO_ALIAS, &["run", "--interp"]);
    assert_eq!(code, Some(101), "{err}");
    assert_eq!(out, "start\n");
    assert!(
        err.starts_with("runtime error: already borrowed: `Bag.items` is borrowed through another handle at m.kara:3:5 (core semantics §6.2)\n  at m.kara:5:9\n"),
        "{err}"
    );
}

#[cfg(feature = "llvm")]
mod compiled {
    use super::*;

    /// The built program's exit code, stdout and stderr, or `None` when the
    /// build fails for want of a runtime archive (the skip the rest of the
    /// E2E suite makes; `KARAC_REQUIRE_RUNTIME_ARCHIVE=1` forbids it).
    fn compiled(tag: &str, src: &str) -> Option<(Option<i32>, String, String)> {
        let dir =
            std::env::temp_dir().join(format!("karac-flag-conflict-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("m.kara"), src).unwrap();
        let built = Command::new(env!("CARGO_BIN_EXE_karac"))
            .current_dir(&dir)
            .args(["build", "m.kara", "-o", "m.bin"])
            .stdin(Stdio::null())
            .output()
            .expect("karac");
        if !built.status.success() {
            let err = String::from_utf8_lossy(&built.stderr).into_owned();
            let _ = std::fs::remove_dir_all(&dir);
            assert!(
                err.contains("archive")
                    && std::env::var("KARAC_REQUIRE_RUNTIME_ARCHIVE").as_deref() != Ok("1"),
                "{err}"
            );
            eprintln!("skipped the built leg: {err}");
            return None;
        }
        let out = Command::new(dir.join("m.bin"))
            .current_dir(&dir)
            .stdin(Stdio::null())
            .output()
            .expect("run");
        let _ = std::fs::remove_dir_all(&dir);
        Some((
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ))
    }

    #[test]
    fn the_compiled_program_names_the_field_and_both_accesses() {
        let Some((code, out, err)) = compiled("llvm", ALIAS) else {
            return;
        };
        assert_eq!(code, Some(101), "{err}");
        assert_eq!(out, "start\n");
        assert!(err.starts_with(ALIAS_REPORT), "{err}");
    }

    #[test]
    fn the_compiled_program_names_the_field_across_a_suspension() {
        let Some((code, out, err)) = compiled("co-llvm", CO_ALIAS) else {
            return;
        };
        assert_eq!(code, Some(101), "{err}");
        assert_eq!(out, "start\n");
        assert!(
            err.starts_with("runtime error: already borrowed: `Bag.items` is borrowed through another handle at m.kara:3:5 (core semantics §6.2)\n  at m.kara:5:9\n"),
            "{err}"
        );
    }
}
