//! B-2026-09-27-130 — an inline user-enum param handed back on some paths and forwarded on the others.

use super::*;

/// B-2026-09-27-130 — `fn pk(h: Hc, k: bool) -> Hc { if k { return h }
/// shc(h); return Hc.Empty }` over an INLINE `enum Hc { Full(S), Empty }`.
/// The forwarding leg ran no `Drop` body on any compiled surface: the
/// statement disarm read `shc(h)` as a hand-over, while an inline payload's
/// bodies stay with the caller at every argument. Free, instance-method and
/// associated spellings, each leg, plus a fresh temp. The unrelated
/// `shows(h: Ho[S])` shares the param NAME `h`; compiled first, it made every
/// later `h` read as a boxed `Ho[S]`, which crashed at the first forward.
#[test]
fn asan_inline_user_enum_param_handed_back_on_some_paths_forwarded() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
enum Hc { Full(S), Empty }
fn shc(h: Hc) { match h { Hc.Full(r) => println(f"s{r.id}"), Hc.Empty => println("e") } }
enum Ho[T] { Full(T), Empty }
fn shows(h: Ho[S]) { match h { Ho.Full(r) => println(f"t{r.id}"), Ho.Empty => println("te") } }
struct K { z: i64 }
impl K {
    fn mp(ref self, h: Hc, k: bool) -> Hc { if k { return h } shc(h); return Hc.Empty }
    fn ap(h: Hc, k: bool) -> Hc { if k { return h } shc(h); return Hc.Empty }
}
fn pk(h: Hc, k: bool) -> Hc { if k { return h } shc(h); return Hc.Empty }
fn main() {
    let kk = K { z: 0 };
    let a = Hc.Full(mks(1)); let r1 = kk.mp(a, false); shc(r1);
    let b = Hc.Full(mks(2)); let r2 = kk.mp(b, true); shc(r2);
    let c = Hc.Full(mks(3)); let r3 = K.ap(c, false); shc(r3);
    let d = Hc.Full(mks(4)); let r4 = K.ap(d, true); shc(r4);
    let e = Hc.Full(mks(5)); let r5 = pk(e, false); shc(r5);
    let f = Hc.Full(mks(6)); let r6 = pk(f, true); shc(r6);
    let r7 = pk(Hc.Full(mks(7)), false); shc(r7);
    shows(Ho.Full(mks(8)));
    println("end")
}
"#,
        &[
            "s1", "dS1", "e", "s2", "dS2", "s3", "dS3", "e", "s4", "dS4", "s5", "dS5", "e", "s6",
            "dS6", "s7", "dS7", "e", "t8", "dS8", "end",
        ],
        "asan_inline_user_enum_param_handed_back_on_some_paths_forwarded",
    );
}

/// B-2026-09-27-130 — the same leak through a LOCAL: `mkh`'s `let h: Ho[S]`
/// must not make `pk`'s inline `h` look boxed.
#[test]
fn asan_inline_user_enum_param_after_same_named_boxed_local() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
enum Hc { Full(S), Empty }
fn shc(h: Hc) { match h { Hc.Full(r) => println(f"s{r.id}"), Hc.Empty => println("e") } }
enum Ho[T] { Full(T), Empty }
fn mkh() -> i64 { let h: Ho[S] = Ho.Full(mks(9)); match h { Ho.Full(r) => r.id, Ho.Empty => 0 } }
fn eat(h: Hc) { println("x") }
fn pk(h: Hc, k: bool) -> Hc { if k { return h } eat(h); return Hc.Empty }
fn main() { println(f"m{mkh()}"); let a = Hc.Full(mks(4)); let r = pk(a, false); shc(r); let b = Hc.Full(mks(5)); let q = pk(b, true); shc(q); println("end") }
"#,
        &["dS9", "m9", "x", "dS4", "e", "s5", "dS5", "end"],
        "asan_inline_user_enum_param_after_same_named_boxed_local",
    );
}
