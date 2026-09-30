//! B-2026-09-20-14 — a generic enum whose monomorph heap-BOXES its payload
//! lost the box, the payload's heap or its `Drop` body, or freed the box
//! twice, in tuple elements, struct fields and by-value struct params.

use super::*;

/// B-2026-09-20-14 — a generic enum whose monomorph heap-BOXES its payload
/// (`G1[W]`, `W` a two-word struct with a `Drop` body), held as a TUPLE
/// element: left alone, matched with a wildcard, moved out with `let`, handed
/// to a by-value param, destructured, matched as a tuple pattern (one and two
/// boxed elements), moved into a `Vec` from an arm, built inline from a
/// constructor, recreated in a loop and moved whole. Each box and its
/// contents are freed once and each payload's body runs once, where the
/// interpreter runs it.
#[test]
fn asan_generic_enum_boxed_payload_in_a_tuple_element_has_one_owner() {
    assert_clean_asan_run_min_allocs(
        r#"struct W { s: String, n: i64 }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.n}:{self.s.len()}") } }
enum G1[T] { Y(T), N }
enum G2[T] { X(T, i64), Z }
struct S[T] { g: G1[T], k: i64 }
struct Sm { g: G1[W], k: i64 }
fn mk(n: i64) -> G1[W] { return G1.Y(W { s: f"payload-heap-string-{n}", n: n }) }
fn takeg(g: G1[W]) { match g { G1.Y(w) => println(f"took{w.n}"), G1.N => println("n") } }
fn main() {
    let t1 = (mk(1), 7); println(f"a{t1.1}")
    let g2 = mk(2); let t2 = (g2, 7); println(f"b{t2.1}")
    let t3 = (mk(3), 7); match t3.0 { G1.Y(_) => println("c"), G1.N => println("n") }
    let t4 = (mk(4), 7); let x4 = t4.0; println(f"d{t4.1}")
    let t5 = (mk(5), 7); takeg(t5.0); println("e")
    let t6 = (mk(6), 7); let (g6, n6) = t6; println(f"f{n6}")
    let t7 = (mk(7), 7); match t7 { (G1.Y(w), n) => println(f"g{w.n}{n}"), (G1.N, _) => println("n") }
    let t8 = (mk(8), mk(9)); match t8 { (G1.Y(w), _) => println(f"h{w.n}"), _ => println("n") }
    let mut v: Vec[W] = Vec.new(); let t10 = (mk(10), 7); match t10.0 { G1.Y(w) => v.push(w), G1.N => println("n") }; println(f"i{v.len()}")
    let t11 = (G2.X(W { s: "a-g2-heap-string-11", n: 11 }, 4), 7); println(f"j{t11.1}")
    for i in 12..14 { let t = (mk(i), i); if i == 13 { match t.0 { G1.Y(w) => println(f"k{w.n}"), G1.N => println("n") } } }
    let t14 = (mk(14), 7); let t15 = t14; println(f"l{t15.1}")
    println("end")
}
"#,
        &[
            "a7", "dW1:21", "b7", "dW2:21", "c", "dW3:21", "dW4:21", "d7", "took5", "dW5:21", "e",
            "dW6:21", "f7", "g77", "dW7:21", "h8", "dW8:21", "dW9:21", "i1", "dW10:22", "j7",
            "dW11:19", "dW12:22", "k13", "dW13:22", "l7", "dW14:22", "end",
        ],
        "asan_generic_enum_boxed_payload_in_a_tuple_element_has_one_owner",
        24,
    );
}

