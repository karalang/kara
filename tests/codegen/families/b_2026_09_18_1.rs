//! B-2026-09-18-1 -- a match arm that CONSUMES a heap-boxed generic enum
//! payload bound out of a by-value param (`Full(r) => { let z = r; .. }` over
//! `Ho[W]`) runs the payload's `Drop` body once.

use super::*;

/// B-2026-09-18-1 — `fn c1(x: Ho[W]) { match x { Full(r) => { let z = r; .. } .. } }`
/// over `struct W { a: String, b: String, c: String }` with its own `Drop`. The
/// cells: fresh-temp and named arguments (c1), a consume on one path with a read
/// on the other (c2, both paths), a rebind handed back as the tail (c3), a
/// rebind of a rebind (c4), a two-parameter declaration (c5), and the unused
/// rebind (c10), beside controls that were already right: the read-only arm
/// (c6), the monomorphic twin (c7), an inline payload (c8) and a consume through
/// a call (c9).
///
/// Before: the bodies mask took the payload's body off the param's generic
/// walker, while the arm's binding stayed a param VIEW (memory only). c1, c2 on
/// both paths, c4 and c10 ran no `W` body on any compiled surface.
///
/// c10 runs `dW7` BEFORE `c10` here and after it under `--interp`. The unused `z`
/// dies at its `let` on this backend, as design.md § 866 places it; the
/// interpreter runs a by-value param's parts at the call's end
/// (B-2026-09-17-35). Every other cell agrees.
#[test]
fn e2e_generic_boxed_payload_consuming_arm_runs_body() {
    let Some(out) = run_program(
        r#"struct W { a: String, b: String, c: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.a.len()}") } }
fn mkw(n: i64) -> W { return W { a: f"a{n}", b: f"bbb{1}", c: f"ccc{1}" }; }
struct V { id: i64 }
impl Drop for V { fn drop(mut ref self) { println(f"dV{self.id}") } }
enum Ho[T] { Full(T), Empty }
enum HoW { Full(W), Empty }
enum P2[A, B] { Two(A, B), Nil }

fn c1(x: Ho[W]) { match x { Full(r) => { let z = r; println(f"c1:{z.a.len()}") } Empty => { println("e") } } }
fn c2(x: Ho[W], f: bool) { match x { Full(r) => { if f { let z = r; println(f"c2t:{z.a.len()}") } else { println(f"c2f:{r.a.len()}") } } Empty => { println("e") } } }
fn c3(x: Ho[W]) -> W { match x { Full(r) => { let z = r; println("c3"); z } Empty => { mkw(0) } } }
fn c4(x: Ho[W]) { match x { Full(r) => { let z = r; let y = z; println(f"c4:{y.a.len()}") } Empty => { println("e") } } }
fn c5(x: P2[W, i64]) { match x { Two(r, n) => { let z = r; println(f"c5:{z.a.len()}/{n}") } Nil => { println("e") } } }
fn c6(x: Ho[W]) { match x { Full(r) => { println(f"c6:{r.a.len()}") } Empty => { println("e") } } }
fn c7(x: HoW) { match x { Full(r) => { let z = r; println(f"c7:{z.a.len()}") } Empty => { println("e") } } }
fn c8(x: Ho[V]) { match x { Full(r) => { let z = r; println(f"c8:{z.id}") } Empty => { println("e") } } }
fn sink(w: W) { println(f"sink{w.a.len()}") }
fn c9(x: Ho[W]) { match x { Full(r) => { sink(r); println("c9") } Empty => { println("e") } } }
fn c10(x: Ho[W]) { match x { Full(r) => { let z = r; println("c10") } Empty => { println("e") } } }

fn main() {
    c1(Ho.Full(mkw(1))); println("-1");
    let h2 = Ho.Full(mkw(22)); c1(h2); println("-2");
    c2(Ho.Full(mkw(333)), true); println("-3");
    c2(Ho.Full(mkw(4444)), false); println("-4");
    let w5 = c3(Ho.Full(mkw(55555))); println(f"got{w5.a.len()}"); println("-5");
    c4(Ho.Full(mkw(6))); println("-6");
    c5(P2.Two(mkw(77), 7)); println("-7");
    c6(Ho.Full(mkw(888))); println("-8");
    c7(HoW.Full(mkw(9999))); println("-9");
    c8(Ho.Full(V { id: 10 })); println("-10");
    c9(Ho.Full(mkw(11111))); println("-11");
    c10(Ho.Full(mkw(121212))); println("-12");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        vec![
            "c1:2", "dW2", "-1", "c1:3", "dW3", "-2", "c2t:4", "dW4", "-3", "c2f:5", "dW5", "-4",
            "c3", "got6", "dW6", "-5", "c4:2", "dW2", "-6", "c5:3/7", "dW3", "-7", "c6:4", "dW4",
            "-8", "c7:5", "dW5", "-9", "c8:10", "dV10", "-10", "sink6", "c9", "dW6", "-11", "dW7",
            "c10", "-12", "end",
        ],
        "got:\n{out}"
    );
}
