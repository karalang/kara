//! A send that its readiness loop found ready can still find the channel
//! full when it runs: another worker of the pool took the last slot in
//! between. The armed call then does nothing and the loop asks `__ready`
//! again, so the send waits instead of answering `Err` (B-2026-10-31-34).
//! The pin: four producers on a full `channel(64)` under four workers lose
//! no value, run after run (the race lost one in most runs before).
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"fn start(group: ref TaskGroup, tx: own Sender[i64]) -> Vec[TaskHandle[i64]] {
    let mut hs: Vec[TaskHandle[i64]] = Vec.new();
    for p in 0..4 {
        let t = tx.clone();
        hs.push(group.spawn(|| {
            let mut errs = 0;
            for i in 0..2000 {
                match t.send(i) {
                    Ok(_) => {}
                    Err(_) => { errs = errs + 1; }
                }
            }
            errs
        }));
    }
    hs
}

fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(64);
    let group = TaskGroup.new();
    let hs = start(group, tx);
    let mut sum = 0;
    let mut n = 0;
    for v in rx.into_iter() {
        sum = sum + v;
        n = n + 1;
    }
    let mut errs = 0;
    for h in hs.into_iter() {
        errs = errs + h.join();
    }
    println(f"{n} {sum} errs={errs}");
}
"#;

const OUT: &str = "8000 7996000 errs=0\n";

#[test]
fn mir_full_channel_send_waits_under_the_pool() {
    let d = std::env::temp_dir().join(format!("karac-chan-retry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("main.kara"), SRC).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let interp = Command::new(karac)
        .current_dir(&d)
        .args(["__mir-run", "main.kara"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&interp.stdout), OUT, "interp");
    let built = Command::new(karac)
        .current_dir(&d)
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
    for run in 0..8 {
        let o = Command::new(d.join("main"))
            .env("KARAC_THREADS", "4")
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&o.stdout),
            OUT,
            "run {run}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}
