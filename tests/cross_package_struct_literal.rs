//! B-2026-11-07-22 — design.md §5 Field visibility: a `pub` field is visible
//! to users of the package, so a struct literal from another package may
//! name it. The literal check added for B-2026-11-04-46 refused every field
//! of another package's struct, `pub` or not.

use std::path::{Path, PathBuf};
use std::process::Command;

fn packages(tag: &str, lib: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-xpkg-literal-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let manifest = |name: &str, deps: &str| {
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n{deps}"
        )
    };
    std::fs::create_dir_all(dir.join("lib/src")).unwrap();
    std::fs::create_dir_all(dir.join("app/src")).unwrap();
    std::fs::write(dir.join("lib/kara.toml"), manifest("lib", "")).unwrap();
    std::fs::write(dir.join("lib/src/lib.kara"), lib).unwrap();
    std::fs::write(
        dir.join("app/kara.toml"),
        manifest("app", "lib = { path = \"../lib\" }\n"),
    )
    .unwrap();
    dir
}

fn check(app: &Path, main: &str) -> (bool, String) {
    std::fs::write(app.join("src/main.kara"), main).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .current_dir(app)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn a_literal_of_another_packages_struct_names_its_pub_fields() {
    let dir = packages(
        "pub",
        "pub struct Opts { pub port: i64, pub host: String }\n",
    );
    let (ok, out) = check(
        &dir.join("app"),
        "import lib.{Opts};\nfn main() {\n    let o = Opts { port: 1, host: \"h\".to_string() };\n    println(o.port);\n}\n",
    );
    assert!(ok, "a literal naming only pub fields must check:\n{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_literal_of_another_packages_struct_cannot_name_a_package_field() {
    let dir = packages("plain", "pub struct Opts { pub port: i64, secret: i64 }\n");
    let (ok, out) = check(
        &dir.join("app"),
        "import lib.{Opts};\nfn main() {\n    let o = Opts { port: 1, secret: 2 };\n    println(o.port);\n}\n",
    );
    assert!(
        !ok,
        "a field without `pub` is not visible outside its package:\n{out}"
    );
    assert!(out.contains("E0111") && out.contains("secret"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
