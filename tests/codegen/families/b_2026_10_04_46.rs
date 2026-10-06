//! B-2026-10-04-46 — a tuple never released an `Option[shared]` element: a
//! local, a by-value param, a struct field, a `Vec` element or a loop element
//! holding `(Option[H], i64)` leaked the handle and lost its `Drop` body, and
//! the reads that compensated for that (`unwrap` releasing a ref it never
//! took, a move-out zeroing nothing) became double frees once anything did.

use super::*;

/// B-2026-10-04-46 — every position that reads, moves or borrows such an
/// element, where both backends agree.
#[test]
fn e2e_tuple_option_shared_element_is_released_once() {
    let src = r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
struct W { t: (Option[H], i64) }
struct Wo { o: Option[H] }
fn ct(t: (Option[H], i64)) { println(f"t{t.0.unwrap().id} {t.1}") }
fn ct2(t: (Option[H], i64)) { println(f"t{t.1}") }
fn cr(t: ref (Option[H], i64)) { println(f"r{t.0.unwrap().id}") }
fn mk(i: i64) -> (Option[H], i64) { (Some(H { id: i }), i) }
fn give(t: (Option[H], i64)) -> Option[H] { t.0 }
fn cx(t: (Option[H], i64)) -> i64 { let x = t.0; x.unwrap().id }
fn tko(o: Option[H]) { println(f"k{o.unwrap().id}") }
fn mkh() -> Option[H] { let p = mk(1); p.0 }
enum Et { A((Option[H], i64)), B }
fn main() {
    { let p = mk(1); println(f"a{p.1}"); }
    { let p = (Some(H { id: 2 }), 2); println(f"b{p.0.unwrap().id}"); println(f"b{p.0.unwrap().id}"); }
    { let p = mk(3); ct(p); println("c"); }
    { let p = mk(4); let x = p.0; let y = p.0; println(f"d{x.unwrap().id} {y.unwrap().id}"); }
    { let p = mk(5); println(f"e{cx(p)}"); }
    { let x = { let p = mk(6); p.0 }; println(f"f{x.unwrap().id}"); }
    { let x = mkh(); println(f"g{x.unwrap().id}"); }
    { let p = mk(8); let q = p; println(f"h{q.1}"); }
    { let p = mk(9); match p.0 { Some(h) => println(f"i{h.id}"), None => {} } }
    { let p = mk(10); if let Some(h) = p.0 { println(f"j{h.id}") } }
    { let mut v: Vec[(Option[H], i64)] = Vec.new(); v.push(mk(11)); println(f"k{v.len()}"); }
    { let p = mk(12); let o = give(p); println(f"l{o.unwrap().id}"); }
    { let h = H { id: 13 }; let p = (Some(h), 2); println(f"m{h.id} {p.1}"); }
    { let p = mk(14); let z = p.0.unwrap(); println(f"n{z.id}"); }
    { let p = mk(15); tko(p.0); println("o"); }
    { let p = mk(16); let w = Wo { o: p.0 }; println(f"p{w.o.unwrap().id}"); }
    { let p = mk(17); let t2 = (p.0, 5); println(f"q{t2.1}"); }
    { let p = mk(18); let mut x: Option[H] = None; x = p.0; println(f"r{x.unwrap().id}"); }
    { let v = [mk(19), mk(20)]; for t in v { println(f"s{t.0.unwrap().id}") } }
    { let o = Some(mk(21)); match o { Some(t) => println(f"t{t.0.unwrap().id}"), None => {} } }
    { let p = mk(22); cr(p); cr(p); println(f"u{p.1}"); }
    println("end")
}"#;
    let want = "a1\ndH1\nb2\nb2\ndH2\nt3 3\nc\ndH3\nd4 4\ndH4\ne5\ndH5\nf6\ndH6\ng1\ndH1\nh8\ndH8\ni9\ndH9\nj10\ndH10\nk1\ndH11\nl12\ndH12\nm13 2\ndH13\nn14\ndH14\nk15\no\ndH15\np16\ndH16\nq5\ndH17\nr18\ndH18\ns19\ns20\ndH19\ndH20\nt21\ndH21\nr22\nr22\nu22\ndH22\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-10-04-46 — the positions the interpreter does not agree on yet, so
/// only the compiled answer is pinned. `a`, `b` and `c` pass a fresh tuple by
/// value or discard one (B-2026-09-27-92); `d` is a closure capture and `e` a
/// displaced element. The interpreter loses the captured body in `d`
/// (B-2026-10-04-68's closure half); `e` agrees since B-2026-10-04-68.
#[test]
fn e2e_tuple_option_shared_element_is_released_once_compiled_only() {
    let src = r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
struct W { t: (Option[H], i64) }
struct Wo { o: Option[H] }
fn ct(t: (Option[H], i64)) { println(f"t{t.0.unwrap().id} {t.1}") }
fn ct2(t: (Option[H], i64)) { println(f"t{t.1}") }
fn cr(t: ref (Option[H], i64)) { println(f"r{t.0.unwrap().id}") }
fn mk(i: i64) -> (Option[H], i64) { (Some(H { id: i }), i) }
fn give(t: (Option[H], i64)) -> Option[H] { t.0 }
fn cx(t: (Option[H], i64)) -> i64 { let x = t.0; x.unwrap().id }
fn tko(o: Option[H]) { println(f"k{o.unwrap().id}") }
fn mkh() -> Option[H] { let p = mk(1); p.0 }
enum Et { A((Option[H], i64)), B }
fn main() {
    { ct((Some(H { id: 1 }), 2)); println("a"); }
    { ct2((Some(H { id: 2 }), 2)); println("b"); }
    { mk(3); println("c"); }
    { let p = mk(4); let f = || p.1; println(f"d{f()}"); }
    { let mut p = mk(5); p.0 = Some(H { id: 6 }); println(f"e{p.1}"); }
    { let w = W { t: mk(7) }; println(f"f{w.t.0.unwrap().id}"); println(f"f{w.t.1}"); }
    println("end")
}"#;
    let want = "t1 2\ndH1\na\nt2\ndH2\nb\ndH3\nc\nd4\ndH4\ndH5\ne5\ndH6\nf7\nf7\ndH7\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(
        interp_out.join(""),
        "t1 2\na\nt2\nb\nc\nd4\ndH5\ne5\ndH6\nf7\nf7\ndH7\nend\n",
        "interpreter"
    );
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
