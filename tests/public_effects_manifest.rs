//! `[package].public_effects` (design.md § Inference and declarations). The
//! key was documented and the effect checker had the policy, but no manifest
//! key reached it: every package was checked as `declared`, and
//! `public_effects = "inferred"` drew an unknown-key notice.
//!
//! Under `inferred` a public function owes no declaration, its callers are
//! charged what it performs (they were charged nothing: the checker took a
//! public function with no clause for a pure one), `karac build` shows each
//! public function's set, and each package of a program is held to its own
//! manifest. A package that imports a dependency on `inferred` is told so
//! (`inferred_dependency_effects`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A scratch directory, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("karac-public-effects-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// A package `name` under the scratch directory: its manifest with
    /// `extra` after the `[package]` keys, and one source file.
    fn package(&self, name: &str, extra: &str, file: &str, src: &str) -> PathBuf {
        let dir = self.0.join(name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("kara.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n{extra}"),
        )
        .unwrap();
        std::fs::write(dir.join("src").join(file), src).unwrap();
        dir
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn karac(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .env("KARAC_HASH_SEED", "1")
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

/// `karac check --output=json`'s document for the package at `dir`.
fn check_json(dir: &Path) -> serde_json::Value {
    let out = karac(dir, &["check", "--output=json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&out)))
}

/// The strings of the array at `v`, sorted.
fn sorted(v: &serde_json::Value) -> Vec<String> {
    let mut out: Vec<String> = v
        .as_array()
        .unwrap_or_else(|| panic!("not an array: {v}"))
        .iter()
        .map(|e| e.as_str().unwrap().to_string())
        .collect();
    out.sort();
    out
}

const INFERRED: &str = "public_effects = \"inferred\"\n";

/// One public function with no clause that writes to stderr, one public
/// method with no clause that does, and a `main` that only calls them.
const UNDECLARED: &str =
    "struct Meter { n: i64 }\n\nimpl Meter {\n    pub fn show(self) {\n        \
                          eprintln(f\"{self.n}\");\n    }\n}\n\npub fn warn(msg: String) {\n    \
                          eprintln(msg);\n}\n\nfn main() {\n    let m = Meter { n: 1 };\n    \
                          m.show();\n    warn(\"w\".to_string());\n}\n";

#[test]
fn declared_is_the_default_and_the_key_takes_two_values() {
    let s = Scratch::new("values");
    // No key, and the key spelled out: the missing declaration is an error.
    for (name, extra) in [
        ("plain", ""),
        ("spelled", "public_effects = \"declared\"\n"),
    ] {
        let dir = s.package(name, extra, "main.kara", UNDECLARED);
        let out = karac(&dir, &["check"]);
        let all = text(&out);
        assert!(!out.status.success(), "{name}: {all}");
        assert!(all.contains("error[E0400]"), "{name}: {all}");
        assert!(all.contains("public function 'warn'"), "{name}: {all}");
        assert!(!all.contains("warning[manifest]"), "{name}: {all}");
    }
    // Another value is the manifest's error, not a silent `declared`.
    let dir = s.package(
        "other",
        "public_effects = \"sometimes\"\n",
        "main.kara",
        UNDECLARED,
    );
    let out = karac(&dir, &["check"]);
    let all = text(&out);
    assert!(!out.status.success(), "{all}");
    assert!(
        all.contains("`[package].public_effects` must be \"declared\" or \"inferred\""),
        "{all}"
    );
}

#[test]
fn inferred_owes_no_declaration_and_charges_callers_what_the_function_performs() {
    let s = Scratch::new("inferred");
    let dir = s.package("app", INFERRED, "main.kara", UNDECLARED);
    let out = karac(&dir, &["check"]);
    let all = text(&out);
    assert!(out.status.success(), "{all}");
    assert!(!all.contains("warning[manifest]"), "{all}");
    let v = check_json(&dir);
    assert_eq!(v["diagnostics"].as_array().map(Vec::len), Some(0), "{v}");
    // `main` performs nothing itself: every effect of the program comes
    // through the two public items.
    let program = sorted(&v["program_effects"]);
    assert!(program.iter().any(|e| e == "writes(Stderr)"), "{program:?}");
    let public = &v["public_function_effects"];
    assert_eq!(sorted(&public["warn"]), ["writes(Stderr)"], "{v}");
    assert!(
        sorted(&public["Meter.show"])
            .iter()
            .any(|e| e == "writes(Stderr)"),
        "{v}"
    );
    // The same program with the two items private reports the same effects.
    let private = UNDECLARED.replace("pub fn", "fn");
    let twin = s.package("twin", "", "main.kara", &private);
    assert_eq!(sorted(&check_json(&twin)["program_effects"]), program);
}

#[test]
fn inferred_keeps_no_effect_trait_ceilings_and_written_clauses() {
    let s = Scratch::new("kept");
    // `#[no_effect]` reads the inferred set, through a public callee too.
    let dir = s.package(
        "quiet",
        INFERRED,
        "main.kara",
        "pub fn warn(msg: String) {\n    eprintln(msg);\n}\n\n#[no_effect(writes(Stderr))]\npub \
         fn quiet() {\n    warn(\"w\".to_string());\n}\n\nfn main() {\n    quiet();\n}\n",
    );
    let all = text(&karac(&dir, &["check"]));
    assert!(all.contains("E_NO_EFFECT_VIOLATED"), "{all}");
    assert!(all.contains("fn `quiet`"), "{all}");
    // A trait method's clause is its impls' ceiling under either setting.
    let dir = s.package(
        "ceiling",
        INFERRED,
        "main.kara",
        "pub trait Report {\n    fn report(self) with writes(Stdout);\n}\n\nstruct Loud { n: i64 \
         }\n\nimpl Report for Loud {\n    fn report(self) {\n        eprintln(f\"{self.n}\");\n    \
         }\n}\n\nfn main() {\n    let l = Loud { n: 1 };\n    l.report();\n}\n",
    );
    let out = karac(&dir, &["check"]);
    let all = text(&out);
    assert!(!out.status.success(), "{all}");
    assert!(all.contains("E0230"), "{all}");
    // A clause a public function does write is checked against its body.
    let dir = s.package(
        "written",
        INFERRED,
        "main.kara",
        "pub fn warn(msg: String) with writes(Stdout) {\n    eprintln(msg);\n}\n\nfn main() {\n    \
         warn(\"w\".to_string());\n}\n",
    );
    let out = karac(&dir, &["check"]);
    let all = text(&out);
    assert!(!out.status.success(), "{all}");
    assert!(all.contains("writes(Stderr)"), "{all}");
}

/// `app` depends on `util` and `strict` by path, with `app_extra` in its
/// `[package]` table; `util` infers its public effects.
fn three_packages(s: &Scratch, app_extra: &str, strict_src: &str) -> PathBuf {
    s.package(
        "util",
        INFERRED,
        "lib.kara",
        "pub fn shout(msg: String) {\n    println(msg);\n}\n",
    );
    s.package("strict", "", "lib.kara", strict_src);
    let app = s.package(
        "app",
        app_extra,
        "main.kara",
        "import strict.{say};\nimport util.{shout};\n\nfn main() {\n    shout(\"a\".to_string());\n    \
         say(\"b\".to_string());\n}\n",
    );
    let manifest = app.join("kara.toml");
    let mut toml = std::fs::read_to_string(&manifest).unwrap();
    toml.push_str(
        "\n[dependencies]\nutil = { path = \"../util\" }\nstrict = { path = \"../strict\" }\n",
    );
    std::fs::write(&manifest, toml).unwrap();
    app
}

const STRICT_DECLARED: &str =
    "pub fn say(msg: String) with writes(Stdout) {\n    println(msg);\n}\n";
const STRICT_UNDECLARED: &str = "pub fn say(msg: String) {\n    println(msg);\n}\n";

#[test]
fn each_package_is_held_to_its_own_setting() {
    // A declared package over an inferred one: the inferred dependency owes
    // no clause, and the dependent is told what it relies on.
    let s = Scratch::new("own-declared");
    let app = three_packages(&s, "", STRICT_DECLARED);
    let out = karac(&app, &["check"]);
    let all = text(&out);
    assert!(out.status.success(), "{all}");
    assert!(!all.contains("E0400"), "{all}");
    assert_eq!(
        all.matches("warning[inferred_dependency_effects]").count(),
        1,
        "{all}"
    );
    assert!(
        all.contains("src/main.kara:2:8: package `util` sets `public_effects = \"inferred\"`"),
        "{all}"
    );
    let v = check_json(&app);
    let diagnostics = v["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{v}");
    let d = &diagnostics[0];
    assert_eq!(d["severity"], "warning", "{v}");
    assert_eq!(d["lint_name"], "inferred_dependency_effects", "{v}");
    assert_eq!(d["file"], "src/main.kara", "{v}");
    assert_eq!(
        (d["line"].as_u64(), d["column"].as_u64()),
        (Some(2), Some(8))
    );
    assert!(
        sorted(&v["program_effects"])
            .iter()
            .any(|e| e == "writes(Stdout)"),
        "{v}"
    );
    // The lint takes a level like any other.
    let out = karac(&app, &["check", "-A", "inferred_dependency_effects"]);
    let all = text(&out);
    assert!(out.status.success(), "{all}");
    assert!(!all.contains("inferred_dependency_effects"), "{all}");
    let out = karac(&app, &["check", "-D", "inferred_dependency_effects"]);
    assert!(!out.status.success(), "{}", text(&out));

    // An inferred package over a declared one that left a clause out: the
    // dependency's own setting still asks for it, in its own file.
    let s = Scratch::new("own-inferred");
    let app = three_packages(&s, INFERRED, STRICT_UNDECLARED);
    let out = karac(&app, &["check"]);
    let all = text(&out);
    assert!(!out.status.success(), "{all}");
    assert_eq!(all.matches("error[E0400]").count(), 1, "{all}");
    assert!(all.contains("public function 'say'"), "{all}");
    let at = all.find("error[E0400]").unwrap();
    let line = all[at..].lines().next().unwrap();
    assert!(
        line.contains("strict") && line.contains("lib.kara:1:"),
        "{line}"
    );
}

#[test]
fn build_shows_each_public_functions_inferred_set_before_built() {
    let s = Scratch::new("build");
    let src = "pub fn warn(msg: String) {\n    eprintln(msg);\n}\n\npub fn twice(n: i64) -> i64 \
               {\n    n + n\n}\n\npub fn id(n: i64) -> i64 {\n    n\n}\n\nfn main() {\n    \
               warn(\"w\".to_string());\n    println(id(twice(2)));\n}\n";
    let block = "Public effects (inferred):\n  id: none\n  twice: panics\n  warn: writes(Stderr)\n";
    let dir = s.package("app", INFERRED, "main.kara", src);
    let out = karac(&dir, &["build"]);
    // A checkout with no runtime archive cannot link: nothing to read.
    if cfg!(feature = "llvm") && !out.status.success() && text(&out).contains("libkarac_runtime") {
        return;
    }
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(stdout.contains(block), "{}", text(&out));
    // With a code generator the build ends with the program's `Effects:`
    // line (B-2026-11-22-39) and then `Built:`, and the block comes
    // directly before the two (review-thread ruling 2026-11-24).
    if cfg!(feature = "llvm") && out.status.success() {
        let rest = &stdout[stdout.find(block).unwrap() + block.len()..];
        let lines: Vec<&str> = rest.lines().collect();
        assert_eq!(lines.len(), 2, "{stdout}");
        assert!(lines[0].starts_with("Effects: "), "{stdout}");
        assert!(lines[1].starts_with("Built: "), "{stdout}");
    }
    // The JSON document carries the same sets and no block.
    let out = karac(&dir, &["build", "--output=json"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(!stdout.contains("Public effects"), "{stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("{e}: {}", text(&out));
    });
    assert_eq!(
        sorted(&v["public_function_effects"]["warn"]),
        ["writes(Stderr)"]
    );
    assert_eq!(sorted(&v["public_function_effects"]["twice"]), ["panics"]);
    // A declared package prints no block.
    let declared = src.replace(
        "pub fn warn(msg: String) {",
        "pub fn warn(msg: String) with writes(Stderr) {",
    );
    let dir = s.package("plain", "", "main.kara", &declared);
    let out = karac(&dir, &["build"]);
    assert!(!text(&out).contains("Public effects"), "{}", text(&out));
}

#[test]
fn the_editor_and_doc_read_the_packages_setting() {
    let s = Scratch::new("editor");
    let dir = s.package("app", INFERRED, "main.kara", UNDECLARED);
    let path = dir.join("src/main.kara");
    let found = karac::check_source_at(UNDECLARED, &path);
    assert!(
        found.iter().all(|d| d.severity != "error"),
        "{:?}",
        found.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    // The same buffer in a declared package is refused there too.
    let dir = s.package("plain", "", "main.kara", UNDECLARED);
    let found = karac::check_source_at(UNDECLARED, &dir.join("src/main.kara"));
    assert!(
        found
            .iter()
            .any(|d| d.severity == "error" && d.message.contains("no effect declaration")),
        "{:?}",
        found.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    // `karac doc` shows the inferred set beside the signature.
    let lib = s.package(
        "util",
        INFERRED,
        "lib.kara",
        "/// Says it.\npub fn shout(msg: String) {\n    println(msg);\n}\n",
    );
    let out = karac(&lib, &["doc"]);
    assert!(out.status.success(), "{}", text(&out));
    let page = std::fs::read_to_string(lib.join("dist/doc/shout.html")).unwrap();
    assert!(
        page.contains("fn shout(msg: String) with writes(Stdout)"),
        "{page}"
    );
}
