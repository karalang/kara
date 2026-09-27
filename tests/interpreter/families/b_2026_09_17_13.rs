//! B-2026-09-17-13 -- a method of a CONCRETE impl block over a GENERIC enum
//! (`impl G1[String]`) sees its payload at the concrete type, as the
//! `impl[T] G1[T]` monomorph always did.

use super::*;

/// B-2026-09-17-13 — the interpreter twin of
/// `e2e_concrete_impl_over_generic_enum_sees_the_payload`: `--interp` was the
/// reference throughout, pinned here so the backends keep one answer.
#[test]
fn test_concrete_impl_over_generic_enum_sees_the_payload() {
    let out = run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum G[T] { X(T), Y }
impl G[R] {
    fn read(self) -> i64 { match self { G.X(t) => { return 1; } G.Y => { return 0; } } }
    fn reb(self) -> i64 { let e = self; match e { G.X(t) => { return 2; } G.Y => { return 0; } } }
}
enum G1[T] { Y(T), N }
impl G1[String] {
    fn shm(self) { match self { G1.Y(v) => { println(f"mx {v}") } G1.N => { println("mx NONE") } } }
    fn get(self) -> String { match self { G1.Y(v) => { v } G1.N => { f"none" } } }
    fn peek(ref self) { match self { G1.Y(v) => { println(f"pk {v}") } G1.N => { println("pk none") } } }
    fn iflet(self) { if let G1.Y(v) = self { println(f"il {v}") } else { println("il none") } }
}
fn mks(s: String) -> G1[String] { return G1.Y(s); }
fn main() {
    let g: G[R] = G.X(mk(23));
    println(f"a{g.read()}");
    let h: G[R] = G.X(mk(24));
    println(f"b{h.reb()}");
    println(f"c{G.X(mk(25)).read()}");
    { let s: G1[String] = G1.Y(f"twin-payload-alpha"); s.shm() }
    G1.Y(f"temp-payload-beta").shm();
    mks(f"call-payload-gamma").shm();
    let n: G1[String] = G1.N;
    n.shm();
    let t: G1[String] = G1.Y(f"named-get-delta");
    println(f"{t.get()}!");
    println(G1.Y(f"temp-get-epsilon").get());
    let p: G1[String] = G1.Y(f"ref-payload-zeta");
    p.peek();
    p.peek();
    let q: G1[String] = G1.Y(f"iflet-payload-eta");
    q.iflet();
    for i in 0..2 { let w: G1[String] = G1.Y(f"loop-payload-{i}-iota"); w.shm(); }
    println("end");
}
"#);
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        [
            "dR23",
            "a1",
            "dR24",
            "b2",
            "dR25",
            "c1",
            "mx twin-payload-alpha",
            "mx temp-payload-beta",
            "mx call-payload-gamma",
            "mx NONE",
            "named-get-delta!",
            "temp-get-epsilon",
            "pk ref-payload-zeta",
            "pk ref-payload-zeta",
            "il iflet-payload-eta",
            "mx loop-payload-0-iota",
            "mx loop-payload-1-iota",
            "end"
        ],
        "got:\n{out}"
    );
}

/// B-2026-09-17-13 — interpreter twin of the colliding-impls cell.
#[test]
fn test_colliding_concrete_impls_over_generic_enum_free_once() {
    let out = run(r#"enum G1[T] { Y(T), N }
impl G1[Vec[i64]] { fn show(self) { match self { G1.Y(v) => { println(f"vec {v}") } G1.N => { println("vec NONE") } } } }
impl G1[String] { fn show(self) { match self { G1.Y(v) => { println(f"str {v}") } G1.N => { println("str NONE") } } } }
fn main() {
    let a: G1[Vec[i64]] = G1.Y([1, 2, 3]);
    a.show();
    let b: G1[String] = G1.Y(f"collide-payload-theta");
    b.show();
    for i in 0..2 { let w: G1[Vec[i64]] = G1.Y([i, i]); w.show(); }
    println("end");
}
"#);
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        [
            "vec [1, 2, 3]",
            "str collide-payload-theta",
            "vec [0, 0]",
            "vec [1, 1]",
            "end"
        ],
        "got:\n{out}"
    );
}

/// B-2026-09-17-13 — interpreter twin of the generic temp-receiver cell.
#[test]
fn test_generic_impl_temp_receiver_boxed_payload_freed_once() {
    let out = run(r#"enum G1[T] { Y(T), N }
impl[T] G1[T] {
    fn shm(self) { match self { G1.Y(v) => { println(f"mx {v}") } G1.N => { println("mx NONE") } } }
    fn iflet(self) { if let G1.Y(v) = self { println(f"il {v}") } else { println("il none") } }
}
fn mks(s: String) -> G1[String] { return G1.Y(s); }
fn main() {
    G1.Y(f"temp-payload-aaaaaaaaaaa").shm();
    mks(f"call-payload-bbbbbbbbbbb").shm();
    G1.Y(f"temp-iflet-ccccccccccccc").iflet();
    let n: G1[String] = G1.N;
    n.shm();
    println("end");
}
"#);
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        [
            "mx temp-payload-aaaaaaaaaaa",
            "mx call-payload-bbbbbbbbbbb",
            "il temp-iflet-ccccccccccccc",
            "mx NONE",
            "end"
        ],
        "got:\n{out}"
    );
}
