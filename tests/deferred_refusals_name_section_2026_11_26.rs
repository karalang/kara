//! B-2026-11-25-71: a construct docs/deferred.md schedules is refused with
//! a message that names its deferred.md section, so the refusal reads as
//! "not yet" rather than as a typo or a parse error. Each case is the
//! matching `conformance/deferred/*_refused` cell's program, and each
//! heading named here exists in docs/deferred.md.

use std::process::Command;

const DEFERRED_MD: &str = include_str!("../docs/deferred.md");

fn check(name: &str, src: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("karac_deferred_note_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success(), "{name} was accepted");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn assert_names(name: &str, src: &str, code: &str, section: &str) {
    assert!(
        DEFERRED_MD.contains(&format!("\n### {section}\n")),
        "deferred.md has no heading `### {section}`"
    );
    let out = check(name, src);
    assert!(out.contains(&format!("error[{code}]")), "{name}: {out}");
    assert!(
        out.contains(&format!("docs/deferred.md § {section}")),
        "{name} does not name § {section}: {out}"
    );
}

#[test]
fn if_let_chain_refusal_names_its_section() {
    assert_names(
        "let_chain",
        "fn main() {\n    let a: Option[i64] = Some(3);\n    if let Some(x) = a and x > 0 {\n        println(f\"{x}\");\n    }\n}\n",
        "E0001",
        "`if let` Chains",
    );
}

#[test]
fn variance_marker_refusal_names_its_section() {
    assert_names(
        "variance",
        "struct Cell2[+T] {\n    v: T,\n}\nfn main() {}\n",
        "E_VARIANCE_USER_DECL_NOT_YET",
        "Declared Variance Markers",
    );
}

#[test]
fn variadic_parameter_refusal_names_its_section() {
    assert_names(
        "varargs",
        "fn sum(xs: ..i64) -> i64 {\n    0\n}\nfn main() {}\n",
        "E0001",
        "Homogeneous Varargs",
    );
}

#[test]
fn call_site_spread_refusal_names_its_section() {
    assert_names(
        "spread",
        "fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\nfn main() {\n    let t = (1, 2);\n    let x = add(..t);\n}\n",
        "E0202",
        "Call-Site Spread",
    );
}

#[test]
fn bitfield_width_refusal_names_its_section() {
    assert_names(
        "bitfield",
        "#[repr(C, bitfield)]\nstruct R {\n    a: u1,\n    b: u7,\n}\nfn main() {}\n",
        "E0104",
        "Bitfield Types",
    );
}

#[test]
fn exits_effect_refusal_names_its_section() {
    assert_names(
        "exits",
        "fn f() with exits {\n}\nfn main() {}\n",
        "E0403",
        "Process Exit as an Effect (`exits`)",
    );
}

#[test]
fn effect_variable_bound_refusal_names_its_section() {
    assert_names(
        "effect_bound",
        "fn apply[with E: no writes(Stdout)](f: Fn() with E) with E {\n    f();\n}\nfn main() {}\n",
        "E0002",
        "Effect Variable Bounds (`with E: no writes(R)`)",
    );
}

#[test]
fn wrapping_block_refusal_names_its_section() {
    assert_names(
        "wrapping",
        "fn main() {\n    let x: i64 = 1;\n    #[wrapping] {\n        println(f\"{x + 1}\");\n    }\n}\n",
        "E_ATTRIBUTE_NOT_VALID_ON_STATEMENT",
        "Scoped `#[wrapping]` / `#[checked]` Arithmetic Regions",
    );
}

#[test]
fn non_ascii_identifier_refusal_names_its_section() {
    assert_names(
        "non_ascii",
        "fn main() {\n    let café = 1;\n    println(f\"{café}\");\n}\n",
        "E0001",
        "Non-ASCII Identifiers",
    );
}

#[test]
fn a_wrong_count_call_without_spread_keeps_its_plain_message() {
    let out = check(
        "plain_arity",
        "fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\nfn main() {\n    let x = add(1);\n}\n",
    );
    assert!(out.contains("expected 2 argument(s), found 1"), "{out}");
    assert!(!out.contains("Call-Site Spread"), "{out}");
}

#[test]
fn volatile_cell_refusal_names_its_section() {
    // A `####` heading, under § Systems, so not `assert_names`' `###` probe.
    assert!(DEFERRED_MD.contains("\n#### Volatile Memory Access (MMIO)\n"));
    let out = check(
        "volatile_cell",
        "fn main() {\n    let mut r: VolatileCell[i64] = VolatileCell.new(1);\n    r.write(2);\n    println(f\"{r.read()}\");\n}\n",
    );
    assert!(out.contains("error[E0520]"), "{out}");
    assert!(
        out.contains("docs/deferred.md § Volatile Memory Access (MMIO)"),
        "{out}"
    );
}
