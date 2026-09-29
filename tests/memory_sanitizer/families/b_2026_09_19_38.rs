//! B-2026-09-19-38 -- a `shared enum`'s unit variant constructed in an
//! unbound position has an owner, and its `Drop` body runs once.

use super::*;

/// B-2026-09-19-38 — a `shared enum`'s unit variant built where nothing binds
/// it: an argument (by value, `ref`, bare `A`, a method's, in a loop), a
/// `match` scrutinee, and a discarded statement (`U.A;`, `B;`, `let _ = U.B`),
/// beside the payload-constructor and call-result discards (`U.C(5);`,
/// `mku();`) that shared the discard gap.
///
/// Before: compiled, every one of those boxes was stranded (16-24 B each, one
/// per loop iteration) and its `Drop` body ran nowhere, while `--interp` ran it
/// everywhere except the scrutinee, where no backend ran it.
#[test]
fn asan_unbound_shared_unit_variant_is_released_clean() {
    assert_clean_asan_run(
        r#"shared enum U { A, B, C(i64) }
impl Drop for U { fn drop(mut ref self) { println("dU") } }
shared enum V { X, Y }
struct H { u: U }
struct Obj { n: i64 }
impl Obj {
    fn eat(ref self, u: U) -> i64 { match u { U.A => 1, _ => 2 } }
    fn look(ref self, u: ref U) -> i64 { match u { U.A => 1, _ => 2 } }
}
fn take(u: U) -> i64 { match u { U.A => { 1 } U.B => { 2 } U.C(n) => { n } } }
fn rtake(u: ref U) -> i64 { match u { U.A => { 1 } U.B => { 2 } U.C(n) => { n } } }
fn keep(u: U) -> U { u }
fn tv(v: V) -> i64 { match v { V.X => 1, V.Y => 2 } }
fn mku() -> U { U.C(3) }
fn main() {
    println("arg"); let x = take(U.A); println(f"  {x}");
    println("refarg"); let y = rtake(U.B); println(f"  {y}");
    println("bare"); let z = take(A); println(f"  {z}");
    println("loop"); let mut i = 0; let mut t = 0; while i < 5 { t = t + tv(V.X) + take(U.B); i = i + 1; } println(f"  {t}");
    println("method"); let o = Obj { n: 1 }; let a = o.eat(U.A); println(f"  {a}");
    println("mref"); let b = o.look(U.B); println(f"  {b}");
    println("scrut"); let r = match U.A { U.A => 1, _ => 2 }; println(f"  {r}");
    println("scrutbare"); let r2 = match B { U.A => 1, _ => 2 }; println(f"  {r2}");
    println("scrutnodrop"); let r3 = match V.Y { V.X => 1, V.Y => 2 }; println(f"  {r3}");
    println("discard"); U.A; println("  after");
    println("discardbare"); B; println("  after");
    println("letdisc"); let _ = U.B; println("  after");
    println("ctor"); U.C(5); println("  after");
    println("call"); mku(); println("  after");
    println("keep"); let k = keep(U.B); println("  kept");
    println("keepdisc"); keep(U.A); println("  after");
    println("nested"); let n = take(keep(U.A)); println(f"  {n}");
    println("field"); let h = H { u: U.B }; println(f"  {take(h.u)}");
    println("end")
}"#,
        &[
            "arg",
            "dU",
            "  1",
            "refarg",
            "dU",
            "  2",
            "bare",
            "dU",
            "  1",
            "loop",
            "dU",
            "dU",
            "dU",
            "dU",
            "dU",
            "  15",
            "method",
            "dU",
            "  1",
            "mref",
            "dU",
            "  2",
            "scrut",
            "dU",
            "  1",
            "scrutbare",
            "dU",
            "  2",
            "scrutnodrop",
            "  2",
            "discard",
            "dU",
            "  after",
            "discardbare",
            "dU",
            "  after",
            "letdisc",
            "dU",
            "  after",
            "ctor",
            "dU",
            "  after",
            "call",
            "dU",
            "  after",
            "keep",
            "dU",
            "  kept",
            "keepdisc",
            "dU",
            "  after",
            "nested",
            "dU",
            "  1",
            "field",
            "  2",
            "dU",
            "end",
        ],
        "unbound_shared_unit_variant",
    );
}
