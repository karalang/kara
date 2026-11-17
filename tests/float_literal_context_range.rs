//! B-2026-11-16-340: an unsuffixed float literal is range-checked against
//! the type its context gives it (design.md §3), as `1e39f32` is in the
//! lexer. Each context that narrows a literal is one cell.

use karac::check_source;

/// The error messages `karac check` reports for `src`.
fn errors(src: &str) -> Vec<String> {
    check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect()
}

fn refused(body: &str, lit: &str, ty: &str) {
    let src = format!("fn take(x: f32) -> f32 {{\n    x\n}}\nfn main() {{\n{body}}}\n");
    let es = errors(&src);
    assert_eq!(es.len(), 1, "{body}: {es:#?}");
    assert!(
        es[0].contains(&format!(
            "float literal `{lit}` is out of range for '{ty}', the type its context gives it"
        )),
        "{body}: {}",
        es[0]
    );
}

#[test]
fn a_narrowing_context_range_checks_an_unsuffixed_float_literal() {
    refused(
        "    let a: f32 = 1e39;\n    println(f\"{a}\");\n",
        "1e39",
        "f32",
    );
    refused(
        "    let a: f32 = -1e39;\n    println(f\"{a}\");\n",
        "1e39",
        "f32",
    );
    refused(
        "    let b = take(1e39);\n    println(f\"{b}\");\n",
        "1e39",
        "f32",
    );
    refused(
        "    let c = 1e39;\n    let d: f32 = c;\n    println(f\"{d}\");\n",
        "1e39",
        "f32",
    );
    refused(
        "    let v: Vec[f32] = [1e39];\n    println(f\"{v[0]}\");\n",
        "1e39",
        "f32",
    );
    refused(
        "    let e: f16 = 70000.0;\n    println(f\"{e}\");\n",
        "70000.0",
        "f16",
    );
    refused(
        "    let g: bf16 = 3.4e38;\n    println(f\"{g}\");\n",
        "3.4e38",
        "bf16",
    );
}

#[test]
fn a_literal_that_rounds_to_the_largest_finite_value_is_accepted() {
    for body in [
        "    let f: f16 = 65519.0;\n    println(f\"{f}\");\n",
        "    let a: f32 = 3.4028235e38;\n    println(f\"{a}\");\n",
        "    let w = 1e39;\n    println(f\"{w}\");\n",
        "    let i: f32 = f32.INFINITY;\n    println(f\"{i}\");\n",
    ] {
        let src = format!("fn main() {{\n{body}}}\n");
        assert!(errors(&src).is_empty(), "{body}: {:#?}", errors(&src));
    }
}
