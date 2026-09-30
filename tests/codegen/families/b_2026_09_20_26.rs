//! B-2026-09-20-26 -- a user enum's `Array` / `Vec` payload rebound into a
//! local inside the arm runs each element's `Drop` body once.

use super::*;

const PRE: &str = "struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }
fn mka() -> Array[R, 2] { return [R { id: 1 }, R { id: 2 }] }
fn mkv() -> Vec[R] { let mut v: Vec[R] = Vec.new(); v.push(R { id: 1 }); v.push(R { id: 2 }); return v }
enum EArr { A(Array[R, 2]), B }
enum EVec { V(Vec[R]), B }
";

/// B-2026-09-20-26 — `match e { EArr.A(v) => { let u = v; .. } }` over a
/// let-bound `enum EArr { A(Array[R, 2]), B }` printed `dR1 dR2 dR1 dR2` on all
/// four surfaces, and the `Vec` spelling doubled on the interpreter. Both
/// backends asked whether the payload runs a body by its HEAD name, and
/// `Array` / `Vec` is no user type, so the arm's move left the scrutinee's
/// payload walk armed beside the rebinding's own. `control:` cells were right
/// before the fix and must stay so.
#[test]
fn rebound_container_payload_runs_each_body_once() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "let-bound-array",
            "fn main() { let e: EArr = EArr.A(mka()); match e { EArr.A(v) => { let u = v; println(f\"au{u[0].id}\") } EArr.B => { println(\"no\") } } println(\"end\") }
