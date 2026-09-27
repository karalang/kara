//! B-2026-09-17-16 -- a whole TUPLE payload taken out of an `Option`/`Result`
//! (`Some(t) => { let u = t; .. }`, `let Some(t) = o else { .. }`) runs its
//! element `Drop` bodies once and frees its leaves.

use super::*;

/// B-2026-09-17-16 — `Some(t) => { let u = t; .. }` and
/// `let Some(t) = o else { .. }` over
/// `Option[(R, R)]` printed no body on the JIT, `-O0` and `-O2` against
/// `--interp`'s `dR1 dR2`, and the `let ... else` spelling also leaked every
/// heap leaf (4 B in 2 blocks). A tuple payload has no type name, so the bind
/// site funded nothing for `t` once the place handed its walk over. Covers
/// `if let`, `Result` on both sides, a loop, two matches over one place, a
/// guard, `while let`, a hand-back, a `Drop` leaf beside a scalar, a heap-free
/// `Drop` leaf, and `let ... else` over a by-value param.
#[test]
fn e2e_taken_tuple_payload_runs_element_bodies_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { id: i64 }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" } }
fn mko(i: i64) -> Option[(R, R)] { return Some((mk(i), mk(i + 1))) }
fn rebind() { let o = mko(1); match o { Some(t) => { let u = t; println(f"u{u.1.id}") } None => {} } }
fn iflet() { let o = mko(3); if let Some(t) = o { let u = t; println(f"i{u.0.id}") } }
fn letelse() -> i64 { let o = mko(5); let Some(t) = o else { return 0 }; println(f"h{t.1.id}"); return 1 }
fn letelse_rebind() -> i64 { let o = mko(7); let Some(t) = o else { return 0 }; let u = t; println(f"k{u.0.id}"); return 1 }
fn letelse_mixed() -> i64 { let o: Option[(R, i64)] = Some((mk(9), 4)); let Some(t) = o else { return 0 }; println(f"m{t.1}"); return 1 }
fn letelse_plain() -> i64 { let o: Option[(W, i64)] = Some((W { id: 10 }, 5)); let Some(t) = o else { return 0 }; println(f"p{t.1}"); return 1 }
fn letelse_param(o: Option[(R, R)]) -> i64 { let Some(t) = o else { return 0 }; println(f"q{t.1.id}"); return 1 }
fn res() -> i64 { let r: Result[i64, (R, R)] = Err((mk(13), mk(14))); let Err(t) = r else { return 0 }; println(f"e{t.0.id}"); return 1 }
fn res_match() { let r: Result[(R, R), i64] = Ok((mk(15), mk(16))); match r { Ok(t) => { let u = t; println(f"o{u.1.id}") } Err(e) => {} } }
fn looped() { for i in 0..2 { let o = mko(20 + i * 2); match o { Some(t) => { let u = t; println(f"l{u.1.id}") } None => {} } } }
fn twice() { let o = mko(30); match o { Some(t) => { println(f"a{t.0.id}") } None => {} } println("mid"); match o { Some(t) => { let u = t; println(f"b{u.1.id}") } None => {} } }
fn wlet() { let mut o = mko(32); while let Some(t) = o { let u = t; println(f"w{u.0.id}"); o = None; } }
fn guarded() { let o = mko(34); match o { Some(t) if t.0.id > 0 => { let u = t; println(f"g{u.0.id}") } Some(t) => { println("other") } None => {} } }
fn handback() -> (R, R) { let o = mko(36); let Some(t) = o else { return (mk(0), mk(0)) }; println("got"); return t }
fn main() {
    rebind(); println("--");
    iflet(); println("--");
    println(f"v{letelse()}"); println("--");
    println(f"v{letelse_rebind()}"); println("--");
    println(f"v{letelse_mixed()}"); println("--");
    println(f"v{letelse_plain()}"); println("--");
    println(f"v{letelse_param(mko(11))}"); println("--");
    println(f"v{res()}"); println("--");
    res_match(); println("--");
    looped(); println("--");
    twice(); println("--");
    wlet(); println("--");
    guarded(); println("--");
    let h = handback(); println(f"hb{h.1.id}"); println("--");
    println("end");
}
"#,
    ) else {
        return;
    };
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        [
            "u2", "dR1", "dR2", "--", "i3", "dR3", "dR4", "--", "h6", "dR5", "dR6", "v1", "--",
            "k7", "dR7", "dR8", "v1", "--", "m4", "dR9", "v1", "--", "p5", "dW10", "v1", "--",
            "q12", "dR11", "dR12", "v1", "--", "e13", "dR13", "dR14", "v1", "--", "o16", "dR15",
            "dR16", "--", "l21", "dR20", "dR21", "l23", "dR22", "dR23", "--", "a30", "mid", "b31",
            "dR30", "dR31", "--", "w32", "dR32", "dR33", "--", "g34", "dR34", "dR35", "--", "got",
            "hb37", "dR36", "dR37", "--", "end"
        ],
        "got:\n{out}"
    );
}
