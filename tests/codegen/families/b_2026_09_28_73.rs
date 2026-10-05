//! B-2026-09-28-73: inside `impl[T] G[T]`, `Self`, `G` and `G[T]` are one type

use super::*;

/// B-2026-09-28-73: the compiled half of the interpreter fixture of the same
/// name. The typechecker used to reject every method here but `id`; they now
/// check, and the compiled program prints what the interpreter does.
#[test]
fn e2e_generic_impl_spells_its_own_type_three_ways() {
    let Some(out) = run_program(
        r#"enum G[T] { X(T), Y }
impl[T] G[T] {
    fn id(self) -> Self { return self; }
    fn id2(self) -> G[T] { return self; }
    fn tail(self) -> G[T] { self }
    fn pick(self, c: bool, o: Self) -> Self { if c { return self; } return o; }
    fn fresh(self) -> Self { return G.Y; }
    fn fresh2(self) -> G[T] { return G.Y; }
}
struct W[T] { v: T }
impl[T] W[T] {
    fn back(self) -> W[T] { return self; }
    fn other(self, o: Self) -> Self { o }
}
fn show(g: G[i64]) {
    match g { G.X(v) => println(f"x{v}"), G.Y => println("y") }
}
fn main() {
    show(G.X(1).id());
    show(G.X(2).id2());
    show(G.X(3).tail());
    show(G.X(4).pick(false, G.X(5)));
    show(G.X(6).pick(true, G.X(7)));
    show(G.X(8).fresh());
    show(G.X(9).fresh2());
    let w = W { v: 10 }.back();
    let u = w.other(W { v: 11 });
    println(f"w{u.v}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "x1\nx2\nx3\nx5\nx6\ny\ny\nw11\n");
}
