//! design.md §5 pattern rules the §5 gap audit found the compiler short of
//! (HTTP thread, 2026-11-24): `x @ (K.A | K.B)` over a non-`Copy` enum, two
//! named range bounds of different types, a bare `..` arm, and a call in a
//! range bound. Each cell also lives in conformance/design/types.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], src: &str, tag: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-d5-patterns-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn refused(tag: &str, src: &str, want: &[&str]) {
    let out = karac(&["check"], src, tag);
    let all = text(&out);
    assert!(!out.status.success(), "{tag}: check accepted it:\n{all}");
    for w in want {
        assert!(all.contains(w), "{tag}: no {w:?} in:\n{all}");
    }
}

/// A unit variant under `@` binds nothing, so it claims nothing from the
/// whole that `x` takes (design.md § `@` bindings). The MIR pipeline's
/// typecheck reported `K.A` as a second owned binding.
#[test]
fn d5_at_binding_over_unit_variants_runs() {
    let src = "enum K { A, B, C }\nfn main() {\n    let v = K.B;\n    \
               let k = match v { x @ (K.A | K.B) => \"ab\", K.C => \"c\" };\n    \
               let j = match K.C { y @ K.C => \"c\", _ => \"other\" };\n    \
               println(f\"{k} {j}\");\n}\n";
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, src, "at");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "ab c\n",
            "{lane:?}: {}",
            text(&out)
        );
    }
}

/// The same-type rule applies after named bounds resolve, to each
/// constant's declared type (design.md § Range patterns).
#[test]
fn d5_named_range_bounds_of_different_types_refused() {
    refused(
        "bounds",
        "const LO: i32 = 0;\nconst HI: i64 = 9;\nfn main() { let n: i64 = 3; \
         match n { LO..=HI => println(\"a\"), _ => println(\"b\") } }\n",
        &["range pattern bounds must have the same type; got 'i32' and 'i64'"],
    );
}

#[test]
fn d5_named_range_bounds_of_one_type_run() {
    let src = "const LO: i32 = 18;\nconst HI: i32 = 65;\nfn g(a: i32) -> Str { \
               match a { ..LO => \"minor\", LO..=HI => \"adult\", _ => \"retired\" } }\n\
               fn main() { println(f\"{g(3)} {g(18)} {g(70)}\"); }\n";
    let out = karac(&["run", "--interp"], src, "bounds-ok");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "minor adult retired\n",
        "{}",
        text(&out)
    );
}

#[test]
fn d5_bare_rest_marker_arm_names_the_wildcard() {
    refused(
        "bare-rest",
        "fn main() { let n = 3; match n { .. => println(\"any\") } }\n",
        &["a bare `..` is not a pattern on its own", "write `_`"],
    );
}

#[test]
fn d5_call_in_a_range_bound_is_the_bound_error() {
    let src = "fn hi() -> i64 { 9 }\nfn main() { let n = 3; \
               match n { 0..=hi() => println(\"a\"), _ => println(\"b\") } }\n";
    let out = karac(&["check"], src, "call-bound");
    let all = text(&out);
    assert!(!out.status.success(), "{all}");
    assert!(all.contains("E_RANGE_PATTERN_BOUND_NOT_SIMPLE"), "{all}");
    assert!(!all.contains("arrow function"), "{all}");
}
