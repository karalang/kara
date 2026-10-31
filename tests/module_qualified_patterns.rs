//! B-2026-10-27-24: a pattern names a variant or const through a module
//! binding (`import shapes;` then `shapes.Shape.Circle(r) =>`), exactly as the
//! expression `shapes.Shape.Circle(1)` does. The parser used to root a pattern
//! path only at an uppercase identifier, so `log.Level.Debug =>` failed with
//! "Expected FatArrow, found Dot".
//!
//! The single-file `std` binding is pinned by
//! `corpus/core/module_qualified_pattern`; these cover project mode, where
//! the binding names a user module, and every pattern shape a path roots.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "karac-modpat-{}-{}-{}",
        std::process::id(),
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    tmp
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

const SHAPES: &str =
    "pub enum Shape {\n    Circle(i64),\n    Rect { w: i64, h: i64 },\n    Dot,\n}\n\n\
                      pub const LOW: i64 = 1;\npub const HIGH: i64 = 9;\n";

/// A package whose `src/shapes.kara` is `SHAPES`, run as `karac <cmd>`.
fn run_package(tag: &str, cmd: &str, main_src: &str) -> (bool, String, String) {
    let tmp = scratch(tag);
    write(&tmp.join("kara.toml"), "[package]\nname = \"modpat\"\n");
    write(&tmp.join("src/shapes.kara"), SHAPES);
    write(&tmp.join("src/main.kara"), main_src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&tmp)
        .arg(cmd)
        .arg("src/main.kara")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_pattern_reaches_every_variant_shape_and_a_const_through_a_module_binding() {
    let src = "import shapes;\n\n\
               fn area(s: shapes.Shape) -> i64 {\n    match s {\n\
               \x20       shapes.Shape.Circle(r) => 3 * r * r,\n\
               \x20       shapes.Shape.Rect { w, h } => w * h,\n\
               \x20       shapes.Shape.Dot => 0,\n    }\n}\n\n\
               fn band(n: i64) -> String {\n    match n {\n\
               \x20       shapes.LOW..=shapes.HIGH => \"digit\",\n\
               \x20       _ => \"other\",\n    }\n}\n\n\
               fn main() {\n\
               \x20   println(area(shapes.Shape.Circle(2)));\n\
               \x20   println(area(shapes.Shape.Rect { w: 3, h: 4 }));\n\
               \x20   println(area(shapes.Shape.Dot));\n\
               \x20   println(band(5));\n\
               \x20   println(band(10));\n}\n";
    let (ok, stdout, stderr) = run_package("shapes", "run", src);
    assert!(ok, "run must accept it: {stdout}{stderr}");
    assert_eq!(stdout, "12\n12\n0\ndigit\nother\n", "{stderr}");
}

#[test]
fn a_pattern_through_a_module_binding_names_a_missing_variant() {
    // The desugar turns `shapes.Shape.Square` into `Shape.Square`, so the
    // ordinary unknown-variant diagnostic answers, not a parse error.
    let src = "import shapes;\n\n\
               fn f(s: shapes.Shape) -> i64 {\n    match s {\n\
               \x20       shapes.Shape.Square => 1,\n        _ => 0,\n    }\n}\n\n\
               fn main() {\n    println(f(shapes.Shape.Dot));\n}\n";
    let (ok, stdout, stderr) = run_package("missing", "check", src);
    let all = format!("{stdout}{stderr}");
    assert!(!ok, "check must refuse it: {all}");
    assert!(
        all.contains("Square") && !all.contains("Expected `=>`"),
        "expected a diagnostic naming `Square`: {all}"
    );
}

#[test]
fn a_dotted_pattern_whose_root_is_unbound_is_refused_not_a_catch_all() {
    // Before the parser accepted a lowercase root, `zzz.Mode.Slow =>` was a
    // parse error. Accepted, it must still name something: an unknown root
    // is `undefined name`, as in a struct or tuple-variant path, rather than
    // a fresh binding that matches every value.
    let tmp = scratch("unbound");
    let path = tmp.join("main.kara");
    write(
        &path,
        "enum Mode {\n    Fast,\n    Slow,\n}\n\n\
         fn main() {\n    let m = Mode.Fast;\n    match m {\n\
         \x20       zzz.Mode.Slow => println(\"slow\"),\n\
         \x20       _ => println(\"other\"),\n    }\n}\n",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "check must refuse it: {all}");
    assert!(
        all.contains("undefined name 'zzz'"),
        "expected `undefined name 'zzz'`: {all}"
    );
}
