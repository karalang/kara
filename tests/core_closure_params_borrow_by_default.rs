//! D5 for closures is on by default (B-2026-10-28-24): a closure's annotated
//! bare parameter borrows as a function's does (design.md § Parameter modes),
//! and only `|x: own T|` owns. `KARAC_D5_CLOSURES=0` reads it by value, as
//! before the migration. A closure that moves its parameter out, or a match
//! binding out of it, is refused at the closure, and `karac fix` writes the
//! parameter's `own`.

use std::path::PathBuf;
use std::process::{Command, Output};

fn fixture(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-d5cl-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf, off: bool) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    if off {
        cmd.env("KARAC_D5_CLOSURES", "0");
    } else {
        cmd.env_remove("KARAC_D5_CLOSURES");
    }
    cmd.args(args).arg(path).output().unwrap()
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

#[test]
fn an_annotated_closure_parameter_borrows_unless_switched_off() {
    let path = fixture(
        "default",
        "fn main() {\n    let pair = |e: String| (1, e);\n    println(pair(\"x\".to_string()).1);\n}\n",
    );
    let on = karac(&["check"], &path, false);
    assert!(
        !on.status.success() && text(&on).contains("cannot give away closure parameter `e`"),
        "the default must borrow: {}",
        text(&on)
    );
    let off = karac(&["check"], &path, true);
    assert!(
        off.status.success(),
        "KARAC_D5_CLOSURES=0 reads it by value: {}",
        text(&off)
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_match_moving_a_binding_out_of_a_closure_parameter_is_fixed_with_own() {
    let src = "enum Ho { Full(String), Empty }
fn main() {
    let f = |q: Ho| { match q { Ho.Full(s) => s, Ho.Empty => \"e\".to_string() } };
    let a = f(Ho.Full(\"x\".to_string()));
    let b = f(Ho.Empty);
    println(f\"{a} {b}\");
}
";
    let path = fixture("match", src);
    let out = karac(&["check"], &path, false);
    assert!(
        !out.status.success() && text(&out).contains("Take `q` by value with `|q: own Ho|`"),
        "check must name the parameter's `own`: {}",
        text(&out)
    );
    let fixed = karac(&["fix"], &path, false);
    let now = std::fs::read_to_string(&path).unwrap();
    assert!(
        now.contains("|q: own Ho|") && !now.contains(".clone()"),
        "fix must write `own`, not a clone: {now}\n{}",
        text(&fixed)
    );
    let out = karac(&["check"], &path, false);
    assert!(out.status.success(), "fixed: {}", text(&out));
    assert_eq!(
        karac::mir::lower::run_source(&now)
            .expect("MIR interpreter")
            .output,
        "x e\n"
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
