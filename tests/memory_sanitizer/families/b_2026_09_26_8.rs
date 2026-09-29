//! B-2026-09-26-8 -- a method returning a `shared` field of its `self`
//! receiver retains it, so every owner releases exactly once.

use super::*;

/// B-2026-09-26-8 — the unretained `self.s` return was a use-after-free and a
/// double release on every compiled surface.
#[test]
fn asan_method_returning_self_shared_field_retains() {
    assert_clean_asan_run(
        r#"struct D { id: i64 }
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
"#,
        &[
            "x3",
            "p3",
            "y3",
            "a30",
            "a31",
            "z3",
            "w4",
            "end33334",
            "dSh4nm4-heap-string-longer-than-sso",
            "dD4",
            "dSh3nm3-heap-string-longer-than-sso",
            "dD3",
        ],
        "B-2026-09-26-8 method returning self shared field",
    );
}
