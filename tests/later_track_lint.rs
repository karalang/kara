//! The `later_track` warning (W0292, B-2026-11-01-41): `#[gpu]` and
//! `comptime` beyond `#[derive]` are later tracks (core-semantics.md §13).
//! They compile and run as ordinary code, and `karac check` says so on the
//! corpus pins for each; a derive backer stays silent.

use std::process::{Command, Stdio};

fn check_json(path: &std::path::Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "--output=json", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn pin(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus/core")
        .join(name)
        .join("source.kara")
}

#[test]
fn later_track_lint_warns_on_a_comptime_fn() {
    let json = check_json(&pin("ok_later_track_comptime"));
    assert!(json.contains("\"code\":\"W0292\""), "{json}");
    assert!(json.contains("may be evaluated at run time"), "{json}");
}

#[test]
fn later_track_lint_warns_on_a_gpu_fn() {
    let json = check_json(&pin("ok_later_track_gpu"));
    assert!(json.contains("\"code\":\"W0292\""), "{json}");
    assert!(json.contains("runs on the CPU"), "{json}");
}

#[test]
fn later_track_lint_leaves_a_derive_backer_alone() {
    let path = std::env::temp_dir().join(format!(
        "karac-later-track-derive-{}.kara",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "comptime fn name_of(comptime T: Type) -> String { T.name() }
comptime fn derive_describe(comptime T: Type) -> Vec[Item] {
    let n = name_of(T);
    [ast.item(\"impl \" + n + \" { fn describe(self) -> String { \\\"a \" + n + \"\\\" } }\")]
}
#[derive(Describe)]
struct Widget { id: i64 }
fn main() {
    let w = Widget { id: 7 };
    println(w.describe());
}
",
    )
    .unwrap();
    let json = check_json(&path);
    let _ = std::fs::remove_file(&path);
    assert!(!json.contains("W0292"), "{json}");
}
