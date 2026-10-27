//! A struct or enum imported from a gated library module under a new name
//! (`import std.log.{Level as L}`) is the module's own type under a second
//! name, which the module's functions accept (B-2026-10-27-23). Before, the
//! splice renamed the type, so the functions' `Level` was a second, distinct
//! type. A trait has no alias form, so its rename is refused.

use std::path::PathBuf;
use std::process::{Command, Output};

fn write(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-gated-rename-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// `src` passes check and prints `expect` on both run lanes.
fn prints(tag: &str, src: &str, expect: &str) {
    let path = write(tag, src);
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{tag}: check: {}", text(&out.stderr));
    for lane in [&["run"][..], &["run", "--interp"]] {
        let out = karac(lane, &path);
        assert!(
            out.status.success(),
            "{tag} {lane:?}: {}",
            text(&out.stderr)
        );
        assert_eq!(text(&out.stdout), expect, "{tag} {lane:?}");
    }
}

#[test]
fn gated_enum_rename_is_the_same_type() {
    prints(
        "log-level",
        "import std.log;\nimport std.log.{Level as L};\n\nfn main() {\n    log.init(log.text_backend(log.Stream.Stdout), level: L.Debug);\n    log.debug(\"hello\");\n}\n",
        "DEBUG hello\n",
    );
}

#[test]
fn gated_type_rename_in_a_result() {
    prints(
        "decode-error",
        "import std.encoding.{hex_decode, DecodeError as DE};\n\nfn main() {\n    let r: Result[Vec[u8], DE] = hex_decode(\"zz\");\n    match r {\n        Ok(v) => println(v.len()),\n        Err(_) => println(\"bad\"),\n    }\n}\n",
        "bad\n",
    );
}

/// The rename's point: the program has a `Level` of its own. The module's
/// `Level` is renamed apart, unit-variant patterns in its bodies included;
/// before, `match level { Level.Debug => .. }` in the library met the
/// program's `Level` and every record printed as `DEBUG`.
#[test]
fn gated_type_rename_beside_a_program_type_of_its_name() {
    let own = "enum Level { Low, High }\n\nfn show(l: Level) -> String {\n    match l {\n        Level.Low => \"low\",\n        Level.High => \"high\",\n    }\n}\n\n";
    prints(
        "own-level-renamed",
        &format!("import std.log;\nimport std.log.{{Level as LogLevel}};\n\n{own}fn main() {{\n    log.init(log.text_backend(log.Stream.Stdout), level: LogLevel.Info);\n    println(show(Level.High));\n    log.info(\"ready\");\n}}\n"),
        "high\nINFO  ready\n",
    );
    prints(
        "own-level-unlisted",
        &format!("import std.log;\n\n{own}fn main() {{\n    log.init(log.text_backend(log.Stream.Stdout), level: log.Level.Info);\n    println(show(Level.High));\n    log.info(\"ready\");\n    log.warn(\"careful\");\n}}\n"),
        "high\nINFO  ready\nWARN  careful\n",
    );
}

#[test]
fn gated_trait_rename_refused() {
    let path = write(
        "backend",
        "import std.log;\nimport std.log.{Backend as B, Record};\n\nstruct Mine {}\n\nimpl B for Mine {\n    fn emit(mut ref self, record: Record) {\n        println(record.msg);\n    }\n}\n\nfn main() {\n    log.init(Mine {});\n}\n",
    );
    let out = karac(&["check"], &path);
    let err = text(&out.stderr);
    assert!(!out.status.success(), "check must refuse:\n{err}");
    assert!(
        err.contains("main.kara:2:17: `std.log.Backend` is a trait, and a trait cannot be renamed (`as B`) yet; import it as `Backend`"),
        "{err}"
    );
}
