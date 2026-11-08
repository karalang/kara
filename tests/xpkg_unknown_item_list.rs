//! E0113 for an unknown item in a dependency lists only the names the
//! importer could import: across packages a name that is not `pub` is E0111,
//! so it is not "available", and a typo of one is not suggested
//! (B-2026-11-08-35).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("karac-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let lib = root.join("textkit");
    let app = root.join("app");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        lib.join("kara.toml"),
        "[package]\nname = \"textkit\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .unwrap();
    std::fs::write(
        lib.join("src/lib.kara"),
        "pub fn version() -> i64 { 1 }\n\
         pub fn shout(n: i64) -> i64 { clean(n) * 2 }\n\
         fn clean(n: i64) -> i64 { n }\n",
    )
    .unwrap();
    std::fs::write(
        app.join("kara.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
         [dependencies]\ntextkit = { path = \"../textkit\" }\n",
    )
    .unwrap();
    root
}

fn check(root: &Path, main: &str) -> String {
    let app = root.join("app");
    std::fs::write(app.join("src/main.kara"), main).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "src/main.kara"])
        .current_dir(&app)
        .output()
        .unwrap();
    assert!(!out.status.success(), "check must refuse");
    String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout)
}

#[test]
fn an_unknown_dependency_item_lists_only_its_pub_names() {
    let root = fixture("xpkg-e0113-list");
    let all = check(
        &root,
        "import textkit.nope;\nfn main() { println(textkit.version()); }\n",
    );
    let _ = std::fs::remove_dir_all(&root);
    assert!(all.contains("error[E0113]"), "{all}");
    assert!(all.contains("available: version, shout"), "{all}");
    assert!(
        !all.contains("clean"),
        "a package-internal fn is listed: {all}"
    );
}

#[test]
fn a_typo_of_a_package_internal_name_is_not_suggested() {
    let root = fixture("xpkg-e0113-typo");
    let all = check(&root, "import textkit.clea;\nfn main() { }\n");
    let _ = std::fs::remove_dir_all(&root);
    assert!(all.contains("error[E0113]"), "{all}");
    assert!(!all.contains("did you mean `clean`"), "{all}");
}
