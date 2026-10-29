//! An auto-par statement group runs as at most four tasks
//! (B-2026-10-28-10): each a run of consecutive statements, in order, its
//! heavy statements shared out between the tasks and its light ones riding
//! along. Before, every statement was a task of its own, and a long run
//! (`mini_inflect`'s `new_tab`, 25 statements) made the function's resume
//! the build's long pole. The output is the in-order program's, on the
//! interpreter and built, and values a task drops drop once.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"fn work(n: i64, k: i64) -> i64 {
    let mut s = 0;
    for i in 0..n {
        for j in 0..k {
            s += i * j + k;
        }
    }
    s
}
fn table(n: i64) -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    for i in 0..n {
        for j in 0..2 {
            v.push(f"{i}.{j}");
        }
    }
    v
}
fn main() {
    let a = work(30, 1);
    table(5);
    let b = work(30, 2);
    let names = table(3);
    let c = work(30, 3);
    let label = "sums";
    let d = work(30, 4);
    table(4);
    let e = work(30, 5);
    let f = work(30, 6);
    println(f"{label}: {a} {b} {c} {d} {e} {f} {names.len()} {names[5]}");
}
"#;

const WANT: &str = "sums: 30 555 1575 3090 5100 7605 6 2.1\n";

fn karac(auto: bool) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", if auto { "1" } else { "0" });
    c
}

#[test]
fn mir_auto_par_statement_group_runs_as_four_tasks() {
    let dir = std::env::temp_dir().join(format!("karac-stmt-chunks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let file = file.to_str().unwrap();

    let seq = karac(false).args(["__mir-run", file]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&seq.stdout), WANT);

    // The ten statements are one group of six heavy ones, as four tasks.
    let par = karac(true)
        .env("KARAC_AUTO_PAR_DEBUG", "1")
        .env("KARAC_MIR_DUMP", "built")
        .args(["__mir-run", file])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&par.stdout), WANT);
    let err = String::from_utf8_lossy(&par.stderr);
    assert!(
        err.lines()
            .any(|l| l == "auto-par: statements as tasks: main@20"),
        "{err}"
    );
    let spawns = format!("\n{err}")
        .split("\nfn ")
        .filter(|b| b.starts_with("main("))
        .map(|b| b.matches("TaskGroup.spawn").count())
        .max();
    assert_eq!(spawns, Some(4), "{err}");

    let exe = dir.join("main");
    let built = karac(true)
        .args(["build", file, "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why.clone())
    })
    .is_some()
    {
        let out = Command::new(&exe).output().unwrap();
        assert!(out.status.success(), "{why}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), WANT);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
