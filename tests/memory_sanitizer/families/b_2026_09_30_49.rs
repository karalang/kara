//! B-2026-09-30-49 -- a named local moved into a struct literal written as a
//! call argument runs its `Drop` body once.

use super::*;

/// B-2026-09-30-49 — the memory half: with a `Vec` source (`fq(Hq { v: q, n:
/// 40 })`), the source's own binding walked and freed the buffer and the
/// literal's temp then walked it again, one invalid read (`d<garbage>`) on
/// every compiled surface, the `ref`-param spellings included. Same program
/// as the codegen twin.
#[test]
fn asan_struct_literal_arg_local_runs_body_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct Hr { v: R, n: i64 }
struct Hq { v: Vec[R], n: i64 }
struct G[T] { v: T, n: i64 }
struct O { h: Hr, m: i64 }
fn f2(g: Hr) -> i64 { g.n }
fn fq(g: Hq) -> i64 { g.n }
fn fref(g: ref Hq) -> i64 { g.n }
fn back(g: Hr) -> Hr { g }
fn gg[T](g: G[T]) -> i64 { g.n }
fn nest(o: O) -> i64 { o.m }
fn opt(g: Option[Hr]) -> i64 { 1 }
fn tup(g: (Hr, i64)) -> i64 { g.1 }
fn keep(v: mut ref Vec[Hr], g: Hr) { v.push(g) }
struct K { n: i64 }
impl K {
    fn m(ref self, g: Hr) -> i64 { g.n }
    fn mr(ref self, g: ref Hq) -> i64 { g.n }
    fn a(g: Hr) -> i64 { g.n }
}
fn main() {
    let r1 = R { id: 1 };
    println(f2(Hr { v: r1, n: 10 }));
    let r2 = R { id: 2 };
    let k2 = f2(Hr { v: r2, n: 20 });
    println("mid");
    println(k2);
    let r3 = R { id: 3 };
    let h3 = back(Hr { v: r3, n: 30 });
    println(h3.n);
    let q4 = [R { id: 4 }];
    println(fq(Hq { v: q4, n: 40 }));
    let q5 = [R { id: 5 }];
    println(fref(Hq { v: q5, n: 50 }));
    let k = K { n: 0 };
    let r6 = R { id: 6 };
    println(k.m(Hr { v: r6, n: 60 }));
    let q7 = [R { id: 7 }];
    println(k.mr(Hq { v: q7, n: 70 }));
    let r8 = R { id: 8 };
    println(K.a(Hr { v: r8, n: 80 }));
    let r9 = R { id: 9 };
    println(gg(G { v: r9, n: 90 }));
    let f = |g: Hr| g.n;
    let r11 = R { id: 11 };
    println(f(Hr { v: r11, n: 110 }));
    let r12 = R { id: 12 };
    println(nest(O { h: Hr { v: r12, n: 1 }, m: 120 }));
    let r13 = R { id: 13 };
    println(opt(Some(Hr { v: r13, n: 1 })));
    let r14 = R { id: 14 };
    println(tup((Hr { v: r14, n: 1 }, 140)));
    let mut v: Vec[Hr] = [];
    let r15 = R { id: 15 };
    keep(mut v, Hr { v: r15, n: 150 });
    println(v.len());
    let mut i = 0;
    while i < 2 { let r = R { id: 20 + i }; println(f2(Hr { v: r, n: i })); i = i + 1; }
    let o = Some(R { id: 16 });
    match o { Some(x) => { println(f2(Hr { v: x, n: 160 })) }, None => {} }
    println("end");
}
"#,
        &[
            "d1", "10", "d2", "mid", "20", "30", "d3", "d4", "40", "d5", "50", "d6", "60", "d7",
            "70", "d8", "80", "d9", "90", "d11", "110", "d12", "120", "d13", "1", "d14", "140",
            "1", "d15", "d20", "0", "d21", "1", "d16", "160", "end",
        ],
        "B-2026-09-30-49 struct literal arg holding a moved local",
    );
}
