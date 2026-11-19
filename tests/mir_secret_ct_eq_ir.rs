//! B-2026-11-19-115: a built `Secret.ct_eq` compares in constant time (secret.md
//! § Access). It used to call the payload type's `eq` glue, which returns
//! at the first differing byte; now each comparison is one call of the
//! runtime's `karac_secret_ct_eq`, which ORs every byte's XOR over the
//! whole length (its own test is in runtime/src/lib.rs), and no `eq` glue
//! is emitted for it.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"import std.secret.{Secret};
fn main() {
    let a = Secret.new("hunter2".to_string());
    let b = Secret.new("hunter2".to_string());
    let c = Secret.new("hunter3".to_string());
    println(a.ct_eq(b));
    println(a.ct_eq(c));
    let x: Secret[Vec[u8]] = Secret.new([1u8, 2, 3].to_vec());
    let y: Secret[Vec[u8]] = Secret.new([1u8, 2, 4].to_vec());
    println(x.ct_eq(y));
}
"#;

#[test]
fn mir_secret_ct_eq_calls_the_constant_time_runtime_helper() {
    let dir = std::env::temp_dir().join(format!("karac-ct-eq-ir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let exe = dir.join("main");
    let ll = dir.join("main.ll");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_MIR_LL", &ll)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    let ir = std::fs::read_to_string(&ll).unwrap_or_else(|_| panic!("no IR\n{why}"));
    let main: Vec<&str> = ir
        .lines()
        .skip_while(|l| !(l.starts_with("define") && l.contains("@kara.main()")))
        .take_while(|l| *l != "}")
        .collect();
    assert_eq!(
        main.iter()
            .filter(|l| l.contains("call i64 @karac_secret_ct_eq("))
            .count(),
        3,
        "{ir}"
    );
    assert!(
        !ir.lines()
            .any(|l| l.starts_with("define") && (l.contains("@eq.") || l.contains("@\"eq."))),
        "eq glue emitted:\n{ir}"
    );
    let ran = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why.clone())
    });
    if ran.is_some() {
        let out = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "true\nfalse\nfalse\n");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
