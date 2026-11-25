//! B-2026-11-25-53 — `karac check <file>` inside a package, and the editor's
//! check of a package member (`karac::check_source_at`, what kara-lsp runs),
//! judge the file as `karac check` with no file judges the package: by the
//! v2 check of the combined source once the module boundaries hold, with
//! that check's diagnostics in the named file reported and the rest counted.
//! Before, they ran the per-module typecheck alone, which refused a
//! re-export as a different type, gave `a.make()` the return type of
//! `b.make()` and missed an impl no file imports. Also: an unassemblable
//! package is refused with the package's error, not an internal E0516, a
//! test file's error fails the check of another file, and B-2026-11-25-60:
//! that refusal's `--output=json` is the diagnostics document, coded.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn package(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-filepkg-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("kara.toml"),
        format!("[package]\nname = \"{tag}\"\nversion = \"0.1.0\"\n"),
    )
    .unwrap();
    for (p, src) in files {
        let f = d.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, src).unwrap();
    }
    d
}

fn karac(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)
}

const REEXPORT: &[(&str, &str)] = &[
    ("src/api.kara", "pub import db.conn.Connection;\npub import db.conn.connect;\n"),
    (
        "src/main.kara",
        "import api.{Connection, connect};\nimport db.conn.Connection as Raw;\nimport db.conn;\nfn take(c: Raw) -> i64 { c.id }\nfn main() { let c: Connection = connect(5); println(take(c)); let d: Raw = conn.Connection { id: 9 }; let e: Connection = d; println(e.id); }\n",
    ),
    (
        "src/db/conn.kara",
        "pub struct Connection { pub id: i64 }\npub fn connect(id: i64) -> Connection { Connection { id: id } }\n",
    ),
];

const TWO_MAKES_MAIN: &str = "import a;\nimport b;\nfn main() {\n    println(a.make().v + 10);\n    println(b.make().v);\n}\n";
const TWO_MAKES: &[(&str, &str)] = &[
    ("src/a.kara", "pub struct Item { pub v: i64 }\npub fn make() -> Item { Item { v: 2 } }\n"),
    ("src/b.kara", "pub struct Item { pub v: String }\npub fn make() -> Item { Item { v: \"b\".to_string() } }\n"),
    ("src/main.kara", TWO_MAKES_MAIN),
];

const IMPL_ELSEWHERE: &[(&str, &str)] = &[
    ("src/traits.kara", "pub trait Area { fn area(ref self) -> i64; }\n"),
    ("src/shapes.kara", "pub struct Sq { pub s: i64 }\n"),
    (
        "src/main.kara",
        "import shapes.Sq;\nimport traits.Area;\nfn main() { let v = Sq { s: 3 }; println(v.area()); }\n",
    ),
    (
        "src/impls/sq_area.kara",
        "import traits.Area;\nimport shapes.Sq;\nimpl Area for Sq { fn area(ref self) -> i64 { self.s * self.s } }\n",
    ),
];

#[test]
fn a_member_checks_as_its_package_does() {
    for (tag, files) in [
        ("reexport", REEXPORT),
        ("two_makes", TWO_MAKES),
        ("impl_elsewhere", IMPL_ELSEWHERE),
    ] {
        let d = package(tag, files);
        let pkg = karac(&d, &["check"]);
        assert!(pkg.status.success(), "{tag} package: {}", text(&pkg));
        for (p, _) in files {
            let out = karac(&d, &["check", p]);
            assert!(out.status.success(), "{tag} {p}: {}", text(&out));
            assert!(!text(&out).contains("__"), "{tag} {p}: {}", text(&out));
            let json = karac(&d, &["check", "--output=json", p]);
            assert!(json.status.success(), "{tag} {p} json: {}", text(&json));
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[test]
fn a_members_own_error_is_shown_and_another_files_is_counted() {
    let main = TWO_MAKES_MAIN.replace("a.make().v + 10", "a.make().v + \"x\"");
    let files: Vec<(&str, &str)> = vec![TWO_MAKES[0], TWO_MAKES[1], ("src/main.kara", &main)];
    let d = package("own_error", &files);
    let out = karac(&d, &["check", "src/main.kara"]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "{t}");
    assert!(t.contains("src/main.kara:4:"), "{t}");
    assert!(t.contains("1 error(s) found."), "{t}");
    let out = karac(&d, &["check", "src/a.kara"]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "{t}");
    assert!(t.contains("1 further error(s) in other modules"), "{t}");
    assert!(!t.contains("error["), "{t}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_editor_checks_a_member_as_its_package_does() {
    let d = package("editor", TWO_MAKES);
    let path = d.join("src/main.kara");
    let clean = karac::check_source_at(TWO_MAKES_MAIN, &path);
    assert!(
        clean.iter().all(|x| x.severity != "error"),
        "{:?}",
        clean.iter().map(|x| &x.message).collect::<Vec<_>>()
    );
    // The buffer, not the disk: an error typed in the editor is found, at
    // its place in the file.
    let buffer = TWO_MAKES_MAIN.replace("a.make().v + 10", "a.make().v + \"x\"");
    let found = karac::check_source_at(&buffer, &path);
    let errors: Vec<_> = found.iter().filter(|x| x.severity == "error").collect();
    assert_eq!(
        errors.len(),
        1,
        "{:?}",
        found.iter().map(|x| &x.message).collect::<Vec<_>>()
    );
    assert_eq!(errors[0].line, 4, "{}", errors[0].message);
    assert_eq!(
        &buffer[errors[0].offset..errors[0].offset + 3],
        "\"x\"",
        "{}",
        errors[0].message
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_test_files_error_fails_the_check_of_another_file() {
    let d = package(
        "test_private",
        &[
            ("src/main.kara", "fn main() { println(1); }\n"),
            ("src/calc.kara", "private fn secret() -> i64 { 41 }\n"),
            (
                "src/deep/more_test.kara",
                "import calc.secret;\ntest \"no\" { assert_eq(secret(), 41); }\n",
            ),
        ],
    );
    assert!(!karac(&d, &["check"]).status.success());
    let out = karac(&d, &["check", "src/main.kara"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(text(&out).contains("further error(s)"), "{}", text(&out));
    let json = karac(&d, &["check", "--output=json", "src/main.kara"]);
    assert_eq!(json.status.code(), Some(1), "{}", text(&json));
    assert!(text(&json).contains("E0111"), "{}", text(&json));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn an_unassemblable_package_is_refused_with_its_own_error() {
    let d = package(
        "mod_kara",
        &[
            (
                "src/main.kara",
                "import db;\nfn main() { println(db.f()); }\n",
            ),
            ("src/db/mod.kara", "pub fn f() -> i64 { 1 }\n"),
        ],
    );
    let out = karac(&d, &["check", "src/main.kara"]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "{t}");
    assert!(t.contains("is not a recognized filename"), "{t}");
    assert!(!t.contains("E0516"), "{t}");
    // B-2026-11-25-60: the JSON document, its diagnostic coded.
    for args in [
        &["check", "--output=json"][..],
        &["check", "--output=json", "src/main.kara"],
    ] {
        let out = karac(&d, args);
        let v: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: {e}: {}", text(&out)));
        let diags = v["diagnostics"].as_array().expect("diagnostics");
        assert_eq!(diags.len(), 1, "{v}");
        assert_eq!(diags[0]["severity"], "error", "{v}");
        assert!(diags[0]["code"].is_string(), "{v}");
        assert!(diags[0]["message"]
            .as_str()
            .unwrap()
            .contains("not a recognized filename"));
    }
    let _ = std::fs::remove_dir_all(&d);
}
