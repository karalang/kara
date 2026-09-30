//! B-2026-09-29-104 — a NESTED struct pattern over a generic struct was
//! judged against the field's DECLARED type parameter, so
//! `let G { v: G { v, n }, n: m } = g` over `g: G[G[String]]` was refused as
//! refutable (and `G { v: true, .. }` / `G { v: false, .. }` over `G[bool]` as
//! non-exhaustive). Once accepted, codegen registered the nested leaf at the
//! declared `T` too, and `v.len()` failed with "no handler for method 'len'".

use super::*;

/// B-2026-09-29-104 — nested `let` destructures of `G[G[String]]`,
/// `G[G[G[String]]]` (the leaf handed back, named and fresh) and
/// `G[G[Vec[i64]]]`, a nested parameter pattern over `G[G[i64]]`, and a
/// `bool` match over `G[bool]` that is exhaustive only once `T` is `bool`.
#[test]
fn e2e_nested_generic_struct_pattern_irrefutable_and_typed_concretely() {
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
    let want =
        "36 4 -4\n6\ndeep-heap-string-longer-than-sso-2\nfresh-heap-string-longer-than-sso-3\n7\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(src) {
        assert_eq!(aot, want, "AOT");
    }
}