",
            "au1\ndR1\ndR2\nend\n",
        ),
        (
            "let-bound-vec",
            "fn main() { let e: EVec = EVec.V(mkv()); match e { EVec.V(v) => { let u = v; println(f\"au{u[0].id}\") } EVec.B => { println(\"no\") } } println(\"end\") }
",
            "au1\ndR1\ndR2\nend\n",
        ),
        (
            "if-let",
            "fn main() { let e: EArr = EArr.A(mka()); if let EArr.A(v) = e { let u = v; println(\"in\") } println(\"end\") }
",
            "dR1\ndR2\nin\nend\n",
        ),
        (
            "while-let",
            "fn main() { let e: EArr = EArr.A(mka()); while let EArr.A(v) = e { let u = v; println(\"in\"); break } println(\"end\") }
",
            "dR1\ndR2\nin\nend\n",
        ),
        (
            "struct-field-scrutinee",
            "struct S { e: EArr, k: i64 }
fn main() { let s = S { e: EArr.A(mka()), k: 1 }; match s.e { EArr.A(v) => { let u = v; println(f\"au{u[0].id}\") } EArr.B => { println(\"no\") } } println(\"end\") }
",
            "au1\ndR1\ndR2\nend\n",
        ),
        (
            "array-beside-a-scalar",
            "enum E2 { A(Array[R, 2], i64), B }
fn main() { let e = E2.A(mka(), 7); match e { E2.A(v, k) => { let u = v; println(f\"au{u[0].id} {k}\") } E2.B => { println(\"no\") } } println(\"end\") }
",
            "au1 7\ndR1\ndR2\nend\n",
        ),
        (
            "rebound-twice",
            "fn main() { let e: EArr = EArr.A(mka()); match e { EArr.A(v) => { let u = v; let w = u; println(f\"w{w[1].id}\") } EArr.B => { println(\"no\") } } println(\"end\") }
",
            "w2\ndR1\ndR2\nend\n",
        ),
        (
            "after-a-guarded-arm",
            "fn main() { let e: EArr = EArr.A(mka()); let c = 3; match e { EArr.A(v) if c > 5 => { println(\"g\") } EArr.A(v) => { let u = v; println(f\"au{u[0].id}\") } EArr.B => { println(\"no\") } } println(\"end\") }
",
            "au1\ndR1\ndR2\nend\n",
        ),
        (
            "in-a-loop",
            "fn main() { let mut i = 0; while i < 2 { let e: EArr = if i == 0 { EArr.A(mka()) } else { EArr.B }; match e { EArr.A(v) => { let u = v; println(f\"au{u[0].id}\") } EArr.B => { println(\"no\") } } i = i + 1; } println(\"end\") }
",
            "au1\ndR1\ndR2\nno\nend\n",
        ),
        (
            "yielded-by-the-match",
            "fn main() { let e: EArr = EArr.A(mka()); let w: Array[R, 2] = match e { EArr.A(v) => v, EArr.B => mka() }; println(f\"w{w[0].id}\") ; println(\"end\") }
",
            "w1\ndR1\ndR2\nend\n",
        ),
        (
            "vec-elements",
            "enum Ec { A(Array[Vec[R], 2]), B }
fn main() {
    let e = Ec.A([mkv(), mkv()]); match e { Ec.A(v) => { let u = v; println(\"mv\") } Ec.B => { println(\"no\") } }
    let f = Ec.A([mkv(), mkv()]); match f { Ec.A(v) => { println(\"rd\") } Ec.B => { println(\"no\") } }
    println(\"end\")
}
",
            "dR1\ndR2\ndR1\ndR2\nmv\nrd\ndR1\ndR2\ndR1\ndR2\nend\n",
        ),
        (
            "control:read-only-arm",
            "fn main() { let e: EArr = EArr.A(mka()); match e { EArr.A(v) => { println(f\"r{v[0].id}\") } EArr.B => { println(\"no\") } } println(\"end\") }
",
            "r1\ndR1\ndR2\nend\n",
        ),
        (
            "control:unused-binding",
            "fn main() { let e: EArr = EArr.A(mka()); match e { EArr.A(v) => { println(\"x\") } EArr.B => { println(\"no\") } } println(\"end\") }
",
            "x\ndR1\ndR2\nend\n",
        ),
        (
            "control:moved-into-a-call",
            "fn eat(x: Array[R, 2]) { println(f\"ate{x[0].id}\") }
fn main() { let e: EArr = EArr.A(mka()); match e { EArr.A(v) => { eat(v) } EArr.B => { println(\"no\") } } println(\"end\") }
",
            "ate1\ndR1\ndR2\nend\n",
        ),
        (
            "control:by-value-param",
            "fn f(e: EArr) { match e { EArr.A(v) => { let u = v; println(f\"au{u[0].id}\") } EArr.B => { println(\"no\") } } }
fn main() { f(EArr.A(mka())); println(\"end\") }
",
            "au1\ndR1\ndR2\nend\n",
        ),
        (
            "control:generic-enum",
            "enum Slot[T] { S(T), N }
fn main() { let e: Slot[Array[R, 2]] = Slot.S(mka()); match e { Slot.S(v) => { let u = v; println(f\"au{u[0].id}\") } Slot.N => { println(\"no\") } } println(\"end\") }
",
            "au1\ndR1\ndR2\nend\n",
        ),
        (
            "control:struct-field-pattern",
            "struct H { a: Array[R, 2] }
fn main() { let e: H = H { a: mka() }; match e { H { a } => { let u = a; println(\"in\") } } println(\"end\") }
",
            "dR1\ndR2\nin\nend\n",
        ),
        (
            "control:nested-array-read-only",
            "enum En { A(Array[Array[R, 1], 2]), B }
fn main() { let e = En.A([[R { id: 1 }], [R { id: 2 }]]); match e { En.A(v) => { println(\"au\") } En.B => { println(\"no\") } } println(\"end\") }
",
            "au\ndR1\ndR2\nend\n",
        ),
        (
            "control:projection-read-only",
            "struct Hp { e: EArr, n: i64 }
fn main() { let h = Hp { e: EArr.A(mka()), n: 0 }; match h.e { EArr.A(v) => println(f\"r{v[1].id}\"), EArr.B => println(\"b\") } println(\"x\") }
",
            "r2\ndR1\ndR2\nx\n",
        ),
        (
            "control:struct-variant-read-only",
            "enum Es { A { v: Array[R, 2], k: i64 }, B }
fn main() { let e = Es.A { v: mka(), k: 3 }; match e { Es.A { v, k } => println(f\"k{k} {v[0].id}\"), Es.B => println(\"b\") } println(\"x\") }
",
            "k3 1\ndR1\ndR2\nx\n",
        ),
        (
            "control:local-wrapping-a-param",
            "fn f(a: Array[R, 2]) -> i64 { let o = EArr.A(a); match o { EArr.A(v) => { println(\"r\"); return 1 }, EArr.B => { return 0 } } }
fn main() { let a = mka(); let z = f(a); println(\"x\") }
",
            "r\ndR1\ndR2\nx\n",
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
