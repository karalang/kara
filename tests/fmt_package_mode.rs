//! `karac fmt` in a package (B-2026-10-30-30): with no path it formats every
//! `.kara` file under `src/` in place, `--check` writes nothing, lists the
//! files that would change and exits 1, a directory is formatted in place, a
//! lone file still prints to stdout unless `--write` is given, and a file
//! that does not parse is reported while the rest are formatted.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const MESSY: &str = "fn main() {\nlet x = 1;\n    println(f\"{x}\");\n}\n";
const TIDY: &str = "fn main() {\n    let x = 1;\n    println(f\"{x}\");\n}\n";
const HELPER_MESSY: &str = "pub fn two() -> i64 {\n2\n}\n";
const HELPER_TIDY: &str = "pub fn two() -> i64 {\n    2\n}\n";

fn package(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-fmt-pkg-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/util")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/main.kara"), MESSY).unwrap();
    std::fs::write(dir.join("src/util/helper.kara"), HELPER_MESSY).unwrap();
    dir
}

fn karac(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn read(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap()
}

#[test]
fn fmt_check_lists_the_files_that_would_change_and_writes_nothing() {
    let dir = package("check");
    let out = karac(&dir, &["fmt", "--check"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "src/main.kara\nsrc/util/helper.kara\n"
    );
    assert_eq!(read(&dir, "src/main.kara"), MESSY);
    // From a subdirectory, the package is still the one around it.
    let sub = karac(&dir.join("src/util"), &["fmt", "--check"]);
    assert_eq!(sub.status.code(), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fmt_with_no_path_formats_the_package_in_place_then_check_passes() {
    let dir = package("write");
    let out = karac(&dir, &["fmt"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    assert_eq!(read(&dir, "src/main.kara"), TIDY);
    assert_eq!(read(&dir, "src/util/helper.kara"), HELPER_TIDY);
    let again = karac(&dir, &["fmt", "--check"]);
    assert!(again.status.success());
    assert!(again.stdout.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fmt_of_a_directory_writes_in_place_and_a_file_prints_unless_write() {
    let dir = package("paths");
    let one = karac(&dir, &["fmt", "src/main.kara"]);
    assert!(one.status.success());
    assert_eq!(String::from_utf8_lossy(&one.stdout), TIDY);
    assert_eq!(read(&dir, "src/main.kara"), MESSY);
    let written = karac(&dir, &["fmt", "--write", "src/main.kara"]);
    assert!(written.status.success());
    assert_eq!(read(&dir, "src/main.kara"), TIDY);
    let tree = karac(&dir, &["fmt", "src/util"]);
    assert!(tree.status.success());
    assert_eq!(read(&dir, "src/util/helper.kara"), HELPER_TIDY);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fmt_reports_a_file_that_does_not_parse_and_formats_the_rest() {
    let dir = package("broken");
    std::fs::write(dir.join("src/bad.kara"), "fn main( {\n").unwrap();
    let out = karac(&dir, &["fmt"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("error[parse]: src/bad.kara:"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(read(&dir, "src/main.kara"), TIDY);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fmt_with_no_path_outside_a_package_asks_for_one() {
    let dir = std::env::temp_dir().join(format!("karac-fmt-nopkg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = karac(&dir, &["fmt"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("`karac fmt <path>`"));
}
