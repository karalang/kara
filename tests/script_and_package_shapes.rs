//! Three probe rows on how a program is put together (design.md § Script
//! mode, § Modules):
//!
//! - Script mode's implicit `main` is `fn main() -> Result[(), AnyError]`,
//!   ending with `Ok(())`, so a top-level `?` works and an `Err` exits 1
//!   with `Error: ..` (B-2026-11-04-19: it returned `()` and the `?` was
//!   refused).
//! - `import db.connection;` binds the nested module `connection` when
//!   `db` is only a directory (B-2026-11-04-21: "unknown module `db`").
//! - `karac run` refuses a package with both `main.kara` and `lib.kara`,
//!   as `karac build` does (B-2026-11-04-23: it ran `main.kara`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("karac-script-package-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn file(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn karac(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Both v2 executors of `karac run`: the default, and `--interp`.
const RUNS: [&[&str]; 2] = [&["run"], &["run", "--interp"]];

const SCRIPT: &str = "struct Bad { why: String }
impl Error for Bad { fn message(self) -> String { f\"bad:{self.why}\" } }
impl Display for Bad { fn to_string(self) -> String { f\"bad:{self.why}\" } }
fn chk(n: i64) -> Result[i64, Bad] {
    if n < 0 { Err(Bad { why: f\"neg{n}\" }) } else { Ok(n * 2) }
}
fn h() -> Result[i64, AnyError] { Ok(2) }
println(\"a\");
let x = h()?;
let v = chk(x)?;
println(v);
let w = chk(-1)?;
println(w);
";

#[test]
fn a_script_question_propagates_out_of_the_implicit_main() {
    let dir = Scratch::new("script-err");
    let f = dir.file("script.kara", SCRIPT);
    let f = f.to_str().unwrap();
    let out = karac(&dir.0, &["check", f]);
    assert!(out.status.success(), "check: {}", text(&out.stderr));
    for run in RUNS {
        let mut args = run.to_vec();
        args.push(f);
        let out = karac(&dir.0, &args);
        assert_eq!(out.status.code(), Some(1), "{run:?}: {}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "a\n4\n", "{run:?}");
        assert!(
            text(&out.stderr).starts_with("Error: bad:neg-1\n"),
            "{run:?}: {}",
            text(&out.stderr)
        );
    }
}

#[test]
fn a_script_that_ends_cleanly_exits_zero() {
    let dir = Scratch::new("script-ok");
    let f = dir.file(
        "script.kara",
        "fn h() -> Result[i64, AnyError] { Ok(2) }\nlet x = h()?;\nprintln(x + 1);\n",
    );
    for run in RUNS {
        let mut args = run.to_vec();
        args.push(f.to_str().unwrap());
        let out = karac(&dir.0, &args);
        assert!(out.status.success(), "{run:?}: {}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "3\n", "{run:?}");
    }
}

const MANIFEST: &str = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n";

#[test]
fn a_nested_module_imported_whole_binds_its_last_segment() {
    let dir = Scratch::new("nested");
    dir.file("kara.toml", MANIFEST);
    dir.file(
        "src/db/connection.kara",
        "pub struct Connection { pub port: i64 }\npub fn open(p: i64) -> Connection { Connection { port: p } }\n",
    );
    dir.file(
        "src/main.kara",
        "import db.connection;\nimport db.connection as c;\n\
         fn main() {\n    let a: connection.Connection = connection.open(7);\n    println(f\"{a.port} {c.open(8).port}\");\n}\n",
    );
    let out = karac(&dir.0, &["check"]);
    assert!(out.status.success(), "check: {}", text(&out.stderr));
    for run in RUNS {
        let mut args = run.to_vec();
        args.push("src/main.kara");
        let out = karac(&dir.0, &args);
        assert!(out.status.success(), "{run:?}: {}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "7 8\n", "{run:?}");
    }
}

#[test]
fn a_missing_nested_module_is_still_unknown() {
    let dir = Scratch::new("nested-missing");
    dir.file("kara.toml", MANIFEST);
    dir.file("src/db/connection.kara", "pub fn f() -> i64 { 1 }\n");
    dir.file(
        "src/main.kara",
        "import db.nothere;\nfn main() { println(1); }\n",
    );
    let out = karac(&dir.0, &["check"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stdout).contains("unknown module")
            || text(&out.stderr).contains("unknown module"),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
}

#[test]
fn run_refuses_a_package_with_main_and_lib() {
    let dir = Scratch::new("main-and-lib");
    dir.file("kara.toml", MANIFEST);
    dir.file("src/main.kara", "fn main() { println(\"ran\"); }\n");
    dir.file("src/lib.kara", "pub fn f() -> i64 { 1 }\n");
    for run in RUNS {
        let mut args = run.to_vec();
        args.push("src/main.kara");
        let out = karac(&dir.0, &args);
        assert_eq!(out.status.code(), Some(1), "{run:?}");
        assert_eq!(text(&out.stdout), "", "{run:?}");
        assert!(
            text(&out.stderr).contains("cannot contain both"),
            "{run:?}: {}",
            text(&out.stderr)
        );
    }
}
