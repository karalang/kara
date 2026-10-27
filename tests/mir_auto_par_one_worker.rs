//! A function with auto-par sites is also built as its in-order twin,
//! `<name>#seq`, and runs the twin when the pool has one worker
//! (`KARAC_THREADS=1`, or a one-core host): there the tasks only cost, and
//! not at the sites alone (kata 28's brute force ran 0.26x its in-order
//! build). B-2026-10-26-17, native half.
//!
//! The method below takes every kind of argument the twin is passed: an
//! owned `self`, a borrow, an owned `Vec` and a destructured tuple.

use std::path::PathBuf;
use std::process::Command;

const SRC: &str = r#"struct Acc {
    name: String,
}

impl Acc {
    fn total(self, v: ref Vec[i64], extra: own Vec[i64], (lo, hi): (i64, i64)) -> i64 {
        let mut s = 0;
        for i in lo..hi {
            let mut j = 0;
            while j < v.len() {
                if (v[j] + i) % 7 == 0 {
                    s = s + 1;
                }
                j = j + 1;
            }
        }
        s + extra.len() + self.name.len()
    }
}

fn main() {
    let v: Vec[i64] = Vec.filled(2000, 3);
    let a = Acc { name: "acc".to_string() };
    println(a.total(v, Vec.filled(5, 1), (0, 300)));
}
"#;

const WANT: &str = "86008\n";

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-auto-par-one-worker-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), SRC).unwrap();
    dir
}

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env_remove("KARAC_AUTO_PAR");
    c
}

/// The interpreter answers the pool question as the native pool does, and
/// both paths print the same.
#[test]
fn mir_auto_par_twin_runs_alike_on_one_worker_and_four_in_the_interpreter() {
    let dir = scratch("interp");
    for threads in ["1", "4"] {
        let out = karac()
            .env("KARAC_THREADS", threads)
            .arg("__mir-run")
            .arg(dir.join("main.kara"))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            WANT,
            "KARAC_THREADS={threads}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The build defines the twin and asks the pool on entry; on one worker and
/// on four the binary prints what the in-order build prints.
#[cfg(feature = "llvm")]
#[test]
fn mir_auto_par_builds_an_in_order_twin_for_one_worker() {
    let build = |dir: &PathBuf, auto_par: Option<&str>| {
        let ll = dir.join("main.ll");
        let mut c = karac();
        if let Some(v) = auto_par {
            c.env("KARAC_AUTO_PAR", v);
        }
        let out = c
            .env("KARAC_MIR_LL", &ll)
            .arg("build")
            .arg(dir.join("main.kara"))
            .arg("-o")
            .arg(dir.join("main.out"))
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        (
            out.status.success(),
            err,
            std::fs::read_to_string(&ll).unwrap_or_default(),
        )
    };
    let dir = scratch("build");
    let (ok, err, ir) = build(&dir, None);
    assert!(
        ir.contains("@\"kara.Acc.total#seq\"") && ir.contains("karac_mir_pooled"),
        "no in-order twin, or no pool check: {err}"
    );
    let seq_dir = scratch("seq");
    let (_, _, seq_ir) = build(&seq_dir, Some("0"));
    let _ = std::fs::remove_dir_all(&seq_dir);
    assert!(
        !seq_ir.contains("#seq") && !seq_ir.contains("karac_mir_pooled"),
        "an in-order build needs no twin"
    );
    if !ok {
        assert!(
            err.contains("Linker failed")
                && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none(),
            "does not build: {err}"
        );
        eprintln!("skipping the runs: no runtime archive");
        return;
    }
    for threads in ["1", "4"] {
        let out = Command::new(dir.join("main.out"))
            .env("KARAC_THREADS", threads)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            WANT,
            "KARAC_THREADS={threads}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
