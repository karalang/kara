//! B-2026-09-20-27 -- the displaced element's user `Drop` body runs at a
//! NESTED store (`d[i][j] = x`), on both backends.

use super::*;

const PRE: &str = "struct S { s: String, k: i64 }
impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.k}:{self.s.len()}\") } }
struct N { k: i64 }
impl Drop for N { fn drop(mut ref self) { println(f\"dN{self.k}\") } }
struct D { id: i64 }
impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }
struct H { xs: Vec[Vec[S]] }
struct H1 { xs: Vec[S] }
enum E { A(N), B }
";

/// B-2026-09-20-27 — `d[0][0] = S { .. }` over `Vec[Vec[S]]` printed only the
/// survivor's body on all four surfaces: the interpreter's index-assign
/// displacement blocks took only a bare-name or `name.field` container, and
/// codegen's nested arm held its bodies back to agree. Both move together, so
/// every cell here reads the same on the interpreter and compiled. `control:`
/// cells were right before.
#[test]
fn nested_store_runs_the_displaced_body() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "vec-vec-struct",
            "fn main() { let n = 7; let mut d: Vec[Vec[S]] = [[S { s: f\"one-{n}\", k: 1 }]]; d[0][0] = S { s: f\"replaced-{n}\", k: 2 }; println(\"end\") }
",
            "dS1:5\ndS2:10\nend\n",
        ),
        (
            "no-heap-element",
            "fn main() { let n = 7; let mut d: Vec[Vec[N]] = [[N { k: 1 }]]; d[0][0] = N { k: 2 }; println(\"end\") }
",
            "dN1\ndN2\nend\n",
        ),
        (
            "array-inner",
            "fn main() { let n = 7; let mut d: Vec[Vec[Array[D, 1]]] = [[[D { id: 5 }]]]; d[0][0] = [D { id: 10 }]; println(\"end\") }
",
            "dD5\ndD10\nend\n",
        ),
        (
            "depth-3",
            "fn main() { let n = 7; let mut d: Vec[Vec[Vec[N]]] = [[[N { k: 1 }]]]; d[0][0][0] = N { k: 2 }; println(\"end\") }
",
            "dN1\ndN2\nend\n",
        ),
        (
            "field-root",
            "fn main() { let n = 7; let mut h = H { xs: [[S { s: f\"one-{n}\", k: 1 }]] }; h.xs[0][0] = S { s: f\"replaced-{n}\", k: 2 }; println(\"end\") }
",
            "dS1:5\ndS2:10\nend\n",
        ),
        (
            "field-under-an-index",
            "fn main() { let n = 7; let mut d: Vec[H1] = [H1 { xs: [S { s: f\"one-{n}\", k: 1 }] }]; d[0].xs[0] = S { s: f\"replaced-{n}\", k: 2 }; println(\"end\") }
",
            "dS1:5\ndS2:10\nend\n",
        ),
        (
            "array-outer",
            "fn main() { let n = 7; let mut d: Array[Vec[N], 1] = [[N { k: 1 }]]; d[0][0] = N { k: 2 }; println(\"end\") }
",
            "dN1\ndN2\nend\n",
        ),
        (
            "array-array",
            "fn main() { let n = 7; let mut d: Array[Array[N, 1], 1] = [[N { k: 1 }]]; d[0][0] = N { k: 2 }; println(\"end\") }
",
            "dN1\ndN2\nend\n",
        ),
        (
            "enum-element",
            "fn main() { let n = 7; let mut d: Vec[Vec[E]] = [[E.A(N { k: 1 })]]; d[0][0] = E.A(N { k: 2 }); println(\"end\") }
",
            "dN1\ndN2\nend\n",
        ),
        (
            "tuple-element",
            "fn main() { let n = 7; let mut d: Vec[Vec[(N, i64)]] = [[(N { k: 1 }, 0)]]; d[0][0] = (N { k: 2 }, 0); println(\"end\") }
",
            "dN1\ndN2\nend\n",
        ),
        (
            "in-a-loop",
            "fn main() { let n = 7; let mut d: Vec[Vec[N]] = [[N { k: 1 }, N { k: 2 }]]; for i in 0..2 { d[0][i] = N { k: 10 + i }; }; println(\"end\") }
",
            "dN1\ndN2\ndN10\ndN11\nend\n",
        ),
        (
            "stored-twice",
            "fn main() { let n = 7; let mut d: Vec[Vec[N]] = [[N { k: 1 }]]; d[0][0] = N { k: 2 }; d[0][0] = N { k: 3 }; println(\"end\") }
",
            "dN1\ndN2\ndN3\nend\n",
        ),
        (
            "variable-indexes",
            "fn main() { let n = 7; let mut d: Vec[Vec[N]] = [[N { k: 1 }, N { k: 2 }]]; let i = 1; let j = 0; d[j][i] = N { k: 3 }; println(\"end\") }
",
            "dN2\ndN1\ndN3\nend\n",
        ),
        (
            "mut-ref-param",
            "fn setit(d: mut ref Vec[Vec[N]]) { d[0][0] = N { k: 2 }; }
fn main() { let mut d: Vec[Vec[N]] = [[N { k: 1 }]]; setit(mut d); println(\"end\") }
",
            "dN1\ndN2\nend\n",
        ),
        (
            "self-field-root",
            "struct W { xs: Vec[Vec[N]] }
impl W { fn set(mut ref self) { self.xs[0][0] = N { k: 2 }; } }
fn main() { let n = 7; let mut w = W { xs: [[N { k: 1 }]] }; w.set(); println(\"end\") }
",
            "dN1\ndN2\nend\n",
        ),
        (
            "value-from-another-row",
            "fn main() { let n = 7; let mut d: Vec[Vec[N]] = [[N { k: 1 }], [N { k: 2 }]]; let t = d[1].pop(); match t { Some(x) => { d[0][0] = x; }, None => {} }; println(\"end\") }
",
            "dN1\ndN2\nend\n",
        ),
        (
            "control:single-level",
            "fn main() { let n = 7; let mut a: Vec[S] = [S { s: f\"one-{n}\", k: 1 }]; a[0] = S { s: f\"replaced-{n}\", k: 2 }; println(\"end\") }
",
            "dS1:5\ndS2:10\nend\n",
        ),
        (
            "control:swap-relocates",
            "fn main() { let n = 7; let mut d: Vec[Vec[N]] = [[N { k: 1 }, N { k: 2 }]]; d[0].swap(0, 1); println(\"end\") }
",
            "dN2\ndN1\nend\n",
        ),
    ];
    for (label, body, want) in cells {
        let prog = format!("{PRE}{body}");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&prog);
        assert!(
            interp_errs.is_empty(),
            "[{label}] interp errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want, "[{label}] interpreter");
        let Some(aot) = run_program(&prog) else {
            continue;
        };
        assert_eq!(aot, *want, "[{label}] AOT");
    }
}
