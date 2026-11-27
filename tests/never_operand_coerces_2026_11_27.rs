//! A `Never` operand of a binary operator (B-2026-11-27-11): `Never` is the
//! bottom type and coerces to any type (design.md § Types), so
//! `let a = { return 7; }; a + 1` checks, as Rust accepts it, and so does
//! the tuple form `t.0 + t.1` over `({ return 8; }, 1)`. The operation is
//! never reached: the operands run in order up to the one that diverges.
//! Before the fix both forms were refused E0208 "type 'Never' does not
//! implement trait Add". The checker has no unreachable-code warning, so
//! none is asked for.

use std::process::Command;

fn karac(name: &str, sub: &str, src: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac_never_operand_{}_{name}_{}",
        std::process::id(),
        sub.trim_start_matches('_')
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([sub, path.to_str().unwrap()])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn checks_and_runs(name: &str, src: &str, want: &str, code: i32) {
    let (c, out) = karac(name, "check", src);
    assert_eq!(c, 0, "{name} was refused: {out}");
    let (c, out) = karac(name, "__mir-run", src);
    assert_eq!(c, code, "{name}: {out}");
    assert!(out.contains(want), "{name}: want {want:?} in {out}");
}

#[test]
fn a_never_binding_in_arithmetic_checks_and_returns_early() {
    checks_and_runs(
        "binding",
        "fn f() -> i64 {\n    let a = { return 7; };\n    a + 1\n}\nfn main() {\n    println(f\"{f()}\");\n}\n",
        "7\n",
        0,
    );
}

#[test]
fn a_never_tuple_field_in_arithmetic_checks_and_returns_early() {
    checks_and_runs(
        "tuple",
        "fn f() -> i64 {\n    let t = ({ return 8; }, 1);\n    t.0 + t.1\n}\nfn main() {\n    println(f\"{f()}\");\n}\n",
        "8\n",
        0,
    );
}

#[test]
fn never_operands_in_a_comparison_a_product_and_a_concatenation() {
    checks_and_runs(
        "kinds",
        "fn g() -> bool {\n    let a = { return true; };\n    a < 3\n}\nfn h() -> i64 {\n    let a = { return 9; };\n    let b = { return 10; };\n    a * b\n}\nfn s() -> String {\n    let s = { return \"early\"; };\n    s + \"!\"\n}\nfn main() {\n    println(f\"{g()} {h()} {s()}\");\n}\n",
        "true 9 early\n",
        0,
    );
}

#[test]
fn a_diverging_operand_stops_the_operands_after_it() {
    checks_and_runs(
        "order",
        "fn side(s: String) -> i64 {\n    println(s);\n    1\n}\nfn main() {\n    let x = side(\"left runs\") + panic(\"right panics\");\n    let y = panic(\"unreached\") + side(\"never printed\");\n    println(f\"{x} {y}\");\n}\n",
        "left runs\n",
        101,
    );
    let (_, out) = karac(
        "order2",
        "__mir-run",
        "fn side(s: String) -> i64 {\n    println(s);\n    1\n}\nfn main() {\n    let y = panic(\"left panics\") + side(\"never printed\");\n    println(f\"{y}\");\n}\n",
    );
    assert!(out.contains("left panics"), "{out}");
    assert!(!out.contains("never printed"), "{out}");
}
