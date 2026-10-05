//! B-2026-10-05-118: an entry-copied struct's enum payload is released once

use super::*;

/// B-2026-10-05-118 — a by-value struct the callee ENTRY-COPIES keeps some enum
/// payloads shared between the two frames (a `shared` handle, a boxed `Array`),
/// because the copy duplicates only the payloads it can. The callee's drop
/// releases them, so the caller's own copy of the struct must not. A fresh
/// temporary (`ck(K { e: E.A(H { .. }), .. })`, `ck(mk(2))`, a nested `K2`, an
/// own-`Drop` struct, a method argument, a boxed array) released them a second
/// time, and so did a named nested struct (`_11`); `_7` is a variant the copy
/// does duplicate, which the caller still frees.
#[test]
fn interp_entry_copied_struct_temp_releases_its_enum_payload_once() {
    let out = run(r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
enum E { A(H), B }
enum M { S(String), A(H) }
enum Eb { A(Array[String, 2]), B }
struct K { e: E, n: i64 }
struct K2 { k: K, m: i64 }
struct Kd { e: E, n: i64 }
impl Drop for Kd { fn drop(mut ref self) { let _ = self.n; } }
struct Km { m: M, n: i64 }
struct Hb { g: Eb, n: i64 }
fn mk(i: i64) -> K { K { e: E.A(H { id: i }), n: i } }
fn ck(k: K) { println(f"k{k.n}") }
fn ck2(k: K2) { println("k2") }
fn ckd(k: Kd) { println("kd") }
fn ckm(k: Km) { println("km") }
fn give(h: Hb) { println("g") }
struct G { z: i64 }
impl G { fn m(ref self, k: K) { println("m") } }
fn main() {
    ck(K { e: E.A(H { id: 1 }), n: 1 }); println("_1");
    ck(mk(2)); println("_2");
    ck2(K2 { k: K { e: E.A(H { id: 3 }), n: 3 }, m: 0 }); println("_3");
    ckd(Kd { e: E.A(H { id: 4 }), n: 4 }); println("_4");
    let g = G { z: 0 };
    g.m(K { e: E.A(H { id: 5 }), n: 5 }); println("_5");
    ckm(Km { m: M.A(H { id: 6 }), n: 6 }); println("_6");
    ckm(Km { m: M.S("str".to_string()), n: 7 }); println("_7");
    give(Hb { g: Eb.A(["aa".to_string(), "bb".to_string()]), n: 8 }); println("_8");
    ck(K { e: E.B, n: 9 }); println("_9");
    let k = keep(Some(H { id: 10 })); println("_10");
    let a = K2 { k: K { e: E.A(H { id: 11 }), n: 11 }, m: 0 }; ck2(a); println("_11");
    let b = K { e: E.A(H { id: 12 }), n: 12 }; ck(b); println("_12");
    println("end")
}
fn keep(o: Option[H]) -> Option[H] { o }
"#);
    assert_eq!(out, "k1\ndH1\n_1\nk2\ndH2\n_2\nk2\ndH3\n_3\nkd\ndH4\n_4\nm\ndH5\n_5\nkm\ndH6\n_6\nkm\n_7\ng\n_8\nk9\n_9\n_10\nk2\ndH11\n_11\nk12\ndH12\n_12\nend\ndH10\n", "got:\n{out}");
}
