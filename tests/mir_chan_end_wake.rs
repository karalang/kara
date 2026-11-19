//! A last `Sender` dropped while its receiver is being polled on another
//! worker wakes the receiver (B-2026-11-19-64). The drop counted its change
//! before the sender count fell, so a receiver polled in between saw the
//! change as already seen, found a sender left and parked, and the fall woke
//! nobody: the program stopped with "deadlock: every task waits on a
//! channel", in every one of 10 runs of the built binary below. Each run
//! here must print the sum.

#![cfg(feature = "llvm")]

use std::path::PathBuf;
use std::process::Command;

const SRC: &str = r#"import std.sync.{TaskGroup};
fn run(k: i64) -> i64 {
    let group = TaskGroup.new();
    let (tx, rx) = channel(1);
    group.spawn(|| { while let Some(x) = rx.recv() { let _ = x; } });
    tx.send(k).unwrap();
    drop(tx);
    k
}
fn main() { let mut t = 0; for k in 0..10000 { t += run(k); } println(t); }
"#;

#[test]
fn a_last_sender_dropped_mid_poll_wakes_its_receiver() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("karac-chan-end-wake-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, SRC).unwrap();
    let exe = path.with_extension("");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build"])
        .arg(&path)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    for threads in ["4", "4", "4", "8", "8"] {
        let out = Command::new(&exe)
            .env("KARAC_THREADS", threads)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "49995000\n",
            "KARAC_THREADS={threads}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "KARAC_THREADS={threads}");
    }
}
