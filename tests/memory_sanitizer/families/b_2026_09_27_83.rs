//! B-2026-09-27-83: returning a boxed `Array` payload whose element runs a `Drop` body

use super::*;

/// B-2026-09-27-83: an arm binding of a boxed `Array[R, 2]` payload, where `R`
/// carries heap and a user `Drop` body, handed out of the frame (`return t`, a
/// re-bind then `return u`, the arm's value as the function's tail, `return
/// Some(t)`, a conditional return, `if let`) over `Option`, `Result` and plain
/// user-enum params and locals. Each double-freed every element's heap compiled:
/// the box's memory drop freed it and so did the caller's result. Two call-arg
/// cells (`return eat(v)` and `eat(match x { .. })`) guard the other direction:
/// there the bodies stay with the box, so its copy must not be zeroed.
#[test]
fn asan_boxed_array_payload_with_drop_elems_returned_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
enum EArr { A(Array[R, 2]), B }
enum E2 { A(Array[R, 2], i64), B }
enum Esx { A { a: Array[R, 2] }, B }
fn z() -> Array[R, 2] { return [mk(0), mk(0)]; }
fn eat(a: Array[R, 2]) -> i64 { return a[1].id; }
fn o1(x: Option[Array[R, 2]]) -> Array[R, 2] { match x { Some(t) => { return t; } None => { return z(); } } }
fn o2(x: Option[Array[R, 2]]) -> Array[R, 2] { match x { Some(t) => { let u = t; return u; } None => { return z(); } } }
fn o3(x: Option[Array[R, 2]]) -> Array[R, 2] { match x { Some(t) => t, None => z() } }
fn o4(x: Option[Array[R, 2]]) -> i64 { match x { Some(t) => { let u = t; return u[0].id; } None => { return 0; } } }
fn o5(x: Option[Array[R, 2]]) -> Option[Array[R, 2]] { match x { Some(t) => { return Some(t); } None => { return None; } } }
fn o6(x: Option[Array[R, 2]], c: bool) -> Array[R, 2] { match x { Some(t) => { if c { return t; } return z(); } None => { return z(); } } }
fn o7(x: Option[Array[R, 2]]) -> Array[R, 2] { match x { Some(t) => { let u = t; u } None => z() } }
fn o8(x: Option[Array[R, 2]]) -> Array[R, 2] { if let Some(t) = x { return t; } return z(); }
fn o9(x: Result[Array[R, 2], String]) -> Array[R, 2] { match x { Ok(t) => { return t; } Err(e) => { return z(); } } }
fn l1() -> Array[R, 2] { let y: Array[R, 2] = [mk(1), mk(2)]; let x = Some(y); match x { Some(t) => { return t; } None => { return z(); } } }
fn l2() -> Array[R, 2] { let y: Array[R, 2] = [mk(1), mk(2)]; let x = Some(y); match x { Some(t) => { let u = t; return u; } None => { return z(); } } }
fn l3() -> i64 { let y: Array[R, 2] = [mk(1), mk(2)]; let x = Some(y); match x { Some(t) => { let u = t; return u[0].id; } None => { return 0; } } }
fn u1(x: EArr) -> Array[R, 2] { match x { EArr.A(v) => { return v; } EArr.B => { return z(); } } }
fn u2(x: EArr) -> Array[R, 2] { match x { EArr.A(v) => { let u = v; return u; } EArr.B => { return z(); } } }
fn u3(x: EArr) -> Array[R, 2] { match x { EArr.A(v) => v, EArr.B => z() } }
fn u4(x: EArr) -> i64 { match x { EArr.A(v) => { return eat(v); } EArr.B => { return 0; } } }
fn u8(x: E2) -> Array[R, 2] { match x { E2.A(v, n) => { return v; } E2.B => { return z(); } } }
fn u9(x: Esx) -> Array[R, 2] { match x { Esx.A { a } => { return a; } Esx.B => { return z(); } } }
fn u10(x: EArr) -> Option[Array[R, 2]] { match x { EArr.A(v) => { return Some(v); } EArr.B => { return None; } } }
fn u11(x: EArr, c: bool) -> Array[R, 2] { match x { EArr.A(v) => { if c { return v; } return z(); } EArr.B => { return z(); } } }
fn u12() -> Array[R, 2] { let y: Array[R, 2] = [mk(1), mk(2)]; let x = EArr.A(y); match x { EArr.A(t) => { return t; } EArr.B => { return z(); } } }
fn u13(x: EArr) -> Array[R, 2] { if let EArr.A(v) = x { return v; } return z(); }
fn u15(x: EArr) -> i64 { return eat(match x { EArr.A(v) => v, EArr.B => z() }); }
fn main() {
  let a1 = o1(Some([mk(1), mk(2)])); println(f"o1 {a1[0].id}");
  let a2 = o2(Some([mk(3), mk(4)])); println(f"o2 {a2[0].id}");
  let a3 = o3(Some([mk(5), mk(6)])); println(f"o3 {a3[0].id}");
  println(f"o4 {o4(Some([mk(7), mk(8)]))}");
  let a4 = o5(Some([mk(9), mk(10)])); println("o5");
  let a5 = o6(Some([mk(11), mk(12)]), true); println(f"o6 {a5[0].id}");
  let a6 = o7(Some([mk(13), mk(14)])); println(f"o7 {a6[0].id}");
  let a7 = o8(Some([mk(15), mk(16)])); println(f"o8 {a7[0].id}");
  let a8 = o9(Ok([mk(17), mk(18)])); println(f"o9 {a8[0].id}");
  let a9 = l1(); println(f"l1 {a9[1].id}");
  let a10 = l2(); println(f"l2 {a10[1].id}");
  println(f"l3 {l3()}");
  let a11 = u1(EArr.A([mk(21), mk(22)])); println(f"u1 {a11[0].id}");
  let a12 = u2(EArr.A([mk(23), mk(24)])); println(f"u2 {a12[0].id}");
  let a13 = u3(EArr.A([mk(25), mk(26)])); println(f"u3 {a13[0].id}");
  println(f"u4 {u4(EArr.A([mk(27), mk(28)]))}");
  let a14 = u8(E2.A([mk(29), mk(30)], 3)); println(f"u8 {a14[0].id}");
  let a15 = u9(Esx.A { a: [mk(31), mk(32)] }); println(f"u9 {a15[0].id}");
  let a16 = u10(EArr.A([mk(33), mk(34)])); println("u10");
  let a17 = u11(EArr.A([mk(35), mk(36)]), true); println(f"u11 {a17[0].id}");
  let a18 = u12(); println(f"u12 {a18[0].id}");
  let e = EArr.A([mk(37), mk(38)]); let a19 = u1(e); println(f"u1n {a19[0].id}");
  let a20 = u13(EArr.A([mk(39), mk(40)])); println(f"u13 {a20[0].id}");
  println(f"u15 {u15(EArr.A([mk(41), mk(42)]))}");
  println("end");
}
"#,
        &[
            "o1 1", "dR1", "dR2", "o2 3", "dR3", "dR4", "o3 5", "dR5", "dR6", "dR7", "dR8", "o4 7",
            "dR9", "dR10", "o5", "o6 11", "dR11", "dR12", "o7 13", "dR13", "dR14", "o8 15", "dR15",
            "dR16", "o9 17", "dR17", "dR18", "l1 2", "dR1", "dR2", "l2 2", "dR1", "dR2", "dR1",
            "dR2", "l3 1", "u1 21", "dR21", "dR22", "u2 23", "dR23", "dR24", "u3 25", "dR25",
            "dR26", "dR27", "dR28", "u4 28", "u8 29", "dR29", "dR30", "u9 31", "dR31", "dR32",
            "dR33", "dR34", "u10", "u11 35", "dR35", "dR36", "u12 1", "dR1", "dR2", "u1n 37",
            "dR37", "dR38", "u13 39", "dR39", "dR40", "dR41", "dR42", "u15 42", "end",
        ],
        "asan_boxed_array_payload_with_drop_elems_returned_once",
    );
}

