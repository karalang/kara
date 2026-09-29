//! B-2026-09-26-8 -- a method returning a `shared` field of its `self`
//! receiver retains it, so the caller's copy is an owner of its own.

use super::*;

/// B-2026-09-26-8 — `fn get(ref self) -> Sh { self.s }` handed the caller a
/// pointer to the field without a retain, so the caller's binding released a
/// count it never held: a use-after-free (valgrind 25 errors on this program,
/// garbage `k` values) on every compiled surface. The return-retain already
/// covered `obj.field` for a named param and missed the `self` receiver.
/// Covered: tail and `return` forms, a discarded call, a projection off the
/// call, a loop, a `mut ref self` receiver and a consuming `self` receiver.
#[test]
fn e2e_method_returning_self_shared_field_retains() {
    let src = r#"struct D { id: i64 }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}") } }
shared struct Sh { k: i64, name: String, d: D }
impl Drop for Sh { fn drop(mut ref self) { println(f"dSh{self.k}{self.name}") } }
fn mksh(n: i64) -> Sh { return Sh { k: n, name: f"nm{n}-heap-string-longer-than-sso", d: D { id: n } }; }
struct Hold { s: Sh, t: i64 }
impl Hold {
    fn get(ref self) -> Sh { self.s }
    fn getr(ref self) -> Sh { return self.s }
    fn getm(mut ref self) -> Sh { self.s }
    fn take(self) -> Sh { self.s }
}
fn main() {
    let mut h = Hold { s: mksh(3), t: 0 };
    let x = h.get(); println(f"x{x.k}");
    h.get();
    println(f"p{h.get().k}");
    let y = h.getr(); println(f"y{y.k}");
    for i in 0..2 { let a = h.get(); println(f"a{a.k}{i}"); }
    let z = h.getm(); println(f"z{z.k}");
    let g = Hold { s: mksh(4), t: 1 };
    let w = g.take(); println(f"w{w.k}");
    println(f"end{h.s.k}{x.k}{y.k}{z.k}{w.k}")
}
"#;
    let want = "x3\np3\ny3\na30\na31\nz3\nw4\nend33334\ndSh4nm4-heap-string-longer-than-sso\ndD4\ndSh3nm3-heap-string-longer-than-sso\ndD3\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
