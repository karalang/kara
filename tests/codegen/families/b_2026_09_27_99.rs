//! B-2026-09-27-99 — a `Drop` field of a by-value struct PARAM that the
//! callee hands on to another owner (`keep(w.r)`, a push into a local `Vec`
//! or `Map`), and a field a callee returns out of a call-result argument.

use super::*;

/// B-2026-09-27-99 — a param's field handed to a callee that keeps it
/// (`let k = keep(w.r)`, `keep(w.s);`, inside a tuple, two hops down, through a
/// method and a generic callee) or pushed into a container the function
/// declares (`xs.push(w.r)`, the alias spelling, `m.insert(w.b, w.r)`) runs its
/// body once, for a named and a call-result argument alike; and so does a field
/// returned out of a call-result argument (`retr(mkw(100))`). The part scan
/// reported only an ALIAS handed to a keeping callee, so the caller's walk ran
/// the part's body again, and codegen's call-result argument walk was not
/// masked by the parts the callee hands on at all.
#[test]
fn e2e_param_field_handed_to_another_owner_runs_its_body_once() {
    let src = r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"d{self.id}{self.name}") } }
fn mkd(n: i64) -> D { D { id: n, name: f"n{n}" } }
struct W { r: D, s: D, b: i64 }
struct X { w: W, t: D }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 1), b: n } }
fn keep(d: D) -> D { d }
fn retr(w: W) -> D { w.r }
struct H { k: i64 }
impl H { fn f(ref self, w: W) -> i64 { let k = keep(w.r); k.id } }
fn gk[T](w: W, t: T) -> i64 { let k = keep(w.r); k.id }
fn keepf(w: W) -> i64 { let k = keep(w.r); k.id }
fn keeps(w: W) -> i64 { keep(w.s); w.b }
fn tup(w: W) -> i64 { let t = (keep(w.r), 5); t.1 }
fn pushf(w: W) -> i64 { let mut xs: Vec[D] = Vec.new(); xs.push(w.r); xs.len() }
fn aliasp(w: W) -> i64 { let r = w.r; let mut xs: Vec[D] = Vec.new(); xs.push(r); xs.len() }
fn ins(w: W) -> i64 { let mut m: Map[i64, D] = Map.new(); m.insert(w.b, w.r); m.len() }
fn hop2(x: X) -> i64 { let k = keep(x.w.r); k.id }
fn main() {
    println(f"a{keepf(mkw(10))}");
    println(f"b{keeps(mkw(20))}");
    println(f"c{tup(mkw(30))}");
    println(f"e{pushf(mkw(40))}");
    println(f"f{aliasp(mkw(50))}");
    println(f"g{ins(mkw(60))}");
    println(f"h{hop2(X { w: mkw(70), t: mkd(79) })}");
    let w = mkw(80);
    println(f"i{keepf(w)}");
    let v = mkw(90);
    println(f"j{pushf(v)}");
    let d = retr(mkw(100));
    println(f"k{d.id}");
    let h = H { k: 0 };
    println(f"l{h.f(mkw(110))}");
    println(f"m{gk(mkw(120), 1)}");
    println("end")
}"#;
    let want = "d10n10\nd11n11\na10\nd21n21\nd20n20\nb20\nd30n30\nd31n31\nc5\nd40n40\nd41n41\ne1\nd50n50\nd51n51\nf1\nd60n60\nd61n61\ng1\nd70n70\nd79n79\nd71n71\nh70\nd80n80\ni80\nd81n81\nd90n90\nj1\nd91n91\nd101n101\nk100\nd100n100\nd110n110\nd111n111\nl110\nd120n120\nd121n121\nm120\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
