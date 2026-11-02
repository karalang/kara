//! A task cancelled while it waits at a `par`'s join leaves by its cancel
//! exit, which drops the `par`'s group and the handles not joined yet: a
//! branch that already finished left its result in its handle, and both
//! leaked (B-2026-11-02-1). The pins run under AddressSanitizer at one and
//! four workers: a `par` block, a `par for`, and a `par` in a function
//! whose own drops run innermost first on that exit.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const BLOCK: &str = r#"import std.time.{sleep, Duration};
fn main() {
    let g = TaskGroup.new();
    let h = g.spawn(|| { let (a, b) = par { { sleep(Duration.ms(2000)); 1 }, 2 }; a + b });
    sleep(Duration.ms(20));
    g.cancel();
    println(h.try_join().is_err());
}
"#;

const FOR: &str = r#"import std.time.{sleep, Duration};
fn main() {
    let g = TaskGroup.new();
    let h = g.spawn(|| { let v = par for x in vec![1, 2] { if x == 1 { sleep(Duration.ms(2000)); } x }; v.len() });
    sleep(Duration.ms(20));
    g.cancel();
    println(h.try_join().is_err());
}
"#;

const DROPS: &str = r#"import std.time.{sleep, Duration};
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}"); } }
fn mk(i: i64) -> R { R { id: i } }
fn work() -> i64 {
    let r = mk(1);
    let (a, b) = par { { let q = mk(2); sleep(Duration.ms(2000)); q.id }, 3 };
    a + b + r.id
}
fn main() {
    let g = TaskGroup.new();
    let h = g.spawn(|| work());
    sleep(Duration.ms(20));
    g.cancel();
    println(h.try_join().is_err());
}
"#;

/// Whether `cc -fsanitize=address` links and runs here.
fn asan_available(dir: &std::path::Path) -> bool {
    if std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_some() {
        return false;
    }
    let c = dir.join("probe.c");
    let exe = dir.join("probe");
    std::fs::write(
        &c,
        "int main(void){return 0;}
",
    )
    .expect("write probe");
    Command::new("cc")
        .arg("-fsanitize=address")
        .arg(&c)
        .arg("-o")
        .arg(&exe)
        .status()
        .is_ok_and(|s| s.success())
        && Command::new(&exe).status().is_ok_and(|s| s.success())
}

fn runs_clean(src: &str, tag: &str, want: &str) {
    let d = std::env::temp_dir().join(format!("karac-cancel-par-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    if !asan_available(&d) {
        eprintln!("skipping: no ASan-capable cc");
        return;
    }
    std::fs::write(d.join("main.kara"), src).unwrap();
    for threads in ["1", "4"] {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .current_dir(&d)
            .env("ASAN_OPTIONS", "exitcode=86")
            .env("KARAC_THREADS", threads)
            .args(["__mir-llvm-asan", "main.kara"])
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        if out.status.code() == Some(3) && err.contains("Linker failed") {
            let _ = common::link_or_skip(Err(err.into_owned()));
            return;
        }
        assert_eq!(
            out.status.code(),
            Some(0),
            "{threads} workers, stderr:
{err}"
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{threads} workers"
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_cancelled_par_block_frees_its_group_and_finished_handle() {
    runs_clean(
        BLOCK, "block", "true
",
    );
}

#[test]
fn a_cancelled_par_for_frees_its_group_and_handles() {
    runs_clean(
        FOR, "for", "true
",
    );
}

#[test]
fn a_cancelled_par_in_a_function_drops_innermost_first() {
    runs_clean(
        DROPS,
        "drops",
        "d2
d1
true
",
    );
}
