//! A local hides an import only inside its own scope (design.md § Imports,
//! B-2026-11-16-6). A package is run and checked as one flat program, and the
//! rewrites that keep its imports alive there gave a name up for the whole
//! file once any function bound it as a value: a parameter, a `let` in a
//! block, a `for`, `match` or closure binding, a `let` after the use, or a
//! local named like an imported module.

use std::path::PathBuf;
use std::process::{Command, Output};

fn package(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-import-local-scope-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    for (path, text) in files {
        std::fs::write(dir.join("src").join(path), text).unwrap();
    }
    dir
}

fn karac_in(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .env_remove("KARAC_LEGACY")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

const A: &str = "pub fn load() -> i64 { 7 }
pub fn helper() -> i64 { 1 }
pub fn ah(helper: i64) -> i64 { helper + 1 }
";

const B: &str = "pub fn helper() -> i64 { 2 }
pub fn twice(helper: i64) -> i64 { helper * 2 }
pub fn hb() -> i64 { helper() }
";

const MAIN: &str = "import a.load as ld;
import a;
import b.{helper, twice, hb};

fn other(ld: i64) -> i64 { ld }

fn blk() -> i64 {
    let r = {
        let ld = 5;
        ld + 1
    };
    r + ld()
}

fn lp() -> i64 {
    let mut s = 0;
    for ld in 0..3 {
        s += ld;
    }
    s + ld()
}

fn mt(o: Option[i64]) -> i64 {
    match o {
        Some(ld) => ld,
        None => ld(),
    }
}

fn cl() -> i64 {
    let f = |ld: i64| ld * 10;
    f(2) + ld()
}

fn late() -> i64 {
    let x = ld();
    let ld = 100;
    x + ld
}

fn modname() -> i64 {
    let a = 1;
    a + 1
}

fn hl() -> i64 {
    let v = {
        let helper = 40;
        helper
    };
    v + helper()
}

fn main() {
    println(other(3));
    println(blk());
    println(lp());
    println(mt(Some(4)));
    println(mt(None));
    println(cl());
    println(late());
    println(modname());
    println(a.load());
    println(hl());
    println(a.helper());
    println(twice(5));
    println(hb());
    println(f\"{ld()}\");
    println(a.ah(1));
}
";

const EXPECTED: &str = "3\n13\n10\n4\n7\n27\n107\n2\n7\n42\n1\n10\n2\n7\n2\n";

/// An alias, a module and a renamed name each stay bound where no local of
/// their name is in scope, and each local stays the local where it is:
/// `check` accepts the package and both executors print what the locals and
/// the imports hold.
#[test]
fn a_local_hides_an_import_only_in_its_own_scope() {
    let dir = package("all", &[("main.kara", MAIN), ("a.kara", A), ("b.kara", B)]);
    let out = karac_in(&dir, &["check", "src/main.kara"]);
    assert!(
        out.status.success(),
        "check: {}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let mut args = lane.to_vec();
        args.push("src/main.kara");
        let out = karac_in(&dir, &args);
        assert_eq!(
            text(&out.stdout),
            EXPECTED,
            "{lane:?}: {}",
            text(&out.stderr)
        );
        assert!(out.status.success(), "{lane:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A use the local does not reach still names the import: `ld` written
/// before `let ld` in the same block, and after the block that bound it.
#[test]
fn an_import_used_outside_its_shadow_is_the_import() {
    let main = "import a.load as ld;

fn main() {
    let before = ld();
    {
        let ld = 1;
        println(ld);
    }
    let after = ld();
    let ld = 2;
    println(before + after + ld);
}
";
    let dir = package("outside", &[("main.kara", main), ("a.kara", A)]);
    let out = karac_in(&dir, &["run", "src/main.kara"]);
    assert_eq!(text(&out.stdout), "1\n16\n", "{}", text(&out.stderr));
    let _ = std::fs::remove_dir_all(&dir);
}
