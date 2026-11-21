//! The review thread's rulings on the second design.md §4/§16 conformance
//! pass (2026-11-21, conformance/design/{modules,targets}): `karac check`
//! checks every declared target and never the host alone
//! (B-2026-11-22-31); a refused constant initializer names the effect of
//! what it calls (B-2026-11-22-32); the §4 wording roll-up
//! (B-2026-11-22-33); the manifest rulings (B-2026-11-22-34); and
//! `#[target(gpu|wasm_browser)]` is a later-track warning, with no gpu or
//! wasm_browser check pass (B-2026-11-22-35).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-rulings-1121-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn karac(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

const NET_MAIN: &str = "fn fetch() -> i64 {
    match TcpStream.connect(\"127.0.0.1:1\") { Ok(_) => 1, Err(_) => 0 }
}
fn main() { println(fetch()); }
";

fn package(tag: &str, manifest_extra: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = scratch(tag);
    write(
        &dir.join("kara.toml"),
        &format!("[package]\nname = \"app\"\nversion = \"0.1.0\"\n{manifest_extra}"),
    );
    for (rel, text) in files {
        write(&dir.join(rel), text);
    }
    dir
}

#[test]
fn check_sweeps_one_declared_target_that_is_not_the_host() {
    let dir = package(
        "one-declared",
        "[build]\ntargets = [\"wasm_wasi\"]\n",
        &[
            ("src/main.kara", NET_MAIN),
            ("src/u.kara", "pub fn f() -> i64 { 1 }\n"),
        ],
    );
    let (ok, out) = karac(&dir, &["check"]);
    assert!(!ok && out.contains("E0411"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_sweeps_one_named_target_in_any_package_shape() {
    for (tag, files) in [
        ("lone-entry", vec![("src/main.kara", NET_MAIN)]),
        (
            "multi-file",
            vec![
                ("src/main.kara", NET_MAIN),
                ("src/u.kara", "pub fn f() -> i64 { 1 }\n"),
            ],
        ),
    ] {
        let dir = package(tag, "", &files);
        let (ok, out) = karac(&dir, &["check"]);
        assert!(ok, "{tag}: the host check passes: {out}");
        let (ok, out) = karac(&dir, &["check", "--targets=wasm_wasi"]);
        assert!(!ok && out.contains("E0411"), "{tag}: {out}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn lone_entry_package_names_its_file_relative_to_the_package() {
    let dir = package(
        "lone-path",
        "",
        &[("src/main.kara", "fn main() { let x: i64 = \"s\"; }\n")],
    );
    let (ok, out) = karac(&dir, &["check"]);
    assert!(!ok, "{out}");
    assert!(out.contains("src/main.kara:1:"), "{out}");
    assert!(!out.contains(dir.to_str().unwrap()), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn effectful_constant_initializer_names_the_effect_at_the_call() {
    let dir = scratch("const-effect");
    write(
        &dir.join("cell.kara"),
        "fn shout() -> i64 { println(\"x\"); 1 }\n\
         const X: i64 = shout() + 1;\n\
         const N: i64 = { println(\"y\"); 1 };\n\
         fn main() { println(X + N); }\n",
    );
    let (ok, out) = karac(&dir, &["check", "cell.kara"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("constant 'X' initializer must be a compile-time constant"),
        "{out}"
    );
    assert!(
        out.contains("note[L0004]: cell.kara:2:16: the initializer of constant 'X' performs `writes(Stdout)` here, through `shout`"),
        "{out}"
    );
    assert!(
        out.contains("cell.kara:3:18: the initializer of constant 'N' performs `writes(Stdout)` here, through `println`"),
        "{out}"
    );
    // A pure call is refused as a call, with no effect note.
    write(
        &dir.join("cell.kara"),
        "fn one() -> i64 { 1 }\nconst X: i64 = one();\nfn main() { println(X); }\n",
    );
    let (ok, out) = karac(&dir, &["check", "cell.kara"]);
    assert!(!ok && !out.contains("L0004"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn block_constant_with_visibility_is_refused_by_name() {
    let dir = scratch("block-pub-const");
    for kw in ["pub", "private"] {
        write(
            &dir.join("cell.kara"),
            &format!("fn main() {{ {kw} const K: i64 = 1; println(K); }}\n"),
        );
        let (ok, out) = karac(&dir, &["check", "cell.kara"]);
        assert!(!ok, "{out}");
        assert!(
            out.contains("a constant declared in a block takes no visibility keyword")
                && out.contains(&format!("remove `{kw}`")),
            "{out}"
        );
        assert_eq!(
            out.matches("error").count(),
            2,
            "one error and the count line: {out}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn default_visibility_type_in_pub_signature_is_not_called_private() {
    let dir = scratch("e0221");
    write(
        &dir.join("cell.kara"),
        "struct Hidden { n: i64 }\npub fn make() -> Hidden { Hidden { n: 1 } }\nfn main() { }\n",
    );
    let (ok, out) = karac(&dir, &["check", "cell.kara"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("type 'Hidden', which is not `pub`, leaks through"),
        "{out}"
    );
    assert!(!out.contains("private type"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_help_names_no_internal_mir_switch() {
    let dir = scratch("help");
    let (_, out) = karac(&dir, &["build", "--help"]);
    assert!(!out.contains("KARAC_MIR_OPT"), "{out}");
    assert!(out.contains("KARAC_OPT_LEVEL"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn manifest_rulings() {
    let main = [("src/main.kara", "fn main() { println(\"hi\"); }\n")];
    let cases: &[(&str, &str, &str)] = &[
        (
            "name",
            "[package]\nname = \"my app\"\n",
            "did you mean `name = \"my_app\"`?",
        ),
        (
            "authors",
            "[package]\nname = \"app\"\nauthors = \"Alice\"\n",
            "a list of strings",
        ),
        (
            "targets-empty",
            "[package]\nname = \"app\"\n[build]\ntargets = []\n",
            "`[build].targets`: is empty",
        ),
    ];
    for (tag, manifest, want) in cases {
        let dir = scratch(tag);
        write(&dir.join("kara.toml"), manifest);
        for (rel, text) in main {
            write(&dir.join(rel), text);
        }
        let (ok, out) = karac(&dir, &["check"]);
        assert!(!ok && out.contains(want), "{tag}: {out}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    let dir = package("unknown-table", "\n[dependecies]\nx = \"1\"\n", &main);
    let (ok, out) = karac(&dir, &["check"]);
    assert!(ok && out.contains("unknown table `[dependecies]`"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
    let dir = scratch("dash-name");
    write(&dir.join("kara.toml"), "[package]\nname = \"my-app\"\n");
    write(&dir.join("src/main.kara"), main[0].1);
    let (ok, out) = karac(&dir, &["check"]);
    assert!(ok, "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn later_track_target_gate_warns_and_runs_no_pass() {
    let dir = scratch("later-track");
    write(
        &dir.join("cell.kara"),
        "#[target(wasm_browser)]\nfn in_browser() -> i64 { \"not checked\" }\n\
         #[target(gpu)]\nfn on_gpu() -> i64 { 1 }\n\
         fn main() { println(\"hi\"); }\n",
    );
    let (ok, out) = karac(&dir, &["check", "cell.kara"]);
    assert!(ok, "{out}");
    for t in ["wasm_browser", "gpu"] {
        assert!(
            out.contains(&format!(
                "`#[target({t})]` names a target that is not in v1"
            )),
            "{out}"
        );
        assert!(!out.contains(&format!("── target: {t} ──")), "{out}");
    }
    assert!(
        !out.contains("NOT checked") && !out.contains("E0411"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
