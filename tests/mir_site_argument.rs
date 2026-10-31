//! A library body that can abort takes the place of the program's call
//! into it as two trailing arguments, forwarded from library body to
//! library body, instead of a global the program writes before each call
//! (B-2026-10-31-2): the program stores nothing on the path that runs, and
//! no global is left for the JIT to share between the pool's workers
//! (B-2026-10-28-44). The pins: the IR has no site global and calls the
//! library's located function with the line as a constant; an abort in a
//! library body still reports the program's line, interpreted, built and
//! on the JIT.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

/// `unwrap` in a loop, then an abort inside `swap_remove`'s own body (its
/// `swap` of index -1 on an empty vector), which reports `take`'s line.
const SRC: &str = r#"fn take(v: mut ref Vec[i64]) -> i64 {
    v.swap_remove(0)
}
fn main() {
    let mut s = 0;
    for k in 0..1000 {
        let o: Option[i64] = Some(k);
        s = s + o.unwrap();
    }
    println(s);
    let mut v: Vec[i64] = [5].to_vec();
    println(take(mut v));
    println(take(mut v));
}
"#;

const OUT: &str = "499500\n5\n";
const ERR: &str =
    "runtime error: index out of bounds\n  at main.kara:2:5\nError return trace:\n  main.kara:2:5\n";

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-site-arg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn check(how: &str, o: &Output) {
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(String::from_utf8_lossy(&o.stdout), OUT, "{how}: {err}");
    assert_eq!(err, ERR, "{how}");
    assert_eq!(o.status.code(), Some(101), "{how}");
}

#[test]
fn mir_library_abort_reports_the_place_passed_in() {
    let d = dir();
    std::fs::write(d.join("main.kara"), SRC).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let interp = Command::new(karac)
        .current_dir(&d)
        .args(["__mir-run", "main.kara"])
        .output()
        .unwrap();
    check("interp", &interp);
    let built = Command::new(karac)
        .current_dir(&d)
        .env("KARAC_AUTO_PAR", "0")
        .env("KARAC_MIR_LL", d.join("main.ll"))
        .args(["build", "main.kara", "-o", "main"])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    })
    .is_none()
    {
        return;
    }
    let ir = std::fs::read_to_string(d.join("main.ll")).unwrap();
    assert!(!ir.contains("@karac.site"), "a site global is left");
    // `unwrap` and `swap_remove` are called with the program's line as a
    // constant.
    for f in ["unwrap", "swap_remove"] {
        let call = format!(".{f}.at\"(");
        assert!(
            ir.lines()
                .any(|l| l.contains("call ") && l.contains(&call) && l.contains(", ptr @str")),
            "no call of {f}'s located function with a constant place"
        );
    }
    check("built", &Command::new(d.join("main")).output().unwrap());
    let run = Command::new(karac)
        .current_dir(&d)
        .env("KARAC_AUTO_PAR", "0")
        .args(["run", "main.kara"])
        .output()
        .unwrap();
    check("karac run", &run);
    let _ = std::fs::remove_dir_all(&d);
}
