//! B-2026-09-27-131 — an enum or `Option` param rebound whole, then handed back on some exits only.

use super::*;

/// B-2026-09-27-131 — a by-value enum or `Option` param REBOUND whole
/// (`let m = h;`) and then returned on some exits only. The hand-back
/// registration declined a rebound user-enum param on both backends, and
/// codegen's `let` retracted a registered `Option` param's walker without
/// handing it on, so the exit that kept the value ran no `Drop` body: on
/// every surface for the inline enum, the boxed enum dropped in the callee
/// and the `Option`; under `--interp` alone for the boxed enum forwarded to
/// a consumer. Rebind before and after the branch, forwarded and dropped,
/// each leg.
#[test]
fn asan_enum_param_rebound_then_handed_back_on_some_paths() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
enum Ho[T] { Full(T), Empty }
fn shows(h: Ho[S]) { match h { Ho.Full(r) => println(f"s{r.id}"), Ho.Empty => println("e") } }
enum Hc { Full(S), Empty }
fn eat(h: Hc) { println("x") }
fn shc(h: Hc) { match h { Hc.Full(r) => println(f"s{r.id}"), Hc.Empty => println("e") } }
fn fwb(h: Ho[S], k: bool) -> Ho[S] { let m = h; if k { return m } shows(m); return Ho.Empty }
fn fwa(h: Ho[S], k: bool) -> Ho[S] { if k { return h } let g = h; shows(g); return Ho.Empty }
fn dpb(h: Ho[S], k: bool) -> Ho[S] { let m = h; if k { return m } println("db"); return Ho.Empty }
fn fwi(h: Hc, k: bool) -> Hc { let m = h; if k { return m } eat(m); return Hc.Empty }
fn fwj(h: Hc, k: bool) -> Hc { if k { return h } let g = h; eat(g); return Hc.Empty }
fn dpi(h: Hc, k: bool) -> Hc { let m = h; if k { return m } println("di"); return Hc.Empty }
fn dpo(h: Option[S], k: bool) -> Option[S] { let m = h; if k { return m } println("do"); return Option.None }
fn main() {
    let b1: Ho[S] = Ho.Full(mks(1)); let q1 = fwb(b1, false); shows(q1);
    let b2: Ho[S] = Ho.Full(mks(2)); let q2 = fwb(b2, true); shows(q2);
    let b3: Ho[S] = Ho.Full(mks(3)); let q3 = fwa(b3, false); shows(q3);
    let b4: Ho[S] = Ho.Full(mks(4)); let q4 = fwa(b4, true); shows(q4);
    let b5: Ho[S] = Ho.Full(mks(5)); let q5 = dpb(b5, false); shows(q5);
    let b6: Ho[S] = Ho.Full(mks(6)); let q6 = dpb(b6, true); shows(q6);
    let c7 = Hc.Full(mks(7)); let r7 = fwi(c7, false); shc(r7);
    let c8 = Hc.Full(mks(8)); let r8 = fwi(c8, true); shc(r8);
    let c9 = Hc.Full(mks(9)); let r9 = fwj(c9, false); shc(r9);
    let c10 = Hc.Full(mks(10)); let r10 = fwj(c10, true); shc(r10);
    let c11 = Hc.Full(mks(11)); let r11 = dpi(c11, false); shc(r11);
    let c12 = Hc.Full(mks(12)); let r12 = dpi(c12, true); shc(r12);
    let o13 = dpo(Option.Some(mks(13)), false); println(f"o{o13.is_some()}");
    let o14 = dpo(Option.Some(mks(14)), true); println(f"o{o14.is_some()}");
    println("end")
}
"#,
        &[
            "s1", "dS1", "e", "s2", "dS2", "s3", "dS3", "e", "s4", "dS4", "dS5", "db", "e", "s6",
            "dS6", "x", "dS7", "e", "s8", "dS8", "x", "dS9", "e", "s10", "dS10", "dS11", "di", "e",
            "s12", "dS12", "dS13", "do", "ofalse", "otrue", "dS14", "end",
        ],
        "asan_enum_param_rebound_then_handed_back_on_some_paths",
    );
}

/// B-2026-09-27-131 — the same rebind through an instance method, an
/// associated fn, a nested `let` inside the returning branch, a rebind of a
/// rebind, an `Option` wrapper around the rebound value, a `Result` param
/// and a generic fn.
#[test]
fn asan_enum_param_rebound_then_handed_back_other_spellings() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
enum Ho[T] { Full(T), Empty }
fn shows(h: Ho[S]) { match h { Ho.Full(r) => println(f"s{r.id}"), Ho.Empty => println("e") } }
enum Hc { Full(S), Empty }
fn eat(h: Hc) { println("x") }
fn shc(h: Hc) { match h { Hc.Full(r) => println(f"s{r.id}"), Hc.Empty => println("e") } }
struct K { z: i64 }
impl K {
    fn mp(ref self, h: Hc, k: bool) -> Hc { let m = h; if k { return m } shc(m); return Hc.Empty }
    fn ap(h: Ho[S], k: bool) -> Ho[S] { let m = h; if k { return m } println("ad"); return Ho.Empty }
}
fn nest(h: Hc, k: bool) -> Hc { if k { let m = h; return m } println("nd"); return Hc.Empty }
fn two(h: Hc, k: bool) -> Hc { let m = h; let n = m; if k { return n } eat(n); return Hc.Empty }
fn wrap(h: Hc, k: bool) -> Option[Hc] { let m = h; if k { return Option.Some(m) } println("wd"); return Option.None }
fn rr(h: Result[S, i64], k: bool) -> Result[S, i64] { let m = h; if k { return m } println("rd"); return Result.Err(0) }
fn gn[T](h: Ho[T], k: bool) -> Ho[T] { let m = h; if k { return m } println("gd"); return Ho.Empty }
fn main() {
    let kk = K { z: 0 };
    let a1 = kk.mp(Hc.Full(mks(1)), false); shc(a1);
    let a2 = kk.mp(Hc.Full(mks(2)), true); shc(a2);
    let b1 = K.ap(Ho.Full(mks(3)), false); shows(b1);
    let b2 = K.ap(Ho.Full(mks(4)), true); shows(b2);
    let c1 = nest(Hc.Full(mks(5)), false); shc(c1);
    let c2 = nest(Hc.Full(mks(6)), true); shc(c2);
    let d1 = two(Hc.Full(mks(7)), false); shc(d1);
    let d2 = two(Hc.Full(mks(8)), true); shc(d2);
    let e1 = wrap(Hc.Full(mks(9)), false); println(f"w{e1.is_some()}");
    let e2 = wrap(Hc.Full(mks(10)), true); println(f"w{e2.is_some()}");
    let f1 = rr(Result.Ok(mks(11)), false); println(f"r{f1.is_ok()}");
    let f2 = rr(Result.Ok(mks(12)), true); println(f"r{f2.is_ok()}");
    let g1 = gn(Ho.Full(mks(13)), false); shows(g1);
    let g2 = gn(Ho.Full(mks(14)), true); shows(g2);
    println("end")
}
"#,
        &[
            "s1", "dS1", "e", "s2", "dS2", "dS3", "ad", "e", "s4", "dS4", "nd", "dS5", "e", "s6",
            "dS6", "x", "dS7", "e", "s8", "dS8", "dS9", "wd", "wfalse", "wtrue", "dS10", "dS11",
            "rd", "rfalse", "rtrue", "dS12", "dS13", "gd", "e", "s14", "dS14", "end",
        ],
        "asan_enum_param_rebound_then_handed_back_other_spellings",
    );
}
