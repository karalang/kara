//! core-semantics §14.14: in `effect resource R: T = e;`, `e` is a constant
//! expression (design.md § Constants), evaluated once into a static before
//! `main`. A default built by a call ran that call before `main` and was
//! accepted; it is refused, naming the resource, and run-time setup is an
//! override in `main`.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-resdef-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const PRELUDE: &str = "trait Log { fn count(self) -> i64; }
struct RealLog { base: i64, label: Str }
impl Log for RealLog { fn count(self) -> i64 { self.base + self.label.len() } }
enum Level { Low, High(i64) }
struct Leveled { level: Level }
impl Log for Leveled { fn count(self) -> i64 { match self.level { Level.Low => 0, Level.High(n) => n } } }
const BASE: i64 = 40;
fn make() -> i64 { println(\"side effect\"); 3 }
";

fn program(decl: &str) -> String {
    format!("{PRELUDE}{decl}\nfn main() {{ println(Audit.count()); }}\n")
}

#[test]
fn a_resource_default_built_by_a_call_is_refused() {
    for (name, decl, what) in [
        (
            "call",
            "effect resource Audit: Log = RealLog { base: make(), label: \"x\" };",
            "function call",
        ),
        (
            "method",
            "effect resource Audit: Log = RealLog { base: \"ab\".len(), label: \"x\" };",
            "method call",
        ),
        (
            "block",
            "effect resource Audit: Log = { RealLog { base: 1, label: \"x\" } };",
            "block or control-flow expression",
        ),
    ] {
        let (ok, err) = check(name, &program(decl));
        assert!(!ok, "{name}: check must refuse");
        assert!(
            err.contains(
                "the default provider of `effect resource Audit` must be a compile-time constant"
            ) && err.contains(what)
                && err.contains("with_provider[Audit]"),
            "{name}: {err}"
        );
    }
}

#[test]
fn a_resource_default_of_constants_is_accepted() {
    for (name, decl) in [
        (
            "literal",
            "effect resource Audit: Log = RealLog { base: BASE + 1, label: \"real\" };",
        ),
        (
            "variant",
            "effect resource Audit: Log = Leveled { level: Level.High(BASE) };",
        ),
    ] {
        let (ok, err) = check(name, &program(decl));
        assert!(ok, "{name}: {err}");
    }
}
