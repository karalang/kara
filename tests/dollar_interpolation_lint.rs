//! The `dollar_interpolation` lint (W0325, allowed by default), from the
//! Kotlin/Swift habit sweep, B-2026-11-24-47: `$name` or `${expr}` in a
//! plain string where the names are bindings in scope, the template a
//! Kotlin, Dart or shell author meant to interpolate. `karac fix` writes the
//! f-string. A `$` that names no binding, a raw string and a function name
//! are left alone.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn file(src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-dollar-interpolation-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

fn karac(args: &[&str], f: &std::path::Path) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(f)
        .output()
        .unwrap();
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr),
    )
}

/// `src` under `#[deny(dollar_interpolation)]`: whether the check refused
/// it, the text after `karac fix`, and what it then prints.
fn denied(src: &str) -> (bool, String, String) {
    let f = file(&format!("#[deny(dollar_interpolation)]\n{src}"));
    let refused = !karac(&["check"], &f).0;
    karac(&["fix"], &f);
    let (clean, msg) = karac(&["check"], &f);
    assert!(clean, "still refused after the fix:\n{msg}");
    let text = std::fs::read_to_string(&f).unwrap();
    let out = karac(&["__mir-run"], &f).1;
    (refused, text, out)
}

#[test]
fn dollar_interpolation_is_allowed_by_default_b_2026_11_24_47() {
    let f = file("fn main() { let n = 3; println(\"n is $n\"); }\n");
    let (ok, out) = karac(&["check"], &f);
    assert!(ok, "{out}");
    assert!(!out.contains("dollar_interpolation"), "{out}");
    let (_, out) = karac(&["check", "-W", "dollar_interpolation"], &f);
    assert!(out.contains("warning[dollar_interpolation]"), "{out}");
}

#[test]
fn dollar_name_and_expr_become_f_string_holes_b_2026_11_24_47() {
    let (refused, text, out) = denied(
        "fn main() {\n    let n = 3;\n    let p = P { x: 4 };\n    \
         println(\"n is $n, next ${n + 1}, x=${p.x}\");\n}\n\
         struct P { x: i64 }\n",
    );
    assert!(refused);
    assert!(
        text.contains("println(f\"n is {n}, next {n + 1}, x={p.x}\");"),
        "{text}"
    );
    assert_eq!(out, "n is 3, next 4, x=4\n");
}

#[test]
fn dollar_fix_keeps_escapes_and_doubles_braces_b_2026_11_24_47() {
    let (refused, text, out) =
        denied("fn main() {\n    let n = 3;\n    println(\"{$n}\\t\\\"$n\\\" $HOME\");\n}\n");
    assert!(refused);
    assert!(
        text.contains("println(f\"{{{n}}}\\t\\\"{n}\\\" $HOME\");"),
        "{text}"
    );
    assert_eq!(out, "{3}\t\"3\" $HOME\n");
}

#[test]
fn dollar_naming_no_binding_is_left_alone_b_2026_11_24_47() {
    let src = "fn twice(n: i64) -> i64 { n * 2 }\n\
               fn main() {\n    let n = 3;\n    \
               println(\"$5 $HOME $twice ${twice(n)} $_\");\n    \
               println(r\"^x$n\");\n    println(n);\n}\n";
    let (refused, text, out) = denied(src);
    assert!(!refused, "{text}");
    assert_eq!(out, "$5 $HOME $twice ${twice(n)} $_\n^x$n\n3\n");
}
