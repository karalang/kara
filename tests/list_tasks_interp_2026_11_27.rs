//! B-2026-11-26-93: `std.runtime.list_tasks()` under `karac run --interp`
//! lists the executor's suspended tasks as `karac run` and `karac build` do
//! (design.md § Debugger contract): ids from 1 (the root's), the spawning
//! task as `parent_task_id` (0 for the root), and what each waits on. It
//! listed none before the fix.

use std::process::Command;

fn run(name: &str, src: &str, args: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("karac_list_tasks_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path.to_str().unwrap())
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{name}: stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const SHOW: &str = "fn show(ts: Vec[TaskInfo]) {\n    for t in ts.iter() {\n        let w = match t.wait_target {\n            WaitTarget.Running => \"running\",\n            WaitTarget.PeerTask(_) => \"peer\",\n            WaitTarget.IoHandle(_) => \"io\",\n            WaitTarget.Sleep => \"sleep\",\n        };\n        println(f\"{t.task_id} {t.parent_task_id} {w}\");\n    }\n}\n";

fn waiting_tasks_src() -> String {
    format!(
        "import std.runtime;\nimport std.time.{{Duration, sleep}};\n{SHOW}fn main() {{\n    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(1);\n    let g = TaskGroup.new();\n    let h = g.spawn(|| rx.recv());\n    let h2 = g.spawn(|| {{ sleep(Duration.ms(500)); 7 }});\n    sleep(Duration.ms(50));\n    show(runtime.list_tasks());\n    let _ = tx.send(1);\n    let _ = h.join();\n    let _ = h2.join();\n}}\n"
    )
}

fn root_listed_src() -> String {
    format!(
        "import std.runtime;\nimport std.time.{{Duration, sleep}};\n{SHOW}fn main() {{\n    let g = TaskGroup.new();\n    let h = g.spawn(|| {{ sleep(Duration.ms(300)); 7 }});\n    let h2 = g.spawn(|| {{ show(runtime.list_tasks()); 1 }});\n    sleep(Duration.ms(50));\n    let _ = h2.join();\n    let _ = h.join();\n}}\n"
    )
}

#[test]
fn list_tasks_under_interp_lists_a_task_parked_on_recv_and_one_asleep() {
    let out = run("waiting", &waiting_tasks_src(), &["run", "--interp"]);
    assert_eq!(out, "2 1 peer\n3 1 sleep\n");
}

#[test]
fn list_tasks_under_interp_lists_the_sleeping_root_from_a_spawned_task() {
    let out = run("root", &root_listed_src(), &["run", "--interp"]);
    assert_eq!(out, "1 0 sleep\n2 1 sleep\n");
}

#[cfg(feature = "llvm")]
#[test]
fn list_tasks_agrees_between_interp_and_the_jit() {
    for (name, src) in [("w_jit", waiting_tasks_src()), ("r_jit", root_listed_src())] {
        if !std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/release/libkarac_runtime.a")
            .exists()
            && std::env::var_os("KARAC_RUNTIME").is_none()
        {
            return;
        }
        assert_eq!(
            run(name, &src, &["run"]),
            run(&format!("{name}_i"), &src, &["run", "--interp"]),
            "{name}"
        );
    }
}
