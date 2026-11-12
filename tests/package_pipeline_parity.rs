//! A package's modules go through the same pre-resolve rewrites and
//! resolver settings a lone file does. Its per-module resolve and type
//! checks run on each module's own items, before the flattened program is
//! desugared, and they crashed on a parallel assignment (B-2026-11-05-19)
//! and refused a user operator-trait impl (B-2026-11-05-21) that a lone
//! file runs. Each desugared form is run here both ways.

use std::path::{Path, PathBuf};
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-package-parity-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn output(cmd: &mut Command) -> (bool, String) {
    let out = cmd.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

/// A package at `dir` with the given `src/` files.
fn package(dir: &Path, files: &[(&str, &str)]) {
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"app\"\n").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    for (name, text) in files {
        std::fs::write(dir.join("src").join(name), text).unwrap();
    }
}

/// Parallel assignment to a binding, an element and a field; a struct
/// spread; a user `Add` and `Index[(i64, i64)]`; a trait default method;
/// an `impl Trait` argument; a named parameter's default.
const FORMS: &str = r#"struct P { x: i64, y: i64 }

struct Grid { w: i64, cells: Vec[i64] }

impl Index[(i64, i64)] for Grid {
    type Output = i64;
    fn index(ref self, at: (i64, i64)) -> ref i64 {
        let (r, c) = at;
        self.cells[r * self.w + c]
    }
}

impl Add for P {
    type Output = P;
    fn add(self, o: P) -> P { P { x: self.x + o.x, y: self.y + o.y } }
}

trait Named {
    fn name(ref self) -> String;
    fn greet(ref self) -> String { f"hi {self.name()}" }
}

impl Named for P {
    fn name(ref self) -> String { f"p{self.x}" }
}

fn total(xs: own impl Iterator[Item = i64]) -> i64 {
    let mut s = 0;
    for x in xs { s += x; }
    s
}

fn scaled(n: i64; by: i64 = 10) -> i64 { n * by }

fn twice(n: i64) -> i64 { n * 2 }

fn main() {
    let mut a = 1;
    let mut b = 2;
    a, b = b, a;
    let mut v = [1, 2, 3];
    v[0], v[2] = v[2], v[0];
    let mut p = P { x: 1, y: 2 };
    p.x, p.y = p.y, p.x;
    println(f"{a} {b} {v} {p.x} {p.y}");
    let q = P { x: 7, ..p };
    let r = q + p;
    println(f"{q.x} {q.y} {r.x} {r.y} {r.greet()}");
    let g = Grid { w: 2, cells: [1, 2, 3, 4] };
    println(g[(1, 0)]);
    println(total([1, 2, 3].into_iter()));
    println(f"{scaled(2)} {scaled(2, by: 3)}");
    println(twice(5));
}
"#;

const EXPECTED: &str = "2 1 [3, 2, 1] 2 1\n7 1 9 2 hi p9\n3\n6\n20 6\n10\n";

#[test]
fn every_desugared_form_runs_alike_in_a_package_and_a_lone_file() {
    let lone = scratch("lone");
    std::fs::write(lone.join("main.kara"), FORMS).unwrap();
    let (lone_ok, lone_out) = output(karac().arg("run").arg("main.kara").current_dir(&lone));
    let pkg = scratch("pkg");
    package(&pkg, &[("main.kara", FORMS)]);
    let (check_ok, check_out) = output(karac().arg("check").current_dir(&pkg));
    let (pkg_ok, pkg_out) = output(karac().arg("run").current_dir(&pkg));
    let _ = std::fs::remove_dir_all(&lone);
    let _ = std::fs::remove_dir_all(&pkg);
    assert!(lone_ok, "{lone_out}");
    assert_eq!(lone_out, EXPECTED);
    assert!(check_ok, "{check_out}");
    assert!(pkg_ok, "{pkg_out}");
    assert_eq!(pkg_out, EXPECTED);
}