/// B-2026-09-27-83: the guard the other way. A rebind of such a payload out of
/// a by-value param (`Some(t) => { let u = t; .. }`, a chain `let w = u;`, a
/// `Result`, annotated or not) leaves the elements' bodies with the param's
/// walk, which runs them after the arm, so the box keeps their memory until
/// then. The first cut of this fix gave `u` the memory and the walk then ran
/// each body over freed heap (`dS` plus garbage, and a double free on the
/// chain returned out). The returned spellings (`return u`, `return w`) still
/// hand both to the caller.
#[test]
fn asan_boxed_array_payload_with_drop_elems_rebind_keeps_param_walk() {
    assert_clean_asan_run(
        r#"struct S { tag: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.tag}") } }
fn mk(a: String, b: String) -> Array[S, 2] { return [S { tag: a }, S { tag: b }]; }
fn pa(x: Option[Array[S, 2]]) {
    match x { Some(t) => { let u = t; println(f"s{u[0].tag}") } None => { println("n") } }
    println("post");
}
fn pb(x: Option[Array[S, 2]]) -> Array[S, 2] {
    match x { Some(t) => { let u = t; return u; } None => { return mk(f"q", f"r"); } }
}
fn pc(x: Option[Array[S, 2]]) {
    match x { Some(t) => { let u = t; let w = u; println(f"s{w[1].tag}") } None => { } }
    println("post");
}
fn pd(x: Option[Array[S, 2]]) -> Array[S, 2] {
    match x { Some(t) => { let u = t; let w = u; return w; } None => { return mk(f"q", f"r"); } }
}
fn pe(x: Result[Array[S, 2], i64]) {
    match x { Ok(t) => { let u: Array[S, 2] = t; println(f"s{u[0].tag}") } Err(_) => { } }
    println("post");
}
fn pf(x: Option[Array[S, 2]], c: bool) -> Array[S, 2] {
    match x { Some(t) => { let u = t; if c { return u; } println("kept"); return mk(f"q", f"r"); } None => { return mk(f"q", f"r"); } }
}
fn la() {
    let o: Option[Array[S, 2]] = Some(mk(f"1", f"2"));
    match o { Some(t) => { let u = t; println(f"s{u[0].tag}") } None => { } }
    println("post");
}
fn lb() -> Array[S, 2] {
    let o: Option[Array[S, 2]] = Some(mk(f"1", f"2"));
    match o { Some(t) => { let u = t; return u; } None => { return mk(f"q", f"r"); } }
}
fn main() {
    println("a"); pa(Some(mk(f"1", f"2")));
    println("c"); pc(Some(mk(f"3", f"4")));
    let d = pd(Some(mk(f"5", f"6"))); println(f"d{d[0].tag}");
    pe(Ok(mk(f"7", f"8")));
    let f1 = pf(Some(mk(f"9", f"10")), true); println(f"f{f1[0].tag}");
    let b = pb(Some(mk(f"11", f"12"))); println(f"b{b[0].tag}");
    println("h"); la();
    println("end");
}
"#,
        &[
            "a", "s1", "post", "dS1", "dS2", "c", "s4", "post", "dS3", "dS4", "d5", "dS5", "dS6",
            "s7", "post", "dS7", "dS8", "f9", "dS9", "dS10", "b11", "dS11", "dS12", "h", "s1",
            "dS1", "dS2", "post", "end",
        ],
        "asan_boxed_array_payload_with_drop_elems_rebind_keeps_param_walk",
    );
}
