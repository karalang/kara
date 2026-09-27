//! B-2026-09-27-96 — a boxed generic enum param handed back on some paths and pushed, matched or forwarded on the others.

use super::*;

/// B-2026-09-27-96 — `fn pkv(h: Ho[S], k: bool, w: mut ref Vec[Ho[S]]) -> Ho[S]
/// { if k { return h } w.push(h); return Ho.Empty }` and its by-value `match`
/// twin. A param handed back on SOME paths now belongs to the callee, which
/// registers it like a param it never returns, and the caller moves it in on
/// every call; before, the caller kept it and the push or consuming match on
/// the other path freed it a second time. Free, instance-method and
/// associated spellings, each leg, plus fresh-temp arguments and the
/// forwarding leg `pkf`, whose `Drop` body `--interp` used to lose.
#[test]
fn e2e_boxed_generic_enum_param_handed_back_on_some_paths_pushed_or_matched() {
    let Some(out) = run_program(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
enum Ho[T] { Full(T), Empty }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
fn shows(h: Ho[S]) { match h { Ho.Full(r) => println(f"s{r.id}"), Ho.Empty => println("e") } }
fn pkf(h: Ho[S], k: bool) -> Ho[S] { if k { return h } shows(h); return Ho.Empty }
fn pkv(h: Ho[S], k: bool, w: mut ref Vec[Ho[S]]) -> Ho[S] { if k { return h } w.push(h); return Ho.Empty }
fn pkm(h: Ho[S], k: bool) -> Ho[S] { if k { return h } match h { Ho.Full(s) => println(f"m{s.id}"), Ho.Empty => println("me") } return Ho.Empty }
struct K { z: i64 }
impl K {
    fn mpv(ref self, h: Ho[S], k: bool, w: mut ref Vec[Ho[S]]) -> Ho[S] { if k { return h } w.push(h); return Ho.Empty }
    fn mpm(ref self, h: Ho[S], k: bool) -> Ho[S] { if k { return h } match h { Ho.Full(s) => println(f"n{s.id}"), Ho.Empty => println("ne") } return Ho.Empty }
    fn apm(h: Ho[S], k: bool) -> Ho[S] { if k { return h } match h { Ho.Full(s) => println(f"a{s.id}"), Ho.Empty => println("ae") } return Ho.Empty }
    fn apv(h: Ho[S], k: bool, w: mut ref Vec[Ho[S]]) -> Ho[S] { if k { return h } w.push(h); return Ho.Empty }
}
fn main() {
    let kk = K { z: 0 };
    let mut w: Vec[Ho[S]] = Vec.new();
    let a1: Ho[S] = Ho.Full(mks(1)); let r1 = pkf(a1, false); shows(r1);
    let a2: Ho[S] = Ho.Full(mks(2)); let r2 = pkf(a2, true); shows(r2);
    let a3: Ho[S] = Ho.Full(mks(3)); let r3 = pkv(a3, false, mut w); shows(r3);
    let a4: Ho[S] = Ho.Full(mks(4)); let r4 = pkv(a4, true, mut w); shows(r4);
    let a5: Ho[S] = Ho.Full(mks(5)); let r5 = pkm(a5, false); shows(r5);
    let a6: Ho[S] = Ho.Full(mks(6)); let r6 = pkm(a6, true); shows(r6);
    let r7 = pkv(Ho.Full(mks(7)), false, mut w); shows(r7);
    let r8 = pkm(Ho.Full(mks(8)), false); shows(r8);
    let b1: Ho[S] = Ho.Full(mks(11)); let q1 = kk.mpv(b1, false, mut w); shows(q1);
    let b2: Ho[S] = Ho.Full(mks(12)); let q2 = kk.mpv(b2, true, mut w); shows(q2);
    let b3: Ho[S] = Ho.Full(mks(13)); let q3 = kk.mpm(b3, false); shows(q3);
    let b4: Ho[S] = Ho.Full(mks(14)); let q4 = kk.mpm(b4, true); shows(q4);
    let b5: Ho[S] = Ho.Full(mks(15)); let q5 = K.apm(b5, false); shows(q5);
    let b6: Ho[S] = Ho.Full(mks(16)); let q6 = K.apm(b6, true); shows(q6);
    let b7: Ho[S] = Ho.Full(mks(17)); let q7 = K.apv(b7, false, mut w); shows(q7);
    let b8: Ho[S] = Ho.Full(mks(18)); let q8 = K.apv(b8, true, mut w); shows(q8);
    println(f"n{w.len()}");
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "s1\ndS1\ne\ns2\ndS2\ne\ns4\ndS4\nm5\ndS5\ne\ns6\ndS6\ne\nm8\ndS8\ne\ne\ns12\ndS12\nn13\ndS13\ne\ns14\ndS14\na15\ndS15\ne\ns16\ndS16\ne\ns18\ndS18\nn4\ndS3\ndS7\ndS11\ndS17\nend\n", "got:\n{out}");
}

/// B-2026-09-27-96 — the `String`-payload spelling of the same three callees,
/// both legs, from a loop, with the pushed elements drained at the end.
#[test]
fn e2e_boxed_generic_enum_string_param_handed_back_on_some_paths_in_loop() {
    let Some(out) = run_program(
        r#"enum Hs[T] { Full(T), Empty }
fn sh(h: Hs[String]) { match h { Hs.Full(r) => println(r), Hs.Empty => println("e") } }
fn pf(h: Hs[String], k: bool) -> Hs[String] { if k { return h } sh(h); return Hs.Empty }
fn pv(h: Hs[String], k: bool, w: mut ref Vec[Hs[String]]) -> Hs[String] { if k { return h } w.push(h); return Hs.Empty }
fn pm(h: Hs[String], k: bool) -> Hs[String] { if k { return h } match h { Hs.Full(s) => println(f"m{s}"), Hs.Empty => println("me") } return Hs.Empty }
fn main() {
    let mut w: Vec[Hs[String]] = Vec.new();
    let mut i = 0;
    while i < 3 {
        let a: Hs[String] = Hs.Full(f"p{i}" + "x"); let r = pf(a, i == 1); sh(r);
        let b: Hs[String] = Hs.Full(f"v{i}" + "x"); let q = pv(b, i == 1, mut w); sh(q);
        let c: Hs[String] = Hs.Full(f"q{i}" + "x"); let t = pm(c, i == 1); sh(t);
        i = i + 1;
    }
    for x in w { sh(x) }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out, "p0x\ne\ne\nmq0x\ne\np1x\nv1x\nq1x\np2x\ne\ne\nmq2x\ne\nv0x\nv2x\nend\n",
        "got:\n{out}"
    );
}
