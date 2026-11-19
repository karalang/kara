//! B-2026-11-19-12: `Option.Some(x)` and the other prelude variants written
//! with their type are accepted (6,807 such uses in the corpus), and the
//! `qualified_prelude_variant` style lint rewrites them to the canonical
//! bare form under `karac fix` or `-W`, in expressions and patterns.

use std::path::PathBuf;
use std::process::{Command, Output};

fn karac(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn scratch(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-qpv-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

const QUALIFIED: &str = "fn half(n: i64) -> Result[i64, i64] {
    if n % 2 == 0 { Result.Ok(n / 2) } else { Result.Err(n) }
}

fn main() {
    let a = Option.Some(4);
    let none: Option[i64] = Option.None;
    match a {
        Option.Some(x) => println(x),
        Option.None => println(0),
    }
    match half(7) {
        Result.Ok(v) => println(v),
        Result.Err(e) => println(e),
    }
    println(none.is_none());
}
";

const BARE: &str = "fn half(n: i64) -> Result[i64, i64] {
    if n % 2 == 0 { Ok(n / 2) } else { Err(n) }
}

fn main() {
    let a = Some(4);
    let none: Option[i64] = None;
    match a {
        Some(x) => println(x),
        None => println(0),
    }
    match half(7) {
        Ok(v) => println(v),
        Err(e) => println(e),
    }
    println(none.is_none());
}
";

#[test]
fn qualified_prelude_variant_is_accepted_silently_by_default() {
    let f = scratch("default", QUALIFIED);
    let out = karac(&["check", f.to_str().unwrap()]);
    let err = text(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(!err.contains("qualified_prelude_variant"), "{err}");
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
}

#[test]
fn qualified_prelude_variant_warns_under_w_and_fix_writes_the_bare_form() {
    let f = scratch("fix", QUALIFIED);
    let p = f.to_str().unwrap();
    let before = karac(&["run", "--interp", p]);
    assert!(before.status.success(), "{}", text(&before.stderr));
    let warned = text(&karac(&["check", "-W", "qualified_prelude_variant", p]).stderr);
    assert_eq!(
        warned.matches("warning[qualified_prelude_variant]").count(),
        8,
        "{warned}"
    );
    let fixed = karac(&["fix", p]);
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    assert_eq!(std::fs::read_to_string(&f).unwrap(), BARE);
    let recheck = text(&karac(&["check", "-W", "qualified_prelude_variant", p]).stderr);
    assert!(!recheck.contains("qualified_prelude_variant"), "{recheck}");
    let after = karac(&["run", "--interp", p]);
    assert_eq!(text(&after.stdout), text(&before.stdout));
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
}

/// A program with an `Option` of its own names its own variants.
#[test]
fn qualified_prelude_variant_leaves_a_programs_own_option_alone() {
    let src = "enum Option { Some(i64), None }\nfn main() {\n    let o = Option.Some(1);\n    match o { Option.Some(n) => println(n), Option.None => {} }\n}\n";
    let f = scratch("own", src);
    let warned = text(
        &karac(&[
            "check",
            "-W",
            "qualified_prelude_variant",
            f.to_str().unwrap(),
        ])
        .stderr,
    );
    assert!(!warned.contains("qualified_prelude_variant"), "{warned}");
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
}
