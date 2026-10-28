//! A binary whose only tasks are auto-par's links the executor but not its
//! I/O reactor: the reactor is reached only through the descriptor waits
//! that make it needed, so `epoll_wait` and `epoll_ctl` stay out of a
//! compute program (B-2026-10-28-8, two_sum's brute force was +24% over
//! its in-order build). The program must still have its pool, or the
//! check would pass on a build with no tasks at all, and must print the
//! in-order answer on one worker and on four.
#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

const SRC: &str = r#"fn two_sum(nums: Slice[i64], target: i64) -> Array[i64, 2] {
    let n = nums.len();
    for i in 0..n {
        for j in (i + 1)..n {
            if nums[i] + nums[j] == target {
                return [i, j];
            }
        }
    }
    [-1, -1]
}

fn main() {
    let mut data: Array[i64, 600] = [0; 600];
    for i in 0..600 {
        data[i] = (i * 7) % 1000;
    }
    let mut sum = 0;
    for k in 0..4 {
        let r = two_sum(data, 5 + k * 300);
        sum = sum + r[0] * 1000 + r[1];
    }
    println(sum);
}
"#;

/// The first pairs in the brute force's order, (143, 572), (16, 599),
/// (0, 515) and (0, 415), as a Python mirror of the loops finds them.
const WANT: &str = "161101\n";

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-task-pool-lean-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), SRC).unwrap();
    dir
}

/// The executable's dynamic imports, or `None` without `nm`.
fn imports(exe: &Path) -> Option<String> {
    let out = Command::new("nm").arg("-D").arg(exe).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn mir_auto_par_binary_links_no_reactor() {
    let dir = scratch();
    let exe = dir.join("main.out");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env_remove("KARAC_AUTO_PAR")
        .env_remove("KARAC_RUNTIME")
        .env_remove("KARAC_FORCE_FULL_RUNTIME")
        .arg("build")
        .arg(dir.join("main.kara"))
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success()
        && err.contains("not found")
        && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none()
    {
        eprintln!("skipping: no runtime archive: {err}");
        return;
    }
    assert!(out.status.success(), "{err}");
    for threads in ["1", "4"] {
        let run = Command::new(&exe)
            .env("KARAC_THREADS", threads)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            WANT,
            "KARAC_THREADS={threads}: {}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
    let Some(syms) = imports(&exe) else {
        eprintln!("skipping the symbol check: no nm");
        return;
    };
    assert!(
        syms.contains("pthread_create"),
        "no pool in the binary, so no auto-par site: the check is void"
    );
    for reactor in ["epoll_wait", "epoll_ctl"] {
        assert!(
            !syms.contains(reactor),
            "{reactor} is linked into a program with no descriptor wait"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
