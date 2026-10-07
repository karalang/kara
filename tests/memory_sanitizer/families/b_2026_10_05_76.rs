//! B-2026-10-05-76: discarded generic call over a struct-literal argument

use super::*;

/// B-2026-10-05-76: a DISCARDED call to a generic callee that hands its by-value
/// param back (`g(R { id: 1 });` over `fn g[T](o: T) -> T { o }`) with a STRUCT
/// LITERAL argument. The caller stands its argument down because the result
/// carries it, and the discard registrar, which owns that result, admitted a
/// call temporary and a named local but not a literal, so compiled the body ran
/// nowhere. Cells cover `let _ =`, a struct with a heap field, a two-param
/// callee, an alias return, and the named, call-temporary and bound controls.
#[test]
fn asan_discarded_generic_handback_of_struct_literal_runs_its_body() {
    assert_clean_asan_run(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mks(i: i64) -> S { S { r: R { id: i }, s: f"ssssssssssssssssssssssssssss{i}" } }
fn g[T](o: T) -> T { o }
fn g2[T](s: T, o: T) -> T { o }
fn g3[T](o: T) -> T { let t = o; t }
fn main() {
    g(R { id: 1 }); println("_a1");
    let _ = g(R { id: 2 }); println("_a2");
    g(S { r: R { id: 3 }, s: "sssssssssssssssssssssssssssss" }); println("_a3");
    g2(R { id: 4 }, R { id: 5 }); println("_a4");
    g3(R { id: 6 }); println("_a5");
    let a = R { id: 7 }; g(a); println("_a6");
    g(mks(8)); println("_a7");
    let r = g(R { id: 9 }); println(f"k{r.id}"); println("_a8");
    println("end")
}
"#,
        &[
            "d1", "_a1", "d2", "_a2", "d3", "_a3", "d4", "d5", "_a4", "d6", "_a5", "d7", "_a6",
            "d8", "_a7", "k9", "d9", "_a8", "end",
        ],
        "asan_discarded_generic_handback_of_struct_literal_runs_its_body",
    );
}
