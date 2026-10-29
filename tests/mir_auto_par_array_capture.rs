//! An auto-par loop's tasks borrow a fixed array they only read
//! (B-2026-10-29-42). Before, each of the loop's tasks copied it into its
//! environment: kata 1's 40 KB input into all 16, which put its peak RSS
//! about 0.6 MiB over the in-order build. The array is written between
//! the two loops, so each loop's borrow ends at its join. The output is
//! the in-order program's, on the interpreter and built.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"fn total(v: Slice[i64], k: i64) -> i64 {
    let mut s = 0;
    for i in 0..v.len() {
        s += v[i] * k;
    }
    s
}
fn main() {
    let mut data: Array[i64, 1000] = [0; 1000];
    for i in 0..1000 {
        data[i] = i % 7;
    }
    let mut sum = 0;
    for k in 0..100 {
        sum += total(data, k);
    }
    data[0] = 100;
    let mut again = 0;
    for k in 0..100 {
        again += total(data, k);
    }
    println(f"{sum} {again}");
}
"#;

const WANT: &str = "14835150 15330150
";

fn karac(auto: bool) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", if auto { "1" } else { "0" });
    c
}

#[test]
fn mir_auto_par_tasks_borrow_an_array_they_read() {
    let dir = std::env::temp_dir().join(format!("karac-array-capture-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let file = file.to_str().unwrap();

    let seq = karac(false).args(["__mir-run", file]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&seq.stdout), WANT);

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
            .any(|l| l == "auto-par: loops as tasks: main@14, main@19"),
        "{err}"
    );
    // Each loop's task environment holds a borrow of the array.
    let tasks: Vec<&str> = err
        .lines()
        .filter(|l| l.starts_with("fn main.ap"))
        .collect();
    assert_eq!(tasks.len(), 2, "{err}");
    for t in tasks {
        assert!(t.contains(", ref Array[i64, 1000], "), "{t}");
    }

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
