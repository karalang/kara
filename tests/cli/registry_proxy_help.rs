//! A registry dependency with no proxy configured is refused, and the help
//! names both ways out: set `KARAC_REGISTRY_PROXY` to fetch it, or use a
//! `path` dependency (design.md §4, "Dependencies in 0.8"). It used to say
//! registry dependencies are not in 0.8 at all.

use std::process::Command;

#[test]
fn registry_dep_refusal_help_names_the_registry_proxy() {
    let dir = std::env::temp_dir().join(format!("karac-reg-help-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"proj\"\n\n[dependencies]\nhttp = \"1.2\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/main.kara"), "fn main() {}\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .env_remove("KARAC_REGISTRY_PROXY")
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{all}");
    assert!(all.contains("E_REGISTRY_DEP_UNSUPPORTED"), "{all}");
    assert!(all.contains("set `KARAC_REGISTRY_PROXY`"), "{all}");
    assert!(all.contains("`path = \"...\"` dependency"), "{all}");
    assert!(!all.contains("not in 0.8"), "{all}");
}
