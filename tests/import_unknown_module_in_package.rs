//! B-2026-11-05-42: in a package, `import utils;` naming no module fell back
//! to the crate root's own path, so `src/main.kara` imported itself and the
//! check reported E0223 "circular module dependency: <crate root> → <crate
//! root>" instead of an unknown name. The module graph now adds no edge for
//! a bare import that names nothing, and the resolver reports it, with the
//! sibling module it most likely meant.

use std::process::{Command, Stdio};

#[test]
fn a_bare_import_of_a_missing_module_is_unknown_not_a_cycle() {
    let dir = std::env::temp_dir().join(format!("karac-pkg-unknown-mod-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"app\"\n").unwrap();
    std::fs::write(dir.join("src/util.kara"), "pub fn f() -> i64 { 1 }\n").unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import utils;\nfn main() { println(utils.f()); }\n",
    )
    .unwrap();
    for args in [&["check", "src/main.kara"][..], &["check"][..]] {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .current_dir(&dir)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("karac");
        let all = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.status.success(), "{args:?}: {all}");
        assert!(!all.contains("E0223"), "{args:?}: {all}");
        assert!(!all.contains("circular"), "{args:?}: {all}");
        assert!(all.contains("utils"), "{args:?}: {all}");
        assert!(all.contains("did you mean `util`"), "{args:?}: {all}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
