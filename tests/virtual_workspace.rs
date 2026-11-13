//! Workspaces (design.md § Workspaces, B-2026-11-13-50): a root with only
//! `[workspace]` and `[workspace.dependencies]` is a valid virtual root, a
//! command run at it runs in each member, and a `path` in
//! `[workspace.dependencies]` is relative to the manifest that declares it.
//! A member's `workspace = true` was E_WORKSPACE_DEP_OUTSIDE_WORKSPACE under
//! a virtual root, the root itself was "missing the required `[package]`
//! section", and a workspace path resolved against the member's directory.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// `<tmp>/karac-<tag>-<pid>/ws` with members `core` (a binary importing `util.hello`) and
/// `util` (a library); `root_toml` is the root's manifest.
fn workspace(tag: &str, root_toml: &str) -> PathBuf {
    // One level down, so the root's `../util` names nothing that exists.
    let base = std::env::temp_dir().join(format!("karac-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let ws = base.join("ws");
    std::fs::create_dir_all(ws.join("core/src")).unwrap();
    std::fs::create_dir_all(ws.join("util/src")).unwrap();
    std::fs::write(ws.join("kara.toml"), root_toml).unwrap();
    std::fs::write(
        ws.join("core/kara.toml"),
        "[package]\nname = \"core\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nutil = { workspace = true }\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("core/src/main.kara"),
        "import util.hello;\nfn main() { println(hello()); }\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("util/kara.toml"),
        "[package]\nname = \"util\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("util/src/lib.kara"),
        "pub fn hello() -> String { \"from the workspace\" }\n",
    )
    .unwrap();
    ws
}

fn karac(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout)
}

const VIRTUAL: &str = "[workspace]\nmembers = [\"core\", \"util\"]\n\n\
                       [workspace.dependencies]\nutil = { path = \"util\" }\n";

#[test]
fn a_member_of_a_virtual_root_takes_its_workspace_dependencies() {
    let ws = workspace("ws-virtual-member", VIRTUAL);
    let check = karac(&["check"], &ws.join("core"));
    let run = karac(&["run", "src/main.kara"], &ws.join("core"));
    let _ = std::fs::remove_dir_all(ws.parent().unwrap());
    assert!(check.status.success(), "{}", text(&check));
    assert!(run.status.success(), "{}", text(&run));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "from the workspace\n");
}

#[test]
fn a_command_at_a_virtual_root_runs_in_each_member() {
    let ws = workspace("ws-virtual-root", VIRTUAL);
    let check = karac(&["check"], &ws);
    std::fs::write(
        ws.join("util/src/lib.kara"),
        "pub fn hello() -> String { 1 }\n",
    )
    .unwrap();
    let broken = karac(&["check"], &ws);
    let _ = std::fs::remove_dir_all(ws.parent().unwrap());
    let check_text = text(&check);
    assert!(check.status.success(), "{check_text}");
    assert!(check_text.contains("==> core"), "{check_text}");
    assert!(check_text.contains("==> util"), "{check_text}");
    assert!(!check_text.contains("[package]"), "{check_text}");
    assert!(!broken.status.success(), "{}", text(&broken));
}

#[test]
fn a_workspace_dependency_path_is_relative_to_the_root_that_declares_it() {
    // A root with a `[package]` of its own, the member one level down: the
    // member's `workspace = true` finds `util` beside the root's manifest.
    let root = "[package]\nname = \"ws\"\nversion = \"0.1.0\"\n\n\
                [workspace]\nmembers = [\"core\"]\n\n\
                [workspace.dependencies]\nutil = { path = \"util\" }\n";
    let ws = workspace("ws-dep-path", root);
    let run = karac(&["run", "src/main.kara"], &ws.join("core"));
    // `../util` named it from the member's directory, which is not where
    // the root's manifest says it is.
    std::fs::write(
        ws.join("kara.toml"),
        root.replace("path = \"util\"", "path = \"../util\""),
    )
    .unwrap();
    let from_member = karac(&["check"], &ws.join("core"));
    let _ = std::fs::remove_dir_all(ws.parent().unwrap());
    assert!(run.status.success(), "{}", text(&run));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "from the workspace\n");
    assert!(!from_member.status.success(), "{}", text(&from_member));
}

#[test]
fn a_virtual_root_with_no_members_says_so() {
    let ws = workspace(
        "ws-no-members",
        "[workspace]\n\n[workspace.dependencies]\nutil = { path = \"util\" }\n",
    );
    let check = karac(&["check"], &ws);
    let _ = std::fs::remove_dir_all(ws.parent().unwrap());
    assert!(!check.status.success());
    assert!(
        text(&check).contains("no `[workspace].members`"),
        "{}",
        text(&check)
    );
}
