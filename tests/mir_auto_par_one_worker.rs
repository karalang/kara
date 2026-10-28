//! A build that runs loops or statements as tasks also carries the program
//! as `KARAC_AUTO_PAR=0` builds it, and runs that one when the pool has one
//! worker (`KARAC_THREADS=1`, or a one-core host): there the tasks only
//! cost, and not at the sites alone (kata 28's brute force ran 0.26x its
//! in-order build). B-2026-10-26-17, native half. A native build links the
//! in-order program as a module of its own, which is the in-order build's
//! module, so on one worker it runs that build's instructions
//! (B-2026-10-28-1).
//!
//! The method below takes an owned `self`, a borrow, an owned `Vec` and a
//! destructured tuple.

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

/// The build carries the in-order program's module, which is the in-order
/// build's own, and asks the pool once; on one worker and on four the
/// binary prints what the in-order build prints.
#[cfg(feature = "llvm")]
#[test]
fn mir_auto_par_build_carries_the_in_order_program_for_one_worker() {
    // Both builds read one source path, which their panic messages name.
    let build = |dir: &PathBuf, auto_par: Option<&str>, name: &str| {
        let ll = dir.join(format!("{name}.ll"));
        let mut c = karac();
        if let Some(v) = auto_par {
            c.env("KARAC_AUTO_PAR", v);
        }
        let out = c
            .env("KARAC_MIR_LL", &ll)
            .arg("build")
            .arg(dir.join("main.kara"))
            .arg("-o")
            .arg(dir.join(format!("{name}.out")))
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        let read = |p: PathBuf| std::fs::read_to_string(p).unwrap_or_default();
        (
            out.status.success(),
            err,
            read(ll.clone()),
            read(dir.join(format!("{name}.ll.seq.ll"))),
        )
    };
    let dir = scratch("build");
    let (ok, err, ir, in_order) = build(&dir, None, "main");
    assert!(
        ir.contains("karac_mir_task")
            && ir.contains("karac_mir_pooled")
            && ir.contains("@karac.main.seq"),
        "no tasks, or no pool check: {err}"
    );
    // No resume reads the place of the latest library call, which would
    // keep every call's store of it alive (B-2026-10-28-1).
    let mut func = "";
    for l in ir.lines() {
        if l.starts_with("define ") {
            func = l;
        }
        assert!(
            !(func.contains("#resume") && l.contains("load") && l.contains("@karac.site")),
            "a resume reads the call place: {func}"
        );
    }
    let (_, _, seq_ir, none) = build(&dir, Some("0"), "seq");
    assert!(
        none.is_empty() && !seq_ir.contains("karac_mir_pooled"),
        "an in-order build carries no second program"
    );
    // The carried module is the in-order build's, but for its names and
    // the C `main`, which the parallel module has.
    let names = |ir: &str| {
        ir.replace("#seq", "")
            .replace("karac.main.seq", "karac.main")
            .replace('"', "")
    };
    let c_main = |ir: &str| {
        let mut out = String::new();
        let mut skip = false;
        for l in ir.lines() {
            skip |= l.starts_with("define i32 @main(");
            if !skip {
                out.push_str(l);
                out.push('\n');
            }
            skip &= l != "}";
        }
        out
    };
    let lines = |ir: String| {
        ir.lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        lines(names(&in_order)),
        lines(names(&c_main(&seq_ir))),
        "the carried program is not the in-order build"
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

/// With debug info the parallel `karac.main.par` keeps its subprogram
/// while the `karac.main` that picks a program has none; the build still
/// verifies and runs alike on one worker and four.
#[cfg(feature = "llvm")]
#[test]
fn mir_auto_par_build_with_debug_info_picks_a_program() {
    let dir = scratch("debug");
    let ll = dir.join("main.ll");
    let out = karac()
        .env("KARAC_DEBUG_INFO", "1")
        .env("KARAC_MIR_LL", &ll)
        .arg("build")
        .arg(dir.join("main.kara"))
        .arg("-o")
        .arg(dir.join("main.out"))
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    let ir = std::fs::read_to_string(&ll).unwrap_or_default();
    assert!(
        ir.contains("DISubprogram") && ir.contains("@karac.main.seq"),
        "no debug info, or no in-order program: {err}"
    );
    if !out.status.success() {
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
