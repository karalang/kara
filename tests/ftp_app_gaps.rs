//! Gaps an FTP server port met (kara-katas apps/service/ftp_server):
//! `rsplit` is named with Kāra's `rsplit_once` rather than offered as
//! `split`, which starts from the other end (B-2026-11-13-71), and
//! `group.join()` on a `TaskGroup` is repaired to `drop(group)`, which
//! joins every task the group started (B-2026-11-13-72).

use std::path::PathBuf;
use std::process::{Command, Output};

fn scratch(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-ftp-gaps-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    file
}

fn karac(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout)
}

#[test]
fn rsplit_names_rsplit_once() {
    let file = scratch(
        "rsplit",
        "fn main() {\n    let addr = \"127.0.0.1:2121\";\n    println(addr.rsplit(\":\").next().unwrap());\n}\n",
    );
    let out = karac(&["check", file.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
    let shown = text(&out);
    assert!(!out.status.success(), "{shown}");
    assert!(shown.contains("`rsplit_once(sep)`"), "{shown}");
    assert!(!shown.contains("did you mean 'split'"), "{shown}");
}

#[test]
fn taskgroup_join_is_repaired_to_drop() {
    let file = scratch(
        "join",
        "import std.sync.{TaskGroup, Mutex};\n\n\
         fn main() {\n    let total = Mutex.new(0);\n    let group = TaskGroup.new();\n    \
         for i in 1..4 {\n        group.spawn(|| {\n            let mut g = total.lock();\n            \
         g.value += i;\n        });\n    }\n    group.join();\n    println(f\"{total.lock().value}\");\n}\n",
    );
    let path = file.to_str().unwrap();
    let before = karac(&["check", path]);
    let fixed = karac(&["fix", path]);
    let after = std::fs::read_to_string(&file).unwrap();
    let run = karac(&["__mir-run", path]);
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
    let shown = text(&before);
    assert!(
        shown.contains("dropping a group joins every task it started"),
        "{shown}"
    );
    assert!(fixed.status.success(), "{}", text(&fixed));
    assert!(after.contains("    drop(group);\n"), "{after}");
    assert!(run.status.success(), "{}", text(&run));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "6\n");
}