#[test]
fn a_package_module_swaps_and_adds_for_its_importer() {
    let dir = scratch("modules");
    package(
        &dir,
        &[
            (
                "main.kara",
                "import vec2.{V2, swapped};\n\nfn main() {\n    let c = swapped(V2 { x: 1, y: 2 }) + V2 { x: 10, y: 20 };\n    println(f\"{c.x} {c.y}\");\n}\n",
            ),
            (
                "vec2.kara",
                "pub struct V2 { pub x: i64, pub y: i64 }\n\nimpl Add for V2 {\n    type Output = V2;\n    fn add(self, o: V2) -> V2 { V2 { x: self.x + o.x, y: self.y + o.y } }\n}\n\npub fn swapped(v: V2) -> V2 {\n    let mut a = v.x;\n    let mut b = v.y;\n    a, b = b, a;\n    V2 { x: a, y: b }\n}\n",
            ),
            (
                "vec2_test.kara",
                "test \"swap\" {\n    let mut a = 1;\n    let mut b = 2;\n    a, b = b, a;\n    assert(a == 2 and b == 1);\n}\n",
            ),
        ],
    );
    let (ok, out) = output(karac().arg("run").current_dir(&dir));
    let (test_ok, test_out) = output(karac().arg("test").current_dir(&dir));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "12 21", "{out}");
    assert!(test_ok && test_out.contains("\"passed\":1"), "{test_out}");
}

#[test]
fn a_clean_package_check_warns_of_another_modules_deprecated_item() {
    let dir = scratch("deprecated");
    package(
        &dir,
        &[
            ("util.kara", "#[deprecated(note: \"x\")]\npub fn old() -> i64 { 1 }\n"),
            (
                "main.kara",
                "#[deprecated(note: \"y\")]\nfn old2() -> i64 { 2 }\n\nimport util.{old};\n\nfn main() {\n    println(old() + old2());\n}\n",
            ),
        ],
    );
    let (ok, out) = output(karac().arg("check").current_dir(&dir));
    let (json_ok, json) = output(karac().args(["check", "--output=json"]).current_dir(&dir));
    let (_, build) = output(karac().arg("build").current_dir(&dir));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok && json_ok, "{out}\n{json}");
    for text in [&out, &build] {
        assert_eq!(text.matches("deprecated item `old`").count(), 1, "{text}");
        assert_eq!(text.matches("deprecated item `old2`").count(), 1, "{text}");
    }
    assert_eq!(
        json.matches("\"lint_name\":\"deprecated\"").count(),
        2,
        "{json}"
    );
}

/// Through a module binding the combined text blanks the qualifier, and the
/// warning's column read the blank as a word: `util.old()` was warned at the
/// `)` (B-2026-11-05-24).
#[test]
fn a_deprecated_use_through_a_module_binding_points_at_the_name() {
    let dir = scratch("deprecated-binding");
    package(
        &dir,
        &[
            (
                "util.kara",
                "#[deprecated(note: \"x\")]\npub fn old() -> i64 { 1 }\n",
            ),
            (
                "main.kara",
                "import util;\n\nfn main() {\n    println(util.old() + util.old());\n}\n",
            ),
        ],
    );
    let (ok, out) = output(karac().arg("check").current_dir(&dir));
    let (_, json) = output(karac().args(["check", "--output=json"]).current_dir(&dir));
    let (_, build) = output(karac().arg("build").current_dir(&dir));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{out}");
    let carets = "    println(util.old() + util.old());\n  |                  ^^^";
    let second = "    println(util.old() + util.old());\n  |                               ^^^";
    for text in [&out, &build] {
        // The host's separator: `src\main.kara` on Windows.
        let slashed = text.replace('\\', "/");
        assert!(slashed.contains("src/main.kara:4:18:"), "{text}");
        assert!(slashed.contains("src/main.kara:4:31:"), "{text}");
        assert!(text.contains(carets) && text.contains(second), "{text}");
    }
    assert!(
        json.contains("\"column\":18") && json.contains("\"column\":31"),
        "{json}"
    );
}
