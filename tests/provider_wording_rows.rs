//! B-2026-11-17-16: E0600 called `Env` "provider-rooted" though it is
//! program-rooted (core-semantics §14.7), and `karac query effects` charged
//! `with_provider[R](..)` with `panics` through an index the source does not
//! have, naming the internal `__builtin_index` in its derivation.

use std::path::PathBuf;
use std::process::{Command, Output};

fn scratch(name: &str, source: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-provider-wording-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("m.kara");
    std::fs::write(&file, source).unwrap();
    (dir, file)
}

fn karac(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn an_overridden_program_rooted_resource_is_named_as_overridden() {
    let (dir, _) = scratch(
        "env",
        "struct Fe {}\n\
         impl EnvProvider for Fe {\n\
         \x20   fn var(ref self, key: Str) -> Result[String, VarError] { Ok(\"v\") }\n\
         }\n\
         fn make() -> (Fn() -> String) {\n\
         \x20   with_provider[Env](Fe {}, || {\n\
         \x20       let g = || env.var(\"K\").unwrap();\n\
         \x20       g\n\
         \x20   })\n\
         }\n\
         fn main() { println(make()()); }\n",
    );
    let out = karac(&dir, &["check", "m.kara"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(err.contains("error[E0600]"), "{err}");
    assert!(
        err.contains("uses `Env`, overridden by the `with_provider` at 6:5"),
        "{err}"
    );
    assert!(!err.contains("provider-rooted"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_provider_rooted_resource_keeps_its_wording() {
    let (dir, _) = scratch(
        "db",
        "effect resource Db: DbProvider;\n\
         trait DbProvider { fn get(ref self) -> i64; }\n\
         struct D {}\n\
         impl DbProvider for D { fn get(ref self) -> i64 { 1 } }\n\
         fn make() -> (Fn() -> i64) {\n\
         \x20   with_provider[Db](D {}, || {\n\
         \x20       let g = || Db.get();\n\
         \x20       g\n\
         \x20   })\n\
         }\n\
         fn main() { println(make()()); }\n",
    );
    let out = karac(&dir, &["check", "m.kara"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("uses provider-rooted resource `Db`, whose provider is bound by"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn with_provider_is_not_an_index_and_an_index_is_named() {
    let (dir, _) = scratch(
        "query",
        "effect resource Db: DbProvider;\n\
         trait DbProvider { fn get(ref self) -> i64; }\n\
         struct D {}\n\
         impl DbProvider for D { fn get(ref self) -> i64 { 1 } }\n\
         fn w() -> i64 with reads(Db) { with_provider[Db](D {}, || Db.get()) }\n\
         fn f(v: Vec[i64]) -> i64 { v[0] }\n\
         fn main() { println(w()); println(f(Vec.filled(1, 0))); }\n",
    );
    let w = String::from_utf8_lossy(&karac(&dir, &["query", "effects", "m.kara.w"]).stdout)
        .into_owned();
    assert!(!w.contains("panics"), "{w}");
    assert!(!w.contains("__builtin"), "{w}");
    let f = String::from_utf8_lossy(&karac(&dir, &["query", "effects", "m.kara.f"]).stdout)
        .into_owned();
    assert!(
        f.contains("f has panics (an index that can be out of bounds)"),
        "{f}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
