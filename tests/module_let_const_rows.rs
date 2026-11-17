//! B-2026-11-17-125: a top-level `let my_count` beside `fn main` got
//! E_MODULE_BINDING_NAMING, whose rename to `MY_COUNT` only led to
//! E_MODULE_LET. E_MODULE_LET now comes first, and its fix declares the
//! `const` under the SCREAMING_SNAKE_CASE name and renames the uses in the
//! same edit; an initializer computed at run time gets no fix, and the help
//! points at a `fn` or a provider.

use std::path::PathBuf;
use std::process::{Command, Output};

fn scratch(name: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-module-let-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), source).unwrap();
    dir
}

fn karac(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn a_lowercase_module_let_is_e_module_let_and_one_fix_makes_it_a_const() {
    let dir = scratch(
        "lower",
        "let my_count: i64 = 4;\nfn main() { println(my_count + my_count); }\n",
    );
    let out = karac(&dir, &["check", "m.kara"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("error[E_MODULE_LET]"), "{err}");
    assert!(!err.contains("E_MODULE_BINDING_NAMING"), "{err}");
    assert!(
        err.contains(
            "help: declare it `const MY_COUNT`: a constant's name is SCREAMING_SNAKE_CASE"
        ),
        "{err}"
    );
    let fix = karac(&dir, &["fix", "m.kara"]);
    assert!(fix.status.success(), "{fix:?}");
    assert_eq!(
        std::fs::read_to_string(dir.join("m.kara")).unwrap(),
        "const MY_COUNT: i64 = 4;\nfn main() { println(MY_COUNT + MY_COUNT); }\n"
    );
    let check = karac(&dir, &["check", "m.kara"]);
    assert!(check.status.success(), "{check:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_lowercase_module_let_mut_is_e_module_let_without_a_rename() {
    let dir = scratch(
        "mut",
        "let mut my_count: i64 = 0;\nfn main() { println(my_count); }\n",
    );
    let out = karac(&dir, &["check", "--output=json", "m.kara"]);
    let json = String::from_utf8_lossy(&out.stdout);
    assert!(json.contains("\"code\":\"E_MODULE_LET\""), "{json}");
    assert!(json.contains("no mutable globals"), "{json}");
    assert!(!json.contains("E_MODULE_BINDING_NAMING"), "{json}");
    assert!(!json.contains("MY_COUNT"), "{json}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_module_let_computed_at_run_time_points_at_a_fn_or_a_provider() {
    let dir = scratch(
        "computed",
        "fn mk() -> i64 { 3 }\nlet names = mk();\nfn main() { println(names); }\n",
    );
    let out = karac(&dir, &["check", "--output=json", "m.kara"]);
    let json = String::from_utf8_lossy(&out.stdout);
    let d = json
        .split("{\"id\":")
        .find(|d| d.contains("\"code\":\"E_MODULE_LET\""))
        .unwrap_or_else(|| panic!("{json}"));
    assert!(d.contains("computed at run time"), "{d}");
    assert!(d.contains("in a `fn` that `main` calls"), "{d}");
    assert!(d.contains("whose provider `main` installs"), "{d}");
    assert!(!d.contains("\"replacement\""), "{d}");
    assert!(!d.contains("\"fix_diff\""), "{d}");
    let _ = std::fs::remove_dir_all(&dir);
}
