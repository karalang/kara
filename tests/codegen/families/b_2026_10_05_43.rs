//! B-2026-10-05-43: a generic impl's derive-only bound on a builtin type

use super::*;

/// B-2026-10-05-43: `impl[T: Clone] G[T]` refused `G[String]` and every other
/// builtin collection at typecheck, while `fn f[T: Clone](x: T)` accepted the
/// same types. The impl gate now asks the free-fn gate's question. Once past
/// the checker, the compiled method has to clone each payload for real: a
/// `String`, a `Vec` and a `Map`, each read back after the original is
/// changed, so a shallow copy would show. `Option` and tuple payloads are left
/// out: `x.clone()` on a type parameter bound to either fails to compile, in a
/// free fn as much as here (B-2026-10-05-73, B-2026-10-05-74).
#[test]
fn e2e_clone_bounded_impl_method_on_builtin_payloads() {
    let Some(out) = run_program(
        r#"struct G[T] { v: T }
impl[T: Clone] G[T] {
    fn get(ref self) -> T { return self.v.clone(); }
}
fn main() {
    let mut a = G { v: "abc".to_string() };
    let s = a.get();
    a.v.push('d');
    println(f"{s} {a.v}");
    let mut b = G { v: vec![1, 2] };
    let v = b.get();
    b.v.push(3);
    println(f"{v.len()} {b.v.len()}");
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1, 10);
    let mut d = G { v: m };
    let m2 = d.get();
    d.v.insert(2, 20);
    println(f"{m2.len()} {d.v.len()}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "abc abcd\n2 3\n1 2\n");
}
