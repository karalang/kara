//! B-2026-09-29-104 — a NESTED struct pattern over a generic struct was
//! judged against the field's DECLARED type parameter, so
//! `let G { v: G { v, n }, n: m } = g` over `g: G[G[String]]` was refused as
//! refutable (and `G { v: true, .. }` / `G { v: false, .. }` over `G[bool]` as
//! non-exhaustive). Once accepted, codegen registered the nested leaf at the
//! declared `T` too, and `v.len()` failed with "no handler for method 'len'".

/// B-2026-09-29-104 — the checked interpreter run of the codegen fixture's
/// program (`run` skips the typechecker, whose refusal was the defect).
#[test]
fn interp_nested_generic_struct_pattern_irrefutable_and_typed_concretely() {
    let src = r#"struct G[T] { v: T, n: i64 }
fn mk(p: String, k: i64) -> String {
    f"{p}-heap-string-longer-than-sso-{k}"
}
fn f4(g: G[G[String]]) -> i64 { let G { v: G { v, n }, n: m } = g; v.len() + n + m }
fn f5(g: G[bool]) -> i64 { match g { G { v: true, n } => n, G { v: false, n } => 0 - n } }
fn f7(G { v: G { v, n }, n: m }: G[G[i64]]) -> i64 { v + n + m }
fn f8(g: G[G[G[String]]]) -> String { let G { v: G { v: G { v, .. }, .. }, .. } = g; v }
fn f9(g: G[G[Vec[i64]]]) -> i64 { let G { v: G { v, n }, .. } = g; v.len() + n }
fn main() {
    let x = G { v: G { v: mk("x", 1), n: 2 }, n: 3 };
    println(f"{f4(x)} {f5(G { v: true, n: 4 })} {f5(G { v: false, n: 4 })}");
    println(f"{f7(G { v: G { v: 1, n: 2 }, n: 3 })}");
    let y = G { v: G { v: G { v: mk("deep", 2), n: 1 }, n: 2 }, n: 3 };
    println(f8(y));
    println(f8(G { v: G { v: G { v: mk("fresh", 3), n: 1 }, n: 2 }, n: 3 }));
    let mut w = Vec.new(); w.push(7); w.push(8);
    println(f"{f9(G { v: G { v: w, n: 5 }, n: 6 })}");
}
"#;
    let (out, errs, _, _) = karac::run_program_full_checked(src);
    assert!(errs.is_empty(), "interp errored: {errs:?}");
    assert_eq!(
        out.join(""),
        "36 4 -4\n6\ndeep-heap-string-longer-than-sso-2\nfresh-heap-string-longer-than-sso-3\n7\n"
    );
}

fn type_errors(src: &str) -> Vec<String> {
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::desugar_program(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "resolve errors: {:?}",
        resolved.errors
    );
    karac::typecheck(&parsed.program, &resolved)
        .errors
        .iter()
        .map(|e| e.to_string())
        .collect()
}

/// B-2026-09-29-104 — substituting the instantiated types must not make a
/// pattern that CAN fail read as irrefutable or exhaustive: a `Some` leaf, a
/// `true` leaf one level down, a missing `false` arm, and a `Some` leaf inside
/// a generic enum's struct variant are all still refused.
#[test]
fn interp_nested_generic_struct_pattern_still_refutable_where_it_can_fail() {
    let cases = [
        (
            "fn f(g: G[Option[i64]]) -> i64 { let G { v: Some(x), n } = g; x + n }",
            "refutable pattern",
        ),
        (
            "fn f(g: G[G[bool]]) -> i64 { let G { v: G { v: true, n }, n: m } = g; n + m }",
            "refutable pattern",
        ),
        (
            "fn f(g: G[bool]) -> i64 { match g { G { v: true, n } => n } }",
            "non-exhaustive match",
        ),
        (
            "fn f(e: E[Option[i64]]) -> i64 { match e { E.A { x: Some(v), k } => v + k, E.B => 0 } }",
            "non-exhaustive match",
        ),
    ];
    for (f, want) in cases {
        let src = format!(
            "struct G[T] {{ v: T, n: i64 }}\nenum E[T] {{ A {{ x: T, k: i64 }}, B }}\n{f}\nfn main() {{ }}\n"
        );
        let errs = type_errors(&src);
        assert!(
            errs.iter().any(|e| e.contains(want)),
            "`{f}`: expected `{want}`, got {errs:?}"
        );
    }
}
