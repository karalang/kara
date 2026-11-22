//! What a package's `karac check --output=json` carries, against a lone
//! file's feed and the text render (B-2026-11-21-75, -79), and two codes
//! `karac explain` lists as retired that fired anyway (B-2026-11-21-76).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A fresh package directory holding `files` (path, text).
fn package(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-json-parity-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    dir
}

fn diagnostics(json: &str) -> Vec<serde_json::Value> {
    let doc: serde_json::Value =
        serde_json::from_str(json.trim()).unwrap_or_else(|e| panic!("{e}: {json}"));
    doc["diagnostics"].as_array().cloned().unwrap_or_default()
}

fn codes(json: &str) -> Vec<String> {
    diagnostics(json)
        .iter()
        .map(|d| d["code"].as_str().unwrap_or("").to_string())
        .collect()
}

const MANIFEST: &str = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n";

/// A module other than the root has its spans rebased past the root's;
/// its records' edits are offsets into its own file, and carry the fixes
/// a lone file's check gives (`n as bool`, the irrefutable `if let`).
#[test]
fn a_package_modules_type_diagnostics_carry_their_fixes_at_offsets_into_its_file() {
    let util = "pub fn f() -> i64 {\n    let t = (1, 2);\n    let n = 3;\n    if n as bool {\n        println(1);\n    }\n    if let (a, b) = t {\n        a + b\n    } else {\n        0\n    }\n}\n";
    let dir = package(
        "module-fixes",
        &[
            ("kara.toml", MANIFEST),
            (
                "src/main.kara",
                "import util.{f};\n\nfn main() {\n    println(f());\n}\n",
            ),
            ("src/util.kara", util),
        ],
    );
    let (_, out, _) = karac(&dir, &["check", "--output=json"]);
    let diags = diagnostics(&out);
    let cast = diags
        .iter()
        .find(|d| d["code"] == "E_INT_AS_BOOL")
        .unwrap_or_else(|| panic!("{out}"));
    assert_eq!(cast["file"], "src/util.kara", "{out}");
    let r = &cast["replacement"];
    let at = r["offset"].as_u64().unwrap() as usize;
    let len = r["length"].as_u64().unwrap() as usize;
    assert_eq!(&util[at..at + len], " as bool", "{out}");
    assert_eq!(r["text"], " != 0", "{out}");
    let iflet = diags
        .iter()
        .find(|d| d["code"] == "W0302")
        .unwrap_or_else(|| panic!("{out}"));
    assert!(iflet["fix_diff"].is_array(), "{out}");
    karac(&dir, &["fix", "src/util.kara"]);
    let (_, out, _) = karac(&dir, &["check", "--output=json"]);
    let left: Vec<_> = codes(&out).into_iter().filter(|c| c != "E0400").collect();
    assert!(left.is_empty(), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The spec's symbolic code a message opens with is the record's code,
/// with the kind's numeric one as `parent_code`, and the `, ..` edit
/// `karac fix` applies is the record's replacement.
#[test]
fn a_package_check_names_the_cross_package_pattern_code_and_carries_its_fix() {
    let dir = package(
        "xpkg",
        &[
            (
                "lib/kara.toml",
                "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
            ),
            (
                "lib/src/lib.kara",
                "#[non_exhaustive]\npub struct Opts { pub port: i64 }\n\npub fn mk() -> Opts { Opts { port: 3 } }\n",
            ),
            (
                "app/kara.toml",
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nlib = { path = \"../lib\" }\n",
            ),
            (
                "app/src/main.kara",
                "import lib.{Opts, mk};\n\nfn main() {\n    let Opts { port } = mk();\n    println(port);\n}\n",
            ),
        ],
    );
    let app = dir.join("app");
    let (_, out, _) = karac(&app, &["check", "--output=json"]);
    let d = diagnostics(&out)
        .into_iter()
        .find(|d| d["code"] == "E_NON_EXHAUSTIVE_CROSS_PACKAGE_PATTERN")
        .unwrap_or_else(|| panic!("{out}"));
    assert_eq!(d["parent_code"], "E0243", "{out}");
    assert!(
        !d["message"].as_str().unwrap().starts_with("error["),
        "{out}"
    );
    assert_eq!(d["replacement"]["text"], ", ..", "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The import cycle's suggestion, which the text render prints, is the
/// JSON record's hint.
#[test]
fn an_import_cycles_json_record_carries_the_suggestion_the_text_prints() {
    let dir = package(
        "cycle",
        &[
            ("kara.toml", MANIFEST),
            (
                "src/main.kara",
                "import a.{f};\nfn main() {\n    println(f());\n}\n",
            ),
            ("src/a.kara", "import b.{g};\npub fn f() -> i64 { g() }\n"),
            ("src/b.kara", "import a.{f};\npub fn g() -> i64 { f() }\n"),
        ],
    );
    let (_, out, _) = karac(&dir, &["check", "--output=json"]);
    let d = diagnostics(&out)
        .into_iter()
        .find(|d| d["code"] == "E0223")
        .unwrap_or_else(|| panic!("{out}"));
    assert!(
        d["hints"][0]["description"]
            .as_str()
            .unwrap()
            .contains("extract the shared items into a lower-layer module"),
        "{out}"
    );
    let (_, _, text) = karac(&dir, &["check"]);
    assert!(text.contains("extract the shared items"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_toolchain_mismatchs_json_record_carries_its_note_and_help() {
    let dir = package(
        "toolchain",
        &[
            ("kara.toml", MANIFEST),
            ("karac-toolchain.toml", "version = \"=0.0.1\"\n"),
            ("src/main.kara", "fn main() {\n    println(1);\n}\n"),
        ],
    );
    let (_, out, _) = karac(&dir, &["check", "--output=json"]);
    let d = &diagnostics(&out)[0];
    assert_eq!(d["code"], "E_TOOLCHAIN_VERSION_MISMATCH", "{out}");
    assert!(
        d["notes"][0].as_str().unwrap().contains("karaup install"),
        "{out}"
    );
    assert!(
        d["help"].as_str().unwrap().contains("relax the `version`"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_offline_resolve_with_no_vendor_directory_carries_its_note_and_help() {
    let dir = package(
        "offline",
        &[
            (
                "kara.toml",
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nlib = { path = \"lib\" }\n",
            ),
            (
                "lib/kara.toml",
                "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
            ),
            ("lib/src/lib.kara", "pub fn one() -> i64 { 1 }\n"),
            ("src/main.kara", "fn main() {\n    println(1);\n}\n"),
        ],
    );
    let (ok, out, err) = karac(&dir, &["resolve", "--offline", "--output=json"]);
    assert!(!ok, "{out}{err}");
    let d = &diagnostics(&out)[0];
    assert_eq!(d["code"], "E_OFFLINE_NO_VENDOR_DIR", "{out}");
    assert!(
        d["notes"][0].as_str().unwrap().contains("./vendor/<name>/"),
        "{out}"
    );
    assert!(
        d["help"].as_str().unwrap().contains("karac vendor"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A lockfile that cannot be written says so: it used to say "failed to
/// read lockfile", and its JSON dropped the note and help.
#[test]
fn a_lockfile_write_failure_says_write_and_carries_its_note_and_help() {
    let dir = package(
        "lockdir",
        &[
            ("kara.toml", MANIFEST),
            ("src/main.kara", "fn main() {\n    println(1);\n}\n"),
        ],
    );
    std::fs::create_dir_all(dir.join("kara.lock")).unwrap();
    let (_, out, _) = karac(&dir, &["update", "--output=json"]);
    let first = out.lines().next().unwrap_or("");
    let d = &diagnostics(first)[0];
    assert_eq!(d["code"], "E_LOCKFILE_IO", "{out}");
    assert!(
        d["message"]
            .as_str()
            .unwrap()
            .starts_with("failed to write lockfile"),
        "{out}"
    );
    assert!(d["notes"][0].as_str().unwrap().contains("write"), "{out}");
    assert!(d["help"].is_string(), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The compiler inserts no reference counts (core-semantics.md §6.4), so a
/// `#![rc_budget(max: 0)]` is never exceeded: the program has the move
/// error, and E_RC_BUDGET_EXCEEDED, which `karac explain` lists as
/// retired, does not fire beside it.
#[test]
fn an_rc_budget_is_never_exceeded_and_the_move_error_stands_alone() {
    let dir = package(
        "rcbudget",
        &[(
            "main.kara",
            "#![rc_budget(max: 0)]\n\nfn main() {\n    let s = \"hello\".to_string();\n    let f = || s.len();\n    let t = s;\n    println(f());\n    println(t)\n}\n",
        )],
    );
    let (_, out, _) = karac(&dir, &["check", "--output=json", "main.kara"]);
    assert_eq!(codes(&out), ["E0500"], "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A panic aborts in every build (core-semantics.md §10), so a
/// `"C-unwind"` export is refused as E0415 under `panic = "unwind"` too,
/// in `check` and `run` alike; E0413, retired, does not fire.
#[test]
fn a_c_unwind_export_under_an_unwind_profile_is_e0415_in_check_and_run() {
    let dir = package(
        "cunwind",
        &[
            (
                "kara.toml",
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[profile]\npanic = \"unwind\"\n",
            ),
            (
                "src/main.kara",
                "pub extern \"C-unwind\" fn boom(x: i64) -> i64 {\n    if x > 0 {\n        panic(\"no\");\n    }\n    x\n}\n\nfn main() {\n    println(boom(0));\n}\n",
            ),
        ],
    );
    for cmd in ["check", "run"] {
        let (_, out, _) = karac(&dir, &[cmd, "--output=json"]);
        let codes = codes(&out);
        assert!(codes.iter().any(|c| c == "E0415"), "{cmd}: {out}");
        assert!(!codes.iter().any(|c| c == "E0413"), "{cmd}: {out}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
