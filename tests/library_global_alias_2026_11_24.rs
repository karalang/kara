//! B-2026-11-23-14: `import std.io.{File as F};` binds `F` to the global
//! `File` (design.md § Imports: `import a.b.c as X;` binds `X`). The import
//! was refused with E0113, "cannot be renamed yet", which named one edit and
//! carried none. The alias now works for every kind of global, in a lone file
//! and in a package's module; it is refused only where the file gives the
//! name to something else too, and that refusal names the choices with no
//! edit, since which uses mean the import is not determined.

use std::path::{Path, PathBuf};
use std::process::Command;

const PROGRAM: &str = "import std.io.{File as F};
import std.sync.{channel as ch, Mutex as M, Sender as Tx};
import std.hash.{Hasher as H, FxBuildHasher as FB};

fn opens(path: Str) -> bool {
    let r: Result[F, IoError] = F.open(path);
    r.is_err()
}

fn put(t: ref Tx[i64], v: i64) {
    let _ = t.send(v);
}

fn digest[T: H](h: ref T) -> u64 {
    h.finish()
}

fn shadow() -> i64 {
    let ch = 7;
    ch + 1
}

fn main() {
    println(opens(\"/nonexistent\"));
    let (tx, rx) = ch(1);
    put(tx, 5);
    println(rx.recv().unwrap());
    let m: M[i64] = M.new(3);
    println(m.lock().value);
    let mut fx = FB {}.build();
    fx.write_u64(1);
    println(digest(fx) > 0);
    println(shadow());
}
";

const EXPECTED: &str = "true\n5\n3\ntrue\n8\n";

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-global-alias-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn karac(args: &[&str], cwd: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn a_library_global_imported_under_an_alias_checks_and_runs() {
    let d = dir("file");
    std::fs::write(d.join("main.kara"), PROGRAM).unwrap();
    let (ok, out) = karac(&["check", "main.kara"], &d);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["__mir-run", "main.kara"], &d);
    assert!(ok, "{out}");
    assert_eq!(out, EXPECTED);
}

#[cfg(feature = "llvm")]
#[test]
fn a_library_global_imported_under_an_alias_runs_compiled() {
    let d = dir("run");
    std::fs::write(d.join("main.kara"), PROGRAM).unwrap();
    let (ok, out) = karac(&["run", "main.kara"], &d);
    assert!(ok, "{out}");
    assert_eq!(out, EXPECTED);
}

#[test]
fn a_library_global_alias_works_in_a_package_module() {
    let d = dir("pkg");
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::write(
        d.join("kara.toml"),
        "[package]\nname = \"pk\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        d.join("src/main.kara"),
        "import std.io.{File as F};\nimport util;\n\nfn main() {\n    println(F.open(\"/nonexistent\").is_err());\n    println(util.two());\n}\n",
    )
    .unwrap();
    std::fs::write(d.join("src/util.kara"), "pub fn two() -> i64 {\n    2\n}\n").unwrap();
    let (ok, out) = karac(&["check"], &d);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["run", "--interp"], &d);
    assert!(ok, "{out}");
    assert_eq!(out, "true\n2\n");
}

#[test]
fn an_alias_the_file_also_declares_is_refused_with_help_and_no_edit() {
    // `F` is a generic parameter too: whether a use of `F` means `File` or
    // the parameter is the program's to say, so there is no edit.
    let d = dir("clash");
    std::fs::write(
        d.join("main.kara"),
        "import std.io.{File as F};\n\nfn id[F](x: F) -> F {\n    x\n}\n\nfn main() {\n    println(id(1));\n}\n",
    )
    .unwrap();
    let (ok, out) = karac(&["check", "--output=json", "main.kara"], &d);
    assert!(!ok, "{out}");
    let j: serde_json::Value = serde_json::from_str(out.lines().next().unwrap_or(""))
        .unwrap_or_else(|e| panic!("{e}: {out}"));
    let ds = j["diagnostics"].as_array().unwrap();
    assert_eq!(ds.len(), 1, "{out}");
    let d0 = &ds[0];
    assert_eq!(d0["code"], "E0113", "{out}");
    let m = d0["message"].as_str().unwrap();
    assert!(m.contains("cannot be imported as `F`"), "{m}");
    assert!(m.contains("import it as `File`"), "{m}");
    assert!(m.contains("under a name the file does not use"), "{m}");
    assert!(
        d0.get("fix").is_none() && d0.get("fix_diff").is_none(),
        "{out}"
    );
}
