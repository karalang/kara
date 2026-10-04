//! B-2026-10-04-32 — an arm binding over a `shared enum` whose payload is a
//! `Vec` of structs carrying a `shared` field took the `Vec` instead of
//! copying it, so a second handle's `match` found it empty and panicked. The
//! struct-element copy asked its admission predicate in the mode that refuses
//! a direct `shared` field, though the copy itself rc-increments one.

use super::*;

/// B-2026-10-04-32 — element structs mixing a `shared` field with a `String`,
/// a scalar, an `Option[shared]`, a nested struct and a `Vec`, read through
/// two handles, twice through one, moved on in the arm, after the first
/// handle is reassigned, and through `if let`.
#[test]
fn e2e_shared_enum_vec_of_shared_bearing_structs_is_copied_per_arm() {
    let src = r#"shared struct Node { v: i64 }
impl Drop for Node { fn drop(mut ref self) { println(f"dN{self.v}") } }
struct Sn { n: Node, s: String }
struct Sm { n: Node, k: i64 }
struct So { n: Option[Node], s: String }
struct In { m: Sn, z: i64 }
struct Sv { n: Node, w: Vec[i64] }
shared enum Q3 { K(Vec[Sn]), L }
shared enum Q4 { K(Vec[Sm]), L }
shared enum Q5 { K(Vec[So]), L }
shared enum Q6 { K(Vec[In]), L }
shared enum Q7 { K(Vec[Sv]), L }
fn sn(v: i64) -> Sn { Sn { n: Node { v: v }, s: f"s{v}" } }
fn main() {
    { let g = Q3.K([sn(1), sn(2)]); let h = g; match g { Q3.K(x) => println(f"a{x.len()} {x[1].n.v}"), Q3.L => {} }; match h { Q3.K(x) => println(f"a{x.len()} {x[0].s}"), Q3.L => {} }; }
    { let g = Q4.K([Sm { n: Node { v: 3 }, k: 1 }]); let h = g; match g { Q4.K(x) => println(f"b{x.len()} {x[0].n.v}"), Q4.L => {} }; match h { Q4.K(x) => println(f"b{x[0].k}"), Q4.L => {} }; }
    { let g = Q5.K([So { n: Some(Node { v: 4 }), s: "x" }]); let h = g; match g { Q5.K(x) => println(f"c{x.len()} {x[0].s}"), Q5.L => {} }; match h { Q5.K(x) => println(f"c{x.len()}"), Q5.L => {} }; }
    { let g = Q6.K([In { m: sn(5), z: 1 }]); let h = g; match g { Q6.K(x) => println(f"d{x.len()} {x[0].m.n.v}"), Q6.L => {} }; match h { Q6.K(x) => println(f"d{x[0].m.s}"), Q6.L => {} }; }
    { let g = Q7.K([Sv { n: Node { v: 6 }, w: [1, 2] }]); let h = g; match g { Q7.K(x) => println(f"e{x.len()} {x[0].n.v}"), Q7.L => {} }; match h { Q7.K(x) => println(f"e{x[0].w.len()}"), Q7.L => {} }; }
    { let g = Q3.K([sn(7)]); match g { Q3.K(x) => println(f"f{x[0].n.v}"), Q3.L => {} }; match g { Q3.K(x) => println(f"f{x[0].s}"), Q3.L => {} }; }
    { let g = Q3.K([sn(8)]); let h = g; match g { Q3.K(x) => { let y = x; println(f"g{y.len()}") }, Q3.L => {} }; match h { Q3.K(x) => println(f"g{x[0].s}"), Q3.L => {} }; }
    { let g = Q3.K([sn(9)]); match g { Q3.K(x) => println(f"h{x[0].n.v}"), Q3.L => {} }; }
    { let mut g = Q3.K([sn(10)]); let h = g; g = Q3.L; match h { Q3.K(x) => println(f"i{x[0].s}"), Q3.L => {} }; }
    { let g = Q3.K([sn(11)]); let h = g; if let Q3.K(x) = g { println(f"j{x.len()}") }; if let Q3.K(x) = h { println(f"j{x[0].n.v}") }; }
    println("end")
}"#;
    let want = "a2 2\na2 s1\ndN1\ndN2\nb1 3\nb1\ndN3\nc1 x\nc1\ndN4\nd1 5\nds5\ndN5\ne1 6\ne2\ndN6\nf7\nfs7\ndN7\ng1\ngs8\ndN8\nh9\ndN9\nis10\ndN10\nj1\nj11\ndN11\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
