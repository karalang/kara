//! B-2026-09-20-33 -- a payload bound out of a by-value GENERIC enum param
//! runs its `Drop` body exactly once, moved out or only read.

use super::*;

const PRE: &str = "struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }
struct Ch { r: R, s: String }
struct Cn { r: R, n: i64 }
struct D { id: i64 }
impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }
enum Gen[T] { A(T), B }
enum G2[T] { A(T, i64), B }
enum Ech { A(Ch), B }
struct Wn { a: R }
struct Wd { a: R, b: String, c: String }
fn sink(c: Ch) { println(\"sink\") }
fn sinkn(c: Cn) { println(\"sink\") }
";

/// B-2026-09-20-33 — a payload moved out of a by-value GENERIC enum param
/// (`match b { Gen.A(w) => { o = w; } .. }`) lost its `Drop` body on every
/// compiled surface while the concrete twin was right: the arm binding stayed
/// a param VIEW after the consuming arm masked the param's own boxed-payload
/// walker, so `o = w` disarmed `o` on the premise that someone else fires.
/// The read-only twin ran the body twice, at the arm's end and at the param's
/// walker. `control:` cells were right before.
#[test]
fn generic_enum_param_payload_runs_its_body_once() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "heap-payload-moved-out",
            "fn g(b: Gen[Ch], base: i64) -> i64 { let mut o: Ch = Ch { r: R { id: base }, s: f\"OUT\" }; match b { Gen.A(w) => { o = w; } Gen.B => { } }; println(\"ret\"); return o.r.id }
fn main() { println(f\"r:{g(Gen.A(Ch { r: R { id: 22 }, s: f\"IN\" }), 21)}\") }
",
            "dR21\nret\ndR22\nr:22\n",
        ),
        (
            "scalar-payload-moved-out",
            "fn g(b: Gen[Cn]) -> i64 { let mut o = Cn { r: R { id: 9 }, n: 0 }; match b { Gen.A(w) => { o = w; } Gen.B => { } }; println(\"ret\"); o.n }
fn main() { println(f\"r:{g(Gen.A(Cn { r: R { id: 4 }, n: 1 }))}\") }
",
            "dR9\nret\ndR4\nr:1\n",
        ),
        (
            "named-argument",
            "fn g(b: Gen[Cn]) -> i64 { let mut o = Cn { r: R { id: 9 }, n: 0 }; match b { Gen.A(w) => { o = w; } Gen.B => { } }; println(\"ret\"); o.n }
fn main() { let a = Gen.A(Cn { r: R { id: 7 }, n: 1 }); let r = g(a); println(f\"r:{r}\") }
",
            "dR9\nret\ndR7\nr:1\n",
        ),
        (
            "named-heap-argument",
            "fn g(b: Gen[Ch]) -> i64 { let mut o = Ch { r: R { id: 9 }, s: f\"o\" }; match b { Gen.A(w) => { o = w; } Gen.B => { } }; println(\"ret\"); o.r.id }
fn main() { let a = Gen.A(Ch { r: R { id: 8 }, s: f\"p\" }); let r = g(a); println(f\"r:{r}\") }
",
            "dR9\nret\ndR8\nr:8\n",
        ),
        (
            "if-let",
            "fn g(b: Gen[Cn]) -> i64 { let mut o = Cn { r: R { id: 9 }, n: 0 }; if let Gen.A(w) = b { o = w; }; println(\"ret\"); o.n }
fn main() { println(f\"r:{g(Gen.A(Cn { r: R { id: 2 }, n: 1 }))}\") }
",
            "dR9\nret\ndR2\nr:1\n",
        ),
        (
            "let-else",
            "fn g(b: Gen[Cn]) -> i64 { let mut o = Cn { r: R { id: 9 }, n: 0 }; let Gen.A(w) = b else { return 0 }; o = w; println(\"ret\"); o.n }
fn main() { println(f\"r:{g(Gen.A(Cn { r: R { id: 3 }, n: 1 }))}\") }
",
            "dR9\nret\ndR3\nr:1\n",
        ),
        (
            "two-field-variant",
            "fn g(b: G2[Cn]) -> i64 { let mut o = Cn { r: R { id: 9 }, n: 0 }; match b { G2.A(w, k) => { o = w; } G2.B => { } }; println(\"ret\"); o.n }
fn main() { println(f\"r:{g(G2.A(Cn { r: R { id: 7 }, n: 1 }, 3))}\") }
",
            "dR9\nret\ndR7\nr:1\n",
        ),
        (
            "in-a-loop",
            "fn g(b: Gen[Cn]) -> i64 { let mut o = Cn { r: R { id: 9 }, n: 0 }; match b { Gen.A(w) => { o = w; } Gen.B => { } }; println(\"ret\"); o.n }
fn main() { let mut k = 0; while k < 2 { println(f\"r:{g(Gen.A(Cn { r: R { id: 30 + k }, n: 1 }))}\"); k = k + 1; } }
",
            "dR9\nret\ndR30\nr:1\ndR9\nret\ndR31\nr:1\n",
        ),
        (
            "read-only-arm",
            "fn g(b: Gen[Cn]) -> i64 { match b { Gen.A(w) => { println(f\"rd{w.r.id}\"); } Gen.B => { } }; println(\"ret\"); 1 }
fn main() { println(f\"r:{g(Gen.A(Cn { r: R { id: 1 }, n: 1 }))}\") }
",
            "rd1\nret\ndR1\nr:1\n",
        ),
        (
            "read-only-scalar-field",
            "fn g(b: Gen[Cn]) -> i64 { match b { Gen.A(w) => { println(f\"rd{w.n}\"); } Gen.B => { } }; println(\"ret\"); 1 }
fn main() { println(f\"r:{g(Gen.A(Cn { r: R { id: 2 }, n: 1 }))}\") }
",
            "rd1\nret\ndR2\nr:1\n",
        ),
        (
            "unused-binding",
            "fn g(b: Gen[Cn]) -> i64 { match b { Gen.A(w) => { println(\"rd\"); } Gen.B => { } }; println(\"ret\"); 1 }
fn main() { println(f\"r:{g(Gen.A(Cn { r: R { id: 3 }, n: 1 }))}\") }
",
            "rd\nret\ndR3\nr:1\n",
        ),
        (
            "two-field-read-only",
            "fn g(b: G2[Cn]) -> i64 { match b { G2.A(w, k) => { println(f\"rd{w.n}{k}\"); } G2.B => { } }; println(\"ret\"); 1 }
fn main() { println(f\"r:{g(G2.A(Cn { r: R { id: 8 }, n: 1 }, 3))}\") }
",
            "rd13\nret\ndR8\nr:1\n",
        ),
        (
            "passed-on-scalar",
            "fn g(b: Gen[Cn]) -> i64 { match b { Gen.A(w) => { sinkn(w); } Gen.B => { } }; println(\"ret\"); 1 }
fn main() { println(f\"r:{g(Gen.A(Cn { r: R { id: 3 }, n: 1 }))}\") }
",
            "sink\nret\ndR3\nr:1\n",
        ),
        (
            "passed-on-heap",
            "fn g(b: Gen[Ch]) -> i64 { match b { Gen.A(w) => { sink(w); } Gen.B => { } }; println(\"ret\"); 1 }
fn main() { println(f\"r:{g(Gen.A(Ch { r: R { id: 2 }, s: f\"p\" }))}\") }
",
            "sink\nret\ndR2\nr:1\n",
        ),
        (
            "narrow-read-only-arm",
            "fn g(b: Gen[Wn]) -> i64 { match b { Gen.A(w) => { println(f\"rd{w.a.id}\"); } Gen.B => { } }; println(\"ret\"); 1 }
fn main() { let r = g(Gen.A(Wn { a: R { id: 41 } })); println(f\"r:{r}\") }
",
            "rd41\nret\ndR41\nr:1\n",
        ),
        (
            "narrow-named-argument",
            "fn g(b: Gen[Wn]) { match b { Gen.A(w) => { println(f\"rd{w.a.id}\") } Gen.B => { println(\"none\") } } }
fn main() { { let a: Gen[Wn] = Gen.A(Wn { a: R { id: 42 } }); g(a) }; println(\"end\") }
",
            "rd42\ndR42\nend\n",
        ),
        (
            "narrow-rebind",
            "fn g(b: Gen[Wn]) -> i64 { match b { Gen.A(w) => { let x = w; println(f\"lt{x.a.id}\"); } Gen.B => { } }; println(\"ret\"); 1 }
fn main() { let r = g(Gen.A(Wn { a: R { id: 43 } })); println(f\"r:{r}\") }
",
            "lt43\nret\ndR43\nr:1\n",
        ),
        (
            "narrow-passed-on",
            "fn sinkw(w: Wn) { println(\"sink\") }
fn g(b: Gen[Wn]) -> i64 { match b { Gen.A(w) => { sinkw(w); } Gen.B => { } }; println(\"ret\"); 1 }
fn main() { let r = g(Gen.A(Wn { a: R { id: 44 } })); println(f\"r:{r}\") }
",
            "sink\nret\ndR44\nr:1\n",
        ),
        (
            "narrow-if-let",
            "fn g(b: Gen[Wn]) -> i64 { if let Gen.A(w) = b { println(f\"il{w.a.id}\") }; println(\"ret\"); 1 }
fn main() { let r = g(Gen.A(Wn { a: R { id: 45 } })); println(f\"r:{r}\") }
",
            "il45\nret\ndR45\nr:1\n",
        ),
        (
            "control:narrow-moved-out",
            "fn g(b: Gen[Wn]) -> i64 { let mut o = Wn { a: R { id: 9 } }; match b { Gen.A(w) => { o = w; } Gen.B => { } }; println(\"ret\"); o.a.id }
fn main() { let r = g(Gen.A(Wn { a: R { id: 46 } })); println(f\"r:{r}\") }
",
            "dR9\nret\ndR46\nr:46\n",
        ),
        (
            "control:b-2026-09-20-47-wide-nested",
            "fn g(b: Gen[Wd]) { match b { Gen.A(v) => { println(f\"db {v.a.id}/{v.b}\") } Gen.B => { println(\"none\") } } }
fn main() { { let a: Gen[Wd] = Gen.A(Wd { a: R { id: 47 }, b: f\"b\", c: f\"c\" }); g(a) }; println(\"end\") }
",
            "db 47/b\ndR47\nend\n",
        ),
        (
            "control:own-drop-payload",
            "fn g(b: Gen[D]) -> i64 { let mut o = D { id: 9 }; match b { Gen.A(w) => { o = w; } Gen.B => { } }; println(\"ret\"); o.id }
fn main() { println(f\"r:{g(Gen.A(D { id: 5 }))}\") }
",
            "dD9\nret\ndD5\nr:5\n",
        ),
        (
            "control:heap-read-only-arm",
            "fn g(b: Gen[Ch]) -> i64 { match b { Gen.A(w) => { println(f\"rd{w.r.id}\"); } Gen.B => { } }; println(\"ret\"); 1 }
fn main() { println(f\"r:{g(Gen.A(Ch { r: R { id: 11 }, s: f\"p\" }))}\") }
",
            "rd11\nret\ndR11\nr:1\n",
        ),
        (
            "control:concrete-twin",
            "fn g(b: Ech, base: i64) -> i64 { let mut o: Ch = Ch { r: R { id: base }, s: f\"OUT\" }; match b { Ech.A(w) => { o = w; } Ech.B => { } }; println(\"ret\"); return o.r.id }
fn main() { println(f\"r:{g(Ech.A(Ch { r: R { id: 12 }, s: f\"IN\" }), 11)}\") }
",
            "dR11\nret\ndR12\nr:12\n",
        ),
        (
            "control:variant-without-payload",
            "fn g(b: Gen[Cn]) -> i64 { let mut o = Cn { r: R { id: 9 }, n: 0 }; match b { Gen.A(w) => { o = w; } Gen.B => { } }; println(\"ret\"); o.n }
fn main() { println(f\"r:{g(Gen.B)}\") }
",
            "ret\ndR9\nr:0\n",
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
