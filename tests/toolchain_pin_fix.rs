//! A `karac-toolchain.toml` whose `version` is a bare number carries the
//! one edit its help names, the quotes, and `karac fix` applies it to the
//! pin file: a manifest is project source, and the edit is determined
//! (review-thread ruling 2026-11-23, B-2026-11-23-2). Each test applies the
//! fix and checks clean afterwards; a pin the quotes do not mend carries
//! none and is left as written.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Every call gets a directory of its own, whatever names two tests pass
/// (B-2026-11-23-13).
static CALL: AtomicUsize = AtomicUsize::new(0);

fn karac(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A package with one clean source file and `pin` as its toolchain pin.
fn package(name: &str, pin: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-toolchain-pin-fix-{name}-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "fn main() {\n    println(1);\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("karac-toolchain.toml"), pin).unwrap();
    dir
}

fn pin_of(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("karac-toolchain.toml")).unwrap()
}

/// The check's one JSON record.
fn record(dir: &Path) -> serde_json::Value {
    let out = karac(dir, &["check", "--output=json", "src/main.kara"]);
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&out)));
    v["diagnostics"][0].clone()
}

/// `version = 0.8` is a float: E_TOOLCHAIN_INVALID_VERSION_TYPE. Its record
/// names the pin file and the edit, with offsets into that file, and
/// `karac fix <file>` writes it; the package then checks clean.
#[test]
fn a_float_version_is_quoted_by_fix_and_the_package_checks_clean() {
    let pin = "# the pin\nversion = 0.8  # this release line\ntargets = []\n";
    let dir = package("float", pin);
    let before = karac(&dir, &["check", "src/main.kara"]);
    assert!(!before.status.success());
    let shown = text(&before);
    assert!(
        shown.contains("error[E_TOOLCHAIN_INVALID_VERSION_TYPE]"),
        "{shown}"
    );
    assert!(
        shown.contains("= help: write it as a string: `version = \"0.8\"`"),
        "{shown}"
    );

    let d = record(&dir);
    assert_eq!(d["code"], "E_TOOLCHAIN_INVALID_VERSION_TYPE", "{d}");
    assert!(
        d["file"]
            .as_str()
            .is_some_and(|f| f.ends_with("karac-toolchain.toml")),
        "{d}"
    );
    let edit = &d["fix_diff"][0];
    let (at, len) = (
        edit["offset"].as_u64().unwrap() as usize,
        edit["length"].as_u64().unwrap() as usize,
    );
    assert_eq!(&pin[at..at + len], "0.8", "{d}");
    assert_eq!(edit["text"], "\"0.8\"", "{d}");

    let fix = karac(&dir, &["fix", "src/main.kara"]);
    assert!(fix.status.success(), "{}", text(&fix));
    assert!(
        text(&fix).contains("applied 1 fix(es) to karac-toolchain.toml"),
        "{}",
        text(&fix)
    );
    assert_eq!(
        pin_of(&dir),
        "# the pin\nversion = \"0.8\"  # this release line\ntargets = []\n"
    );
    let after = karac(&dir, &["check", "src/main.kara"]);
    assert!(after.status.success(), "{}", text(&after));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `version = 0.8.1` is not TOML at all (E_TOOLCHAIN_INVALID_TOML), and the
/// quotes mend it the same way. `karac fix` with no file fixes the package,
/// its pin included.
#[test]
fn a_three_part_version_is_quoted_by_the_package_fix() {
    let dir = package("three", "version = 0.8.1\n");
    let before = karac(&dir, &["check"]);
    assert!(!before.status.success());
    let shown = text(&before);
    assert!(shown.contains("error[E_TOOLCHAIN_INVALID_TOML]"), "{shown}");
    assert!(
        shown.contains("= help: write it as a string: `version = \"0.8.1\"`"),
        "{shown}"
    );
    let fix = karac(&dir, &["fix"]);
    assert!(fix.status.success(), "{}", text(&fix));
    assert_eq!(pin_of(&dir), "version = \"0.8.1\"\n");
    let after = karac(&dir, &["check"]);
    assert!(after.status.success(), "{}", text(&after));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A dry run names the edit and writes nothing.
#[test]
fn a_dry_run_names_the_pin_edit_and_leaves_the_file() {
    let dir = package("dry", "version = 0.8\n");
    let out = karac(&dir, &["fix", "--dry-run", "src/main.kara"]);
    let shown = text(&out);
    assert!(
        shown.contains("would apply 1 fix(es) to karac-toolchain.toml:")
            && shown.contains("karac-toolchain.toml:1:11: `0.8` → `\"0.8\"`"),
        "{shown}"
    );
    assert_eq!(pin_of(&dir), "version = 0.8\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `version` the quotes do not turn into a constraint, and a file broken
/// somewhere else, carry no help and no edit, and `karac fix` leaves them.
#[test]
fn a_pin_the_quotes_do_not_mend_carries_no_edit() {
    for (name, pin, code) in [
        (
            "bool",
            "version = true\n",
            "E_TOOLCHAIN_INVALID_VERSION_TYPE",
        ),
        (
            "list",
            "version = [0.8]\n",
            "E_TOOLCHAIN_INVALID_VERSION_TYPE",
        ),
        (
            "elsewhere",
            "version = 0.8.1\ntargets = [\n",
            "E_TOOLCHAIN_INVALID_TOML",
        ),
    ] {
        let dir = package(name, pin);
        let d = record(&dir);
        assert_eq!(d["code"], code, "{name}: {d}");
        assert!(d.get("fix_diff").is_none(), "{name}: {d}");
        assert!(d.get("help").is_none(), "{name}: {d}");
        let fix = karac(&dir, &["fix", "src/main.kara"]);
        assert!(
            !text(&fix).contains("karac-toolchain.toml"),
            "{name}: {}",
            text(&fix)
        );
        assert_eq!(pin_of(&dir), pin, "{name}");
        let after = karac(&dir, &["check", "src/main.kara"]);
        assert!(!after.status.success(), "{name}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
