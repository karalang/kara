//! B-2026-11-07-23 — design.md § `#[non_exhaustive]`: outside the defining
//! package a match on a `#[non_exhaustive]` enum needs a wildcard arm even
//! when it names every current variant, wherever the enum sits in the
//! scrutinee, and `karac fix` adds it; a struct literal is refused and a
//! struct pattern needs `..`. The defining package matches its own enum
//! exhaustively. The rule fired only for library types, and only when no
//! qualified variant (`Kind.A`) was read as a catch-all binding.

use std::path::{Path, PathBuf};
use std::process::Command;

const LIB: &str = "#[non_exhaustive]\npub enum Kind { A, B }\n\
pub fn get() -> Kind { Kind.A }\n\
pub fn name(k: Kind) -> String {\n    match k { Kind.A => \"a\", Kind.B => \"b\" }\n}\n\
#[non_exhaustive]\npub struct Opts { pub port: i64 }\n\
pub fn mk() -> Opts { Opts { port: 3 } }\n";

fn packages(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-xpkg-non-exhaustive-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let manifest = |name: &str, deps: &str| {
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n{deps}"
        )
    };
    std::fs::create_dir_all(dir.join("lib/src")).unwrap();
    std::fs::create_dir_all(dir.join("app/src")).unwrap();
    std::fs::write(dir.join("lib/kara.toml"), manifest("lib", "")).unwrap();
    std::fs::write(dir.join("lib/src/lib.kara"), LIB).unwrap();
    std::fs::write(
        dir.join("app/kara.toml"),
        manifest("app", "lib = { path = \"../lib\" }\n"),
    )
    .unwrap();
    dir
}

fn karac(app: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
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
fn a_match_on_another_packages_non_exhaustive_enum_needs_a_wildcard() {
    let dir = packages("match");
    let app = dir.join("app");
    std::fs::write(
        app.join("src/main.kara"),
        "import lib.{Kind, get};\nfn main() {\n    \
         match get() { Kind.A => println(\"a\"), Kind.B => println(\"b\") }\n    \
         match Some(get()) { Some(Kind.A) => println(\"a\"), Some(Kind.B) => println(\"b\"), None => println(\"none\") }\n}\n",
    )
    .unwrap();
    let (ok, out) = karac(&app, &["check"]);
    assert!(
        !ok,
        "naming every variant is not enough outside the package:\n{out}"
    );
    assert_eq!(
        out.matches("E_NON_EXHAUSTIVE_CROSS_PACKAGE_MATCH").count(),
        2,
        "the top-level match and the nested one are both refused:\n{out}"
    );
    let (ok, out) = karac(&app, &["fix", "src/main.kara"]);
    assert!(ok, "{out}");
    let fixed = std::fs::read_to_string(app.join("src/main.kara")).unwrap();
    assert!(
        fixed.contains("_ => panic(\"handle new variant\")")
            && fixed.contains("Some(_) => panic(\"handle new variant\")"),
        "karac fix adds the wildcard arms:\n{fixed}"
    );
    let (ok, out) = karac(&app, &["check"]);
    assert!(ok, "the fixed program checks:\n{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_non_exhaustive_struct_of_another_package_is_not_built_or_destructured_whole() {
    let dir = packages("struct");
    let app = dir.join("app");
    std::fs::write(
        app.join("src/main.kara"),
        "import lib.{Opts, mk};\nfn main() {\n    let o = Opts { port: 1 };\n    \
         let Opts { port } = mk();\n    println(f\"{o.port} {port}\");\n}\n",
    )
    .unwrap();
    let (ok, out) = karac(&app, &["check"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("E_NON_EXHAUSTIVE_CROSS_PACKAGE_LITERAL")
            && out.contains("E_NON_EXHAUSTIVE_CROSS_PACKAGE_PATTERN"),
        "{out}"
    );
    std::fs::write(
        app.join("src/main.kara"),
        "import lib.{Opts, mk};\nfn main() {\n    let Opts { port, .. } = mk();\n    println(port);\n}\n",
    )
    .unwrap();
    let (ok, out) = karac(&app, &["check"]);
    assert!(
        ok,
        "a pattern with `..` and the package's constructor check:\n{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
