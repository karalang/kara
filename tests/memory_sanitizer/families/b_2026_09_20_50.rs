//! B-2026-09-20-50 -- an enum returned by a call and handed onward to an owner
//! that outlives the call (returned, pushed into a `Vec`, stored in a field)
//! frees its orphaned original once.

use super::*;

/// B-2026-09-20-50 — the callee ENTRY-COPIES a by-value enum param, so a
/// call-returned temp handed onward leaves the caller an orphaned original.
/// The fresh-temp enum arm declined every escaping temp, so nothing freed it:
/// one payload buffer per call at -O0, 358 B in 13 blocks on the first
/// program here before the fix. Cells 12 and 13 (named local, inline
/// constructor) and cell 11 (a consuming callee) were already clean and guard
/// the double-free direction.
#[test]
fn asan_call_returned_enum_handed_onward_frees_its_original() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}"); } }
enum Ts { A(String), B }
enum Tr { A(R), B }
enum Tv { A(Vec[String]), B }
struct Box2 { t: Ts }
struct Bag { xs: Vec[Ts] }
impl Bag {
    fn put(mut ref self, e: Ts) { self.xs.push(e); }
    fn give(ref self, e: Ts) -> Ts { e }
    fn sgive(e: Ts) -> Ts { e }
}
fn mks(n: i64) -> Ts { Ts.A(f"payload-{n}-xxxxxxxxxxxxxxxxxxxx") }
fn mkr(n: i64) -> Tr { Tr.A(R { id: n, s: f"r-{n}" }) }
fn mkv(n: i64) -> Tv { Tv.A([f"v-{n}", f"w-{n}"]) }
fn gives(e: Ts) -> Ts { e }
fn giver(e: Tr) -> Tr { e }
fn givev(e: Tv) -> Tv { e }
fn puts(v: mut ref Vec[Ts], e: Ts) { v.push(e); }
fn putr(v: mut ref Vec[Tr], e: Tr) { v.push(e); }
fn holds(e: Ts) -> Box2 { Box2 { t: e } }
fn passg[T](e: T) -> T { e }
fn eat(e: Ts) -> i64 { 7 }
fn show(t: ref Ts) -> String { match t { Ts.A(s) => s.clone(), Ts.B => "b" } }
fn main() {
    let z1: Ts = gives(mks(1)); println(f"1:{show(z1)}");
    let z2: Tr = giver(mkr(2)); println("2");
    let z3: Tv = givev(mkv(3)); match z3 { Tv.A(xs) => println(f"3:{xs.len()}"), Tv.B => println("3b") }
    let mut v: Vec[Ts] = []; puts(mut v, mks(4)); println(f"4:{v.len()}");
    let mut w: Vec[Tr] = []; putr(mut w, mkr(5)); println(f"5:{w.len()}");
    let b: Box2 = holds(mks(6)); println(f"6:{show(b.t)}");
    let mut bag = Bag { xs: [] }; bag.put(mks(7)); println(f"7:{bag.xs.len()}");
    let z8: Ts = bag.give(mks(8)); println(f"8:{show(z8)}");
    let z9: Ts = Bag.sgive(mks(9)); println(f"9:{show(z9)}");
    let z10: Ts = passg(mks(10)); println(f"10:{show(z10)}");
    let n11 = eat(mks(11)); println(f"11:{n11}");
    let e12 = mks(12); let z12: Ts = gives(e12); println(f"12:{show(z12)}");
    let z13: Ts = gives(Ts.A(f"inline-{13}")); println(f"13:{show(z13)}");
    for i in 0..3 { let zz: Ts = gives(mks(20 + i)); println(f"L:{show(zz)}"); }
    println("end");
}
"#,
        &[
            "1:payload-1-xxxxxxxxxxxxxxxxxxxx",
            "dR2",
            "2",
            "3:2",
            "4:1",
            "5:1",
            "dR5",
            "6:payload-6-xxxxxxxxxxxxxxxxxxxx",
            "7:1",
            "8:payload-8-xxxxxxxxxxxxxxxxxxxx",
            "9:payload-9-xxxxxxxxxxxxxxxxxxxx",
            "10:payload-10-xxxxxxxxxxxxxxxxxxxx",
            "11:7",
            "12:payload-12-xxxxxxxxxxxxxxxxxxxx",
            "13:inline-13",
            "L:payload-20-xxxxxxxxxxxxxxxxxxxx",
            "L:payload-21-xxxxxxxxxxxxxxxxxxxx",
            "L:payload-22-xxxxxxxxxxxxxxxxxxxx",
            "end",
        ],
        "asan_call_returned_enum_handed_onward_frees_its_original",
    );
}

/// B-2026-09-20-50 — a struct payload with its own heap (`p`, `v`) is
/// entry-copied like a `String` one and leaked the same way before the fix.
/// The `Array` and `shared` payloads (`a`, `s`) are the guards: the callee
/// takes those BY TRANSFER, so there is no orphan, and a caller-side free
/// there would be the double free.
#[test]
fn asan_call_returned_enum_onward_transfer_and_struct_payloads() {
    assert_clean_asan_run(
        r#"
shared struct Sh { s: String }
enum Ta { A(Array[String, 2]), B }
enum Tsh { A(Sh), B }
struct P { id: i64, s: String }
enum Tp { A(P), B }
fn mka(n: i64) -> Ta { Ta.A([f"a-{n}", f"b-{n}"]) }
fn mksh(n: i64) -> Tsh { Tsh.A(Sh { s: f"sh-{n}" }) }
fn mkp(n: i64) -> Tp { Tp.A(P { id: n, s: f"p-{n}" }) }
fn givea(e: Ta) -> Ta { e }
fn givesh(e: Tsh) -> Tsh { e }
fn givep(e: Tp) -> Tp { e }
fn keepp(v: mut ref Vec[Tp], e: Tp) { v.push(e); }
fn main() {
    let a = givea(mka(1)); match a { Ta.A(xs) => println(f"a:{xs[1]}"), Ta.B => println("b") }
    let s = givesh(mksh(2)); match s { Tsh.A(x) => println(f"s:{x.s}"), Tsh.B => println("b") }
    let p = givep(mkp(3)); match p { Tp.A(x) => println(f"p:{x.s}"), Tp.B => println("b") }
    let mut v: Vec[Tp] = []; keepp(mut v, mkp(4)); println(f"v:{v.len()}");
    println("end");
}
"#,
        &["a:b-1", "s:sh-2", "p:p-3", "v:1", "end"],
        "asan_call_returned_enum_onward_transfer_and_struct_payloads",
    );
}
