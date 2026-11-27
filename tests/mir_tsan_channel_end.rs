//! A channel's ends dropped on different workers run clean under
//! ThreadSanitizer (B-2026-11-20-31). `karac_mir_chan_end` read the
//! channel's drop glue after releasing its lock, when the other side's last
//! end could already have freed it: a read of freed memory, which TSan
//! reported as a data race (or a heap use after free) in every one-shot
//! channel waitlist kata (1114-1117, 1188). The glue is now read under the
//! lock. Against the TSan-built runtime archive; a missing archive or a
//! `cc` without TSan skips, as in `mir_tsan.rs`.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

fn tsan_available() -> bool {
    if std::env::var_os("KARAC_SKIP_TSAN_TESTS").is_some() {
        return false;
    }
    let dir = std::env::temp_dir().join(format!("karac-tsan-chan-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let c = dir.join("probe.c");
    let exe = dir.join("probe");
    std::fs::write(&c, "int main(void){return 0;}\n").unwrap();
    let ok = Command::new("cc")
        .arg("-fsanitize=thread")
        .arg(&c)
        .arg("-o")
        .arg(&exe)
        .status()
        .is_ok_and(|s| s.success())
        && Command::new(&exe).status().is_ok_and(|s| s.success());
    let _ = std::fs::remove_dir_all(&dir);
    ok
}

#[test]
fn waitlist_channels_whose_ends_drop_on_different_workers_run_clean() {
    if !tsan_available() {
        eprintln!("skipping: no TSan-capable cc");
        return;
    }
    let dir = std::env::temp_dir().join(format!("karac-tsan-chan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("main.kara");
    std::fs::write(
        &src,
        r#"import std.sync.{Sender};
import std.sync.{Mutex, TaskGroup};

struct Turns {
    stage: i64,
    waiting: Vec[(i64, Sender[()])],
}

sync struct Gate {
    turns: Mutex[Turns],
}

impl Gate {
    fn new() -> Gate {
        Gate { turns: Mutex.new(Turns { stage: 0, waiting: [] }) }
    }

    // The receiving end drops on the waiting task, the sending end on
    // whichever task finishes the stage it waits for.
    fn wait_for(self, stage: i64) {
        let (tx, rx) = channel(1);
        {
            let g = self.turns.lock();
            if g.value.stage >= stage {
                return;
            }
            g.value.waiting.push((stage, tx));
        }
        let _ = rx.recv();
    }

    fn finish(self) {
        let g = self.turns.lock();
        g.value.stage += 1;
        let stage = g.value.stage;
        let mut still: Vec[(i64, Sender[()])] = [];
        for (want, tx) in g.value.waiting.drain(0..g.value.waiting.len()) {
            if want <= stage {
                let _ = tx.send(());
            } else {
                still.push((want, tx));
            }
        }
        g.value.waiting = still;
    }
}

fn run(order: i64) -> i64 {
    let gate = Gate.new();
    let done = Mutex.new(0);
    {
        let group = TaskGroup.new();
        for k in 0..3 {
            let stage = (k + order) % 3;
            group.spawn(|| {
                gate.wait_for(stage);
                done.lock().value += stage + 1;
                gate.finish();
            });
        }
    }
    let d = done.lock();
    d.value
}

fn main() {
    let mut total = 0;
    for i in 0..600 {
        total += run(i % 3);
    }
    println(total);
}
"#,
    )
    .unwrap();
    // Before the fix every run reported (5 of 5); two runs keep it cheap.
    for _ in 0..2 {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg("__mir-llvm-tsan")
            .arg(&src)
            .env("KARAC_THREADS", "4")
            .output()
            .expect("run karac");
        let err = String::from_utf8_lossy(&out.stderr);
        let unlinked = err.contains("Linker failed") || err.contains("libkarac_runtime_tsan.a");
        if out.status.code() == Some(3) && unlinked {
            let _ = common::link_or_skip(Err(err.into_owned()));
            return;
        }
        assert_eq!(out.status.code(), Some(0), "stderr:\n{err}");
        assert!(!err.contains("ThreadSanitizer"), "stderr:\n{err}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "3600\n");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
