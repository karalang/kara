//! A format spec's width and precision are at most 65535, as in Rust, and a
//! larger one is a compile error at the hole: `{n:4294967296}` ended a
//! compiled program with "out of memory" (B-2026-11-17-1). Rust's alternate
//! form `#` is named as such, with the prefix to write (B-2026-11-17-2).

use karac::{run_playground, PlaygroundDiagnostic, PlaygroundResult};

fn errors(r: &PlaygroundResult) -> Vec<&PlaygroundDiagnostic> {
    r.diagnostics
        .iter()
        .filter(|d| d.severity == "error")
        .collect()
}

/// The program printing `f"[{hole}]"` with `n = 255` and `x = 1.5`.
fn program(hole: &str) -> String {
    format!("fn main() {{\n    let n = 255;\n    let x = 1.5;\n    println(f\"[{hole}]\");\n}}\n")
}

/// `hole` is refused with one error on line 4 whose message holds `msg`.
fn refused(hole: &str, msg: &str) {
    let r = run_playground(&program(hole));
    let errs = errors(&r);
    assert_eq!(errs.len(), 1, "{hole}\n{errs:#?}");
    assert!(errs[0].message.contains(msg), "{hole}\n{:#?}", errs[0]);
    assert_eq!(errs[0].line, 4, "{hole}\n{:#?}", errs[0]);
}

#[test]
fn format_width_and_precision_over_65535_are_refused() {
    for (hole, part) in [
        ("{n:65536}", "width `65536` is out of range"),
        ("{n:4294967296}", "width `4294967296` is out of range"),
        (
            "{n:>10000000000000000000}",
            "width `10000000000000000000` is out of range",
        ),
        ("{x:.65536}", "precision `65536` is out of range"),
    ] {
        refused(hole, part);
        refused(hole, "at most 65535, as in Rust");
    }
    let r = run_playground(&program("{n:65535}"));
    assert!(r.ok, "{:#?}", r.diagnostics);
    let line = r.stdout.concat();
    assert_eq!(line.len(), "[]\n".len() + 65535, "{}", &line[..20]);
    assert!(line.ends_with("255]\n"));
}

#[test]
fn format_alternate_form_is_named_with_the_prefix_to_write() {
    for (hole, hint) in [
        ("{n:#x}", "`0x{n:x}`"),
        ("{n:#X}", "`0x{n:X}`"),
        ("{n:#b}", "`0b{n:b}`"),
        ("{n:#o}", "`0o{n:o}`"),
        ("{n:+#010x}", "`0x{n:x}`"),
    ] {
        refused(hole, "the alternate form `#` is not supported in v1");
        refused(hole, hint);
    }
    let r = run_playground(&program("0x{n:x}"));
    assert_eq!(r.stdout.concat(), "[0xff]\n");
}
