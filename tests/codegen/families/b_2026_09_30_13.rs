//! B-2026-09-30-13: a `let` use-after-move copy of a Drop struct keeps the source's memory.

use super::*;

/// B-2026-09-30-13 — a `let` use-after-move copy (`let x = t; println(t.s)`)
/// of a struct with a user `Drop` gives the consumer its own heap, so the
/// source keeps its MEMORY and gives up its BODY. The source's cleanup was
/// retracted outright, so its original `String` buffer / `shared` box leaked
/// on every compiled surface; a generic `G[String]` source then kept a memory
/// drop that freed nothing. One body per value, as the interpreter counts it.
/// `c8` is the VIEW guard: `let m = self; let n = m;` reads `m` again, and `m`
/// holds only a bodies-only action (the caller owns the memory), so it gives
/// up that action and keeps nothing; converting it would run `m`'s body too.
#[test]
fn e2e_let_use_after_move_copy_of_a_drop_struct_keeps_the_source_memory() {
    let Some(out) = run_program(
        r#"shared struct Sh { k: i64 }
struct S1 { s: String, id: i64 }
impl Drop for S1 { fn drop(mut ref self) { println(f"d1_{self.id}") } }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"d2_{self.id}") } }
struct S3 { h: Sh, s: String, id: i64 }
impl Drop for S3 { fn drop(mut ref self) { println(f"d3_{self.id}") } }
struct G[T] { v: T, id: i64 }
impl[T] Drop for G[T] { fn drop(mut ref self) { println(f"dG{self.id}") } }
impl S1 { fn tk(self) -> i64 { let m = self; let n = m; return m.s.len() } }
fn mk(i: i64) -> S1 { return S1 { s: f"cccccccccccccccccccccccccccccc{i}", id: i } }
fn c1() { let t = mk(1); let x = t; println(f"{t.s.len()} {x.s.len()}") }
fn c2() { let t = S2 { h: Sh { k: 2 }, id: 2 }; let x = t; println(f"{t.h.k} {x.h.k}") }
fn c3() { let t = S3 { h: Sh { k: 3 }, s: f"dddddddddddddddddddddddddddddd{3}", id: 3 }; let x = t; println(f"{t.h.k} {t.s.len()} {x.h.k}") }
fn c4() { let t = G { v: f"ffffffffffffffffffffffffffffff{4}", id: 4 }; let x = t; println(f"{t.v.len()} {x.v.len()}") }
fn c5() { let t = G { v: Sh { k: 5 }, id: 5 }; let x = t; println(f"{t.v.k} {x.v.k}") }
fn c6() { let t = G { v: f"gggggggggggggggggggggggggggggg{6}", id: 6 }; { let x = t; println(f"blk{x.id}") } println(f"{t.v.len()}") }
fn c7(t: S1) { let x = t; println(f"p{t.s.len()} {x.id}") }
fn c8() { let a = mk(8); println(f"v{a.tk()}"); println(f"f{mk(9).tk()}") }
fn main() { c1(); c2(); c3(); c4(); c5(); c6(); c7(mk(7)); c8(); println("end") }
"#,
    ) else {
        return;
    };
    assert_eq!(out, "31 31\nd1_1\n2 2\nd2_2\n3 31 3\nd3_3\n31 31\ndG4\n5 5\ndG5\nblk6\ndG6\n31\np31 7\nd1_7\nd1_8\nv31\nd1_9\nf31\nend\n", "got:\n{out}");
}