/// B-2026-09-20-14 — the same boxed payload as a STRUCT field, in a generic
/// holder (`S[W]`) and a concrete one (`Sm`): unmatched, built from a named
/// local, wildcard-matched, moved out, the whole struct moved, the payload
/// moved into a `Vec` from an arm, the field handed to a by-value param, and
/// the holder pushed into a `Vec`.
#[test]
fn asan_generic_enum_boxed_payload_in_a_struct_field_has_one_owner() {
    assert_clean_asan_run_min_allocs(
        r#"struct W { s: String, n: i64 }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.n}:{self.s.len()}") } }
enum G1[T] { Y(T), N }
enum G2[T] { X(T, i64), Z }
struct S[T] { g: G1[T], k: i64 }
struct Sm { g: G1[W], k: i64 }
fn mk(n: i64) -> G1[W] { return G1.Y(W { s: f"payload-heap-string-{n}", n: n }) }
fn takeg(g: G1[W]) { match g { G1.Y(w) => println(f"took{w.n}"), G1.N => println("n") } }
fn mksm(n: i64) -> Sm { return Sm { g: mk(n), k: n } }
fn main() {
    let s1 = S { g: mk(1), k: 7 }; println(f"a{s1.k}")
    let s2 = Sm { g: mk(2), k: 7 }; println(f"b{s2.k}")
    let g3 = mk(3); let s3 = Sm { g: g3, k: 7 }; println(f"c{s3.k}")
    let s4 = Sm { g: mk(4), k: 7 }; match s4.g { G1.Y(_) => println("d"), G1.N => println("n") }
    let s5 = S { g: mk(5), k: 7 }; let x5 = s5.g; println(f"e{s5.k}")
    let s6 = Sm { g: mk(6), k: 7 }; let s6b = s6; println(f"f{s6b.k}")
    let s7 = S { g: mk(7), k: 7 }; let s7b = s7; println(f"g{s7b.k}")
    let mut v: Vec[W] = Vec.new(); let s8 = Sm { g: mk(8), k: 7 }; match s8.g { G1.Y(w) => v.push(w), G1.N => println("n") }; println(f"h{v.len()}")
    let s13 = Sm { g: mk(13), k: 13 }; takeg(s13.g); println("i")
    let mut u: Vec[Sm] = Vec.new(); u.push(mksm(14)); println(f"j{u.len()}")
    println("end")
}
"#,
        &[
            "a7", "dW1:21", "b7", "dW2:21", "c7", "dW3:21", "d", "dW4:21", "dW5:21", "e7", "f7",
            "dW6:21", "g7", "dW7:21", "h1", "dW8:21", "took13", "dW13:22", "i", "j1", "dW14:22",
            "end",
        ],
        "asan_generic_enum_boxed_payload_in_a_struct_field_has_one_owner",
        20,
    );
}

/// B-2026-09-20-14 — a concrete struct holding the boxed payload, passed BY
/// VALUE as a named local, a struct literal and a call's result, and matched
/// inside the callee. The entry copy cannot duplicate a box whose contents
/// run a body, so the callee takes it and now runs that body; the caller's
/// walk skips the field.
#[test]
fn asan_generic_enum_boxed_payload_struct_passed_by_value_runs_its_body_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct W { s: String, n: i64 }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.n}:{self.s.len()}") } }
enum G1[T] { Y(T), N }
enum G2[T] { X(T, i64), Z }
struct S[T] { g: G1[T], k: i64 }
struct Sm { g: G1[W], k: i64 }
fn mk(n: i64) -> G1[W] { return G1.Y(W { s: f"payload-heap-string-{n}", n: n }) }
fn takeg(g: G1[W]) { match g { G1.Y(w) => println(f"took{w.n}"), G1.N => println("n") } }
fn takesm(s: Sm) { println(f"ts{s.k}") }
fn takesmm(s: Sm) { match s.g { G1.Y(w) => println(f"tm{w.n}"), G1.N => println("n") } }
fn mksm(n: i64) -> Sm { return Sm { g: mk(n), k: n } }
fn main() {
    let s9 = Sm { g: mk(9), k: 9 }; takesm(s9)
    takesm(Sm { g: mk(10), k: 10 })
    takesm(mksm(11))
    let s12 = Sm { g: mk(12), k: 12 }; takesmm(s12)
    println("end")
}
"#,
        &[
            "ts9", "dW9:21", "ts10", "dW10:22", "ts11", "dW11:22", "tm12", "dW12:22", "end",
        ],
        "asan_generic_enum_boxed_payload_struct_passed_by_value_runs_its_body_once",
        8,
    );
}
