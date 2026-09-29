//! B-2026-09-20-4 -- a named struct handed to an owned `self` (or, after a
//! reassignment, to a by-value param) is freed by exactly one frame, including
//! a struct whose enum field boxes its payload.

use super::*;

/// B-2026-09-20-4 — owned-`self` methods over structs the callee takes by
/// TRANSFER (not copy-supported): an `Array[String, 2]` field, a nested one,
/// and `Array[D, 2]` with `Drop` elements. Also covered: `take` returning the
/// field, `fwd` handing `self` on, `two` calling another owned method on
/// `self`, a by-value param used as the receiver, a fresh literal receiver
/// and a `ref self` peek first.
///
/// Before: each receiver was dropped by both frames. At `-O0` the program
/// aborted with `free(): double free detected in tcache 2` after
/// printing `peek 10`.
#[test]
fn asan_owned_self_transfer_struct_receiver_frees_once() {
    assert_clean_asan_run(
        r#"struct D { s: String }
impl Drop for D { fn drop(mut ref self) { println(f"  d{self.s}") } }
struct Ha { g: Array[String, 2] }
struct In { g: Array[String, 2] }
struct Hn { i: In, n: i64 }
struct Hd { g: Array[D, 2] }
fn eat(h: Ha) { println(f"  ate {h.g[0]}") }
fn mka() -> Array[String, 2] { return [f"aaaaaaaa-1", f"bbbbbbbb-2"] }
impl Ha {
    fn give(self) { println("  held") }
    fn take(self) -> Array[String, 2] { return self.g }
    fn fwd(self) { eat(self) }
    fn two(self) { self.give() }
    fn peek(ref self) -> i64 { return self.g[1].len() }
}
impl Hn { fn give(self) { println(f"  held {self.n}") } }
impl Hd { fn give(self) { println("  held") } }
fn pass(h: Ha) { h.give() }
fn main() {
    let a = Ha { g: mka() };
    println(f"peek {a.peek()}");
    a.give();
    let b = Ha { g: mka() };
    let t = b.take();
    println(f"take {t[1]}");
    let c = Ha { g: mka() };
    c.fwd();
    let e = Ha { g: mka() };
    e.two();
    pass(Ha { g: mka() });
    Ha { g: mka() }.give();
    let n = Hn { i: In { g: mka() }, n: 3 };
    n.give();
    println("drops");
    let d = Hd { g: [D { s: f"1" }, D { s: f"2" }] };
    d.give();
    println("end")
}"#,
        &[
            "peek 10",
            "  held",
            "take bbbbbbbb-2",
            "  ate aaaaaaaa-1",
            "  held",
            "  held",
            "  held",
            "  held 3",
            "drops",
            "  held",
            "  d1",
            "  d2",
            "end",
        ],
        "b_2026_09_20_4_recv",
    );
}

/// B-2026-09-20-4 — the row's own cell (`eatb(self.g)` inside `impl Hb`)
/// over a boxed `enum Eb { A(Array[String, 2]), B }` field. Beside it: a
/// method that only holds `self`, one that matches `self.g`, one that
/// forwards to another owned method, the unit variant, and a `mut` binding
/// handed to a by-value param twice around a reassignment.
///
/// Before: the receiver's entry copy duplicated nothing of the boxed field,
/// so the caller, the receiver and `eatb` all owned one box. The program
/// printed nothing at `-O0` (14 valgrind errors) and aborted at `-O2`.
#[test]
fn asan_owned_self_boxed_enum_field_frees_once() {
    assert_clean_asan_run(
        r#"enum Eb { A(Array[String, 2]), B }
struct Hb { g: Eb, n: i64 }
fn eatb(g: Eb) { println("  ate") }
fn mk() -> Array[String, 2] { return [f"aaaaaaaa-1", f"bbbbbbbb-2"] }
impl Hb {
    fn give(self) { eatb(self.g) }
    fn held(self) { println(f"  held {self.n}") }
    fn read(self) -> i64 { match self.g { Eb.A(a) => a[0].len(), Eb.B => 0 } }
    fn two(self) { self.give() }
    fn peek(ref self) -> i64 { return self.n }
}
fn give2(h: Hb) { println(f"  took {h.n}") }
fn main() {
    println("start");
    let h = Hb { g: Eb.A(mk()), n: 1 };
    h.give();
    let k = Hb { g: Eb.A(mk()), n: 2 };
    k.held();
    let r = Hb { g: Eb.A(mk()), n: 3 };
    println(f"read {r.read()}");
    let w = Hb { g: Eb.A(mk()), n: 4 };
    println(f"peek {w.peek()}");
    w.two();
    let u = Hb { g: Eb.B, n: 5 };
    u.give();
    let mut m = Hb { g: Eb.A(mk()), n: 6 };
    give2(m);
    m = Hb { g: Eb.A(mk()), n: 7 };
    give2(m);
    println("end")
}"#,
        &[
            "start", "  ate", "  held 2", "read 10", "peek 4", "  ate", "  ate", "  took 6",
            "  took 7", "end",
        ],
        "b_2026_09_20_4_enumf",
    );
}
