//! B-2026-09-28-73: inside `impl[T] G[T]`, `Self`, `G` and `G[T]` are one type

use super::*;

/// Every spelling of the impl's own type the row measured, plus a struct.
/// `id2` and `tail` hand `self` back as `G[T]`, `pick` returns a
/// `Self`-typed parameter from `-> Self`, and `fresh` builds a new value of
/// its own type from `-> Self`.
const SPELLINGS: &str = r#"enum G[T] { X(T), Y }
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
"#;

fn type_errors(src: &str) -> Vec<String> {
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    karac::typecheck(&parsed.program, &resolved)
        .errors
        .into_iter()
        .map(|e| e.message)
        .collect()
}

/// B-2026-09-28-73: the four spellings that used to fail (`expected 'G[T]',
/// found 'G'`, `expected 'G', found 'Self'`, `expected 'G', found 'G[T]'`)
/// now check, and run.
#[test]
fn interp_generic_impl_spells_its_own_type_three_ways() {
    let errors = type_errors(SPELLINGS);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(run(SPELLINGS), "x1\nx2\nx3\nx5\nx6\ny\ny\nw11\n");
}

/// B-2026-09-28-73: the unification is for a GENERIC impl only, so inside a
/// concrete `impl G[i64]` a `G[String]` is still not `Self`.
#[test]
fn interp_concrete_impl_self_still_checks_its_args() {
    let errors = type_errors(
        r#"enum G[T] { X(T), Y }
impl G[i64] {
    fn bad(self) -> Self { return G.X("s"); }
}
fn main() {}
"#,
    );
    assert!(
        !errors.is_empty(),
        "a G[String] must not satisfy `-> Self` in impl G[i64]"
    );
}
