//! Kata 1114's polling arm waits for its turn with a zero sleep between
//! reads of an `Atomic`, and so do 1115's to 1117's. core-semantics.md
//! §11.1 names a sleep as a suspension point, where a task lets its
//! siblings run, but a sleep whose pause had already passed when first
//! asked completed without parking: the spinner kept its worker, and
//! with as many spinners as workers (one, on the interpreter and under
//! `KARAC_THREADS=1`) the task that sets the flag never ran. A sleep now
//! parks the first time it is asked, even for no time at all.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// How long a program may run before it counts as hung.
const DEADLINE: Duration = Duration::from_secs(30);

/// Eight tasks spin on a flag with a zero sleep between reads; a ninth
/// sets it.
const SPINNERS: &str = "import std.sync.{Atomic, TaskGroup};
import std.time.{Duration, sleep};

fn main() {
    let flag: Atomic[bool] = Atomic.new(false);
    let woke: Atomic[i64] = Atomic.new(0);
    {
        let group = TaskGroup.new();
        for _ in 0..8 {
            group.spawn(|| {
                while not flag.load() {
                    sleep(Duration.ms(0));
                }
                let _ = woke.fetch_add(1);
            });
        }
        group.spawn(|| flag.store(true));
    }
    println(f\"{woke.load()} woke\");
}
";

/// Runs `cmd` to its end, or kills it at [`DEADLINE`] and fails.
fn run(name: &str, mut cmd: Command) -> Output {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if start.elapsed() > DEADLINE {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            panic!(
                "{name} hung: a zero sleep kept the other tasks from running\n{}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn scratch(tag: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-zero-sleep-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SPINNERS).unwrap();
    (dir, file)
}

fn karac(file: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.args(args).arg(file);
    c
}

#[test]
fn kata_1114_a_zero_sleep_lets_the_setter_run_on_the_interpreter() {
    let (dir, file) = scratch("interp");
    let out = run("interpreter", karac(&file, &["__mir-run"]));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "8 woke\n");
}

#[cfg(feature = "llvm")]
#[test]
fn kata_1114_a_zero_sleep_lets_the_setter_run_when_built() {
    let (dir, file) = scratch("built");
    let exe = dir.join("main");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", file.to_str().unwrap(), "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    assert!(
        built.status.success() || err.contains("Linker failed"),
        "does not build: {err}"
    );
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err)
    });
    if linked.is_some() {
        for threads in ["1", "4"] {
            let mut cmd = Command::new(&exe);
            cmd.env("KARAC_THREADS", threads);
            let out = run(&format!("{threads} workers"), cmd);
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                String::from_utf8(out.stdout).unwrap(),
                "8 woke\n",
                "{threads} workers"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
