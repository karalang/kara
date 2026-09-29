//! B-2026-09-29-20 -- a generic struct's owners (nested, `Vec` element,
//! clone, displaced element) release its `shared` boxes through the instantiation.

use super::*;

/// B-2026-09-29-20 — a GENERIC struct instantiated at another generic struct
/// (`Q[Q[S2]]`, `Z[Q[S2]]`, `Q[Q[Q[S2]]]`) leaked the innermost `shared` box on
/// every compiled surface, because the combined drop's gate and its nested
/// walk asked the erased declaration and a name-keyed cycle guard read the
/// inner `Q` as a cycle. The same erasure reached the other owners of such a
/// value: a `Vec` element's drop (`[T { u: Sh { .. } }]`), `Vec.clone()` (the
/// clone read garbage past the first field and aliased the box), a displaced
/// `v[i] = ..` element (no memory released, no `Drop` body run), and the
/// source disarm of `v[i] = a` (the erased zeroing left `a` holding the box).
#[test]
fn e2e_generic_struct_nested_and_element_owners_release_shared_boxes() {
    let Some(out) = run_program(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
struct Q[U] { u: U }
struct Z[U] { n: i64, s: String, u: U }
struct T[U] { u: U, id: i64 }
struct H { xs: Vec[Q[S2]] }
fn mk(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
fn hs(i: i64) -> String { return f"heap-{i}-abcdefghijklmnopqrstuvwxyz" }
fn st(v: mut ref Vec[Q[S2]]) { v[0] = Q { u: mk(22) }; }
fn main() {
    { let q = Q { u: Q { u: mk(1) } }; println(f"a{q.u.u.id}") }
    { let q = Q { u: Q { u: Q { u: mk(2) } } }; println(f"b{q.u.u.u.id}") }
    { let z = Z { n: 1, s: hs(3), u: Q { u: mk(3) } }; println(f"c{z.u.u.id}") }
    { let v = [Q { u: Q { u: mk(4) } }]; println(f"d{v.len()}") }
    { let v = [T { u: Sh { k: 5 }, id: 5 }]; println(f"e{v[0].id}") }
    { let mut v: Vec[Q[Q[S2]]] = Vec.new(); v.push(Q { u: Q { u: mk(6) } }); println(f"f{v.len()}") }
    { let v = [Z { n: 7, s: hs(7), u: mk(7) }]; let w = v.clone(); println(f"g{w[0].n}{w[0].u.id}") }
    { let v = [Q { u: Q { u: mk(8) } }]; let w = v.clone(); println(f"h{w[0].u.u.id}") }
    { let mut v = [Q { u: mk(9) }]; v[0] = Q { u: mk(10) }; println(f"i{v[0].u.id}") }
    { let mut v = [Q { u: hs(11) }]; v[0] = Q { u: hs(12) }; println(f"j{v[0].u}") }
    { let mut v = [T { u: Sh { k: 13 }, id: 13 }]; v[0] = T { u: Sh { k: 14 }, id: 14 }; println(f"k{v[0].id}") }
    { let mut h = H { xs: [Q { u: mk(15) }] }; h.xs[0] = Q { u: mk(16) }; println(f"l{h.xs[0].u.id}") }
    { let mut v = [Q { u: mk(21) }]; st(mut v); println(f"m{v[0].u.id}") }
    { let mut v = [Q { u: Q { u: mk(23) } }]; v[0] = Q { u: Q { u: mk(24) } }; println(f"n{v[0].u.u.id}") }
    { let mut v = [Q { u: mk(25) }, Q { u: mk(26) }]; let a = v.pop().unwrap(); v[0] = a; println(f"o{v[0].u.id}{v.len()}") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "a1\ndS1\nb2\ndS2\nc3\ndS3\nd1\ndS4\ne5\nf1\ndS6\ndS7\ng77\ndS7\ndS8\nh8\ndS8\ndS9\ni10\ndS10\njheap-12-abcdefghijklmnopqrstuvwxyz\nk14\ndS15\nl16\ndS16\ndS21\nm22\ndS22\ndS23\nn24\ndS24\ndS25\no261\ndS26\nend\n", "got:\n{out}");
}
