//! The runtime debug metadata of design.md § Debugger contract
//! (B-2026-11-05-34): a compiled build lists the `par {}` blocks running
//! (element 1's spawn-site id, the block's place, its branch count and
//! element 2's parent task) and the suspended tasks (element 3's
//! `WaitTarget`, the place of the spawn, the parent), and `--release`
//! strips it: `has_debug_metadata()` is false and both lists are empty.
//! kara.toml's `runtime_debug_metadata` key overrides either default.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// A `par` block whose first branch lists the blocks running; the
/// listing outside it is empty.
const PAR: &str = r#"import std.runtime;

fn probe() -> i64 {
    let blocks = runtime.list_par_blocks();
    for b in blocks.iter() {
        println(f"block line {b.line} col {b.col} workers {b.worker_count} parent {b.parent_task_id} id {b.spawn_site_id != 0}");
        for w in b.worker_locations.iter() {
            let at: Vec[Str] = w.split(":").collect();
            println(f"worker line {at[at.len() - 1]}");
        }
    }
    blocks.len()
}

fn main() {
    println(runtime.has_debug_metadata());
    let (a, b) = par {
        probe(),
        2,
    };
    println(a + b);
    println(runtime.list_par_blocks().len());
}
"#;

/// Three spawned tasks suspended while `main` lists them: one on a
/// channel, one joining a task it spawned (`PeerTask` names it), and that
/// task asleep.
const TASKS: &str = r#"import std.runtime;

fn wait_name(w: WaitTarget) -> String {
    match w {
        WaitTarget.Running => "running",
        WaitTarget.PeerTask(id) => f"peer {id}",
        WaitTarget.IoHandle(fd) => f"io {fd}",
        WaitTarget.Sleep => "sleep",
    }
}

fn main() {
    let (tx, rx) = channel(1);
    let group = TaskGroup.new();
    let a = group.spawn(|| { let v: i64 = rx.recv().unwrap(); v });
    let c = group.spawn(|| {
        let inner = TaskGroup.new();
        let b = inner.spawn(|| { sleep_ms(200); 5 });
        b.join() + 1
    });
    sleep_ms(20);
    for t in runtime.list_tasks().iter() {
        println(f"{t.task_id}: {wait_name(t.wait_target)} line {t.line} parent {t.parent_task_id} id {t.spawn_site_id != 0}");
    }
    tx.send(7).unwrap();
    println(a.join() + c.join());
}
"#;

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-debug-metadata-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Builds `src` as `name`, with `--release` or without, and runs it: its
/// stdout, or `None` without a linker.
fn run(dir: &Path, name: &str, src: &str, release: bool) -> Option<String> {
    let file = dir.join(format!("{name}.kara"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(name);
    let mut args = vec!["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()];
    if release {
        args.push("--release");
    }
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(&args)
        .output()
        .unwrap();
    let why = format!(
        "{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    })?;
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success(), "{name}: {out:?}");
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

#[test]
fn mir_debug_metadata_lists_running_par_blocks() {
    let dir = scratch("par");
    if let Some(out) = run(&dir, "par", PAR, false) {
        // The block is the `par` on line 17, two branches (lines 18 and
        // 19), run by `main`.
        assert_eq!(
            out, "true\nblock line 17 col 18 workers 2 parent 1 id true\nworker line 18\nworker line 19\n3\n0\n",
            "dev build"
        );
    }
    if let Some(out) = run(&dir, "par_release", PAR, true) {
        assert_eq!(out, "false\n2\n0\n", "release build");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mir_debug_metadata_lists_suspended_tasks() {
    let dir = scratch("tasks");
    if let Some(out) = run(&dir, "tasks", TASKS, false) {
        // Task 2 waits on the channel and task 3 joins task 4, which
        // sleeps; `main` (task 1) spawned 2 and 3, and 3 spawned 4.
        assert_eq!(
            out,
            "2: peer 0 line 15 parent 1 id true\n3: peer 4 line 16 parent 1 id true\n4: sleep line 18 parent 3 id true\n13\n",
            "dev build"
        );
    }
    if let Some(out) = run(&dir, "tasks_release", TASKS, true) {
        assert_eq!(out, "13\n", "release build");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `runtime_debug_metadata` in kara.toml's profiles overrides the default
/// both ways: off in a dev build, on in a release one (B-2026-11-05-36).
#[test]
fn mir_debug_metadata_follows_the_manifest_profiles() {
    let dir = scratch("manifest");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"dm\"\nversion = \"0.1.0\"\n\n[profile.dev]\nruntime_debug_metadata = false\n\n[profile.release]\nruntime_debug_metadata = true\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import std.runtime;\n\nfn main() {\n    println(runtime.has_debug_metadata());\n}\n",
    )
    .unwrap();
    for (release, want) in [(false, "false\n"), (true, "true\n")] {
        let exe = dir.join(if release { "rel" } else { "dev" });
        let mut args = vec!["build", "-o", exe.to_str().unwrap()];
        if release {
            args.push("--release");
        }
        let built = Command::new(env!("CARGO_BIN_EXE_karac"))
            .current_dir(&dir)
            .args(&args)
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
            continue;
        }
        let out = Command::new(&exe).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "release {release}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
