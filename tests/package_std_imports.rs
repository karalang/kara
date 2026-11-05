//! A package's modules import the library exactly as a lone file does
//! (B-2026-11-05-7). A library module whose items are globals (`std.sync`,
//! `std.net`, `std.json`, ...) has no node in a package's module graph, and
//! its import was refused there as "unknown module" while a lone file took
//! it. Every importable `std` module is checked both ways here, so a module
//! added to either list is covered without editing this file.

use std::path::{Path, PathBuf};
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-package-std-imports-{tag}-{}-{}",
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

/// A package at `dir` whose `src/main.kara` is `src`.
fn package(dir: &Path, src: &str) {
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"app\"\n").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.kara"), src).unwrap();
}

/// The diagnostics' messages, without the file name each one carries.
fn messages(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.starts_with("error"))
        .map(|l| match l.find("main.kara:") {
            Some(i) => l[i + "main.kara:".len()..].to_string(),
            None => l.to_string(),
        })
        .collect()
}

/// Each importable `std` module and one item it exports.
fn every_std_module() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (path, have, _) in karac::prelude::LIBRARY_GLOBAL_MODULES {
        if let Some(item) = have.first() {
            out.push((path.join("."), (*item).to_string()));
        }
    }
    for (path, _) in karac::prelude::GATED_STDLIB_SOURCES {
        let segs: Vec<String> = path.iter().map(|s| s.to_string()).collect();
        let span = karac::token::Span::default();
        let Some(items) = karac::prelude::gated_module_public_items(&segs, span) else {
            continue;
        };
        if let Some(item) = items.first() {
            let m = path.join(".");
            if !out.iter().any(|(have, _)| *have == m) {
                out.push((m, item.name.clone()));
            }
        }
    }
    out
}

#[test]
fn a_package_imports_every_std_module_a_lone_file_does() {
    let modules = every_std_module();
    for m in ["std.sync", "std.net", "std.json", "std.time", "std.path"] {
        assert!(
            modules.iter().any(|(have, _)| have == m),
            "{m} is missing from the module lists"
        );
    }
    let mut failures = Vec::new();
    for (module, item) in &modules {
        let src = format!("import {module}.{{{item}}};\n\nfn main() {{ }}\n");
        let lone = scratch("lone");
        std::fs::write(lone.join("main.kara"), &src).unwrap();
        let (lone_ok, lone_out) = output(karac().arg("check").arg("main.kara").current_dir(&lone));
        let pkg = scratch("pkg");
        package(&pkg, &src);
        let (pkg_ok, pkg_out) = output(karac().arg("check").current_dir(&pkg));
        let _ = std::fs::remove_dir_all(&lone);
        let _ = std::fs::remove_dir_all(&pkg);
        if pkg_out.contains("unknown module") {
            failures.push(format!("{module}.{item}: package refused it: {pkg_out}"));
        } else if pkg_ok != lone_ok || messages(&pkg_out) != messages(&lone_out) {
            failures.push(format!(
                "{module}.{item}: package ({pkg_ok}) {pkg_out}\nlone file ({lone_ok}) {lone_out}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} modules differ:\n{}",
        failures.len(),
        modules.len(),
        failures.join("\n")
    );
}

#[test]
fn a_package_refuses_a_bad_library_import_as_a_lone_file_does() {
    for src in [
        "import std.sync.{Atomikk};\n\nfn main() { }\n",
        "import std.sync;\n\nfn main() { }\n",
        "import std.json.{Json as J};\n\nfn main() { }\n",
    ] {
        let lone = scratch("bad-lone");
        std::fs::write(lone.join("main.kara"), src).unwrap();
        let (lone_ok, lone_out) = output(karac().arg("check").arg("main.kara").current_dir(&lone));
        let pkg = scratch("bad-pkg");
        package(&pkg, src);
        let (pkg_ok, pkg_out) = output(karac().arg("check").current_dir(&pkg));
        let _ = std::fs::remove_dir_all(&lone);
        let _ = std::fs::remove_dir_all(&pkg);
        assert!(
            !lone_ok && !pkg_ok,
            "{src}: both must refuse: {lone_out} / {pkg_out}"
        );
        assert_eq!(messages(&pkg_out), messages(&lone_out), "{src}");
    }
}

#[test]
fn a_package_module_runs_with_sync_and_json() {
    let dir = scratch("run");
    package(
        &dir,
        "import counter.{bump};\n\nfn main() {\n    println(bump());\n}\n",
    );
    std::fs::write(
        dir.join("src/counter.kara"),
        "import std.json.{Json};\nimport std.sync.{Atomic, Mutex};\n\n\
         pub fn bump() -> String {\n    let a = Atomic.new(4);\n    a.fetch_add(1);\n    \
         let m = Mutex.new(2);\n    let j = Json.parse(\"[1, 2]\");\n    \
         f\"{a.load()} {j.is_ok()}\"\n}\n",
    )
    .unwrap();
    let (ok, out) = output(karac().arg("run").current_dir(&dir));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "5 true", "{out}");
}
