//! Pins for the rows the 2026-10-05 drop-schedule sweep found fixed on main
//! (34088fa5d): B-2026-09-20-64, B-2026-09-23-6, B-2026-09-23-41, B-2026-09-26-24,
//! B-2026-09-26-39, B-2026-09-26-51, B-2026-09-27-34, B-2026-09-27-61, B-2026-09-27-112,
//! B-2026-09-28-26, B-2026-09-28-35, B-2026-09-29-48, B-2026-09-29-65, B-2026-09-29-80,
//! B-2026-09-29-86 and B-2026-10-01-22. Each runs the row's own cell, whose output
//! now matches the row's due answer on every surface. Cells live in
//! `/mnt/attach/project-files/dropsched/sweep1005/k*/`.

use super::*;

/// B-2026-09-23-6: `B-2026-09-23-6_1`.
#[test]
fn asan_sweep_b_2026_09_23_6_1() {
    assert_clean_asan_run(
        r##"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
struct N { id: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"  dN{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"aaa" } }
struct B1 { v: Array[R, 2] }
struct B2 { v: Array[R, 2], t: String }
struct B3 { v: Array[R, 3] }
struct B4 { v: Array[N, 2] }
struct B6 { v: Array[String, 2] }
fn s_bind(a: Array[R, 2]) -> i64 { let b = B1 { v: a }; println("  in"); return 7 }
fn s_two(a: Array[R, 2]) -> i64 { let b = B2 { v: a, t: f"zz" }; println(f"  in:{b.t}"); return 7 }
fn s_three(a: Array[R, 3]) -> i64 { let b = B3 { v: a }; println("  in"); return 7 }
fn s_noheap(a: Array[N, 2]) -> i64 { let b = B4 { v: a }; println("  in"); return 7 }
fn s_read(a: Array[R, 2]) -> i64 { let b = B1 { v: a }; println(f"  in:{b.v[0].id}"); return 7 }
fn n_str(a: Array[String, 2]) -> i64 { let b = B6 { v: a }; println(f"  in:{b.v[0]}"); return 7 }
fn n_tuple(a: Array[R, 2]) -> i64 { let b = (a, 1); println("  in"); return 7 }
fn b_local() -> i64 { let a: Array[R, 2] = [mkr(91), mkr(92)]; let b = B1 { v: a }; println("  in"); return 7 }
fn b_discard(a: Array[R, 2]) -> i64 { B1 { v: a }; println("  in"); return 7 }
fn b_ctl(a: Array[R, 2]) -> i64 { println("  in"); return 7 }
fn main() {
    println("s/bind");    { let a: Array[R, 2] = [mkr(1), mkr(2)]; let z = s_bind(a); }
    println("s/two");     { let a: Array[R, 2] = [mkr(11), mkr(12)]; let z = s_two(a); }
    println("s/three");   { let a: Array[R, 3] = [mkr(21), mkr(22), mkr(23)]; let z = s_three(a); }
    println("s/noheap");  { let a: Array[N, 2] = [N { id: 31 }, N { id: 32 }]; let z = s_noheap(a); }
    println("s/read");    { let a: Array[R, 2] = [mkr(41), mkr(42)]; let z = s_read(a); }
    println("n/str");     { let a: Array[String, 2] = [f"s51", f"s52"]; let z = n_str(a); }
    println("n/tuple");   { let a: Array[R, 2] = [mkr(71), mkr(72)]; let z = n_tuple(a); }
    println("b/local");   { let z = b_local(); }
    println("b/discard"); { let a: Array[R, 2] = [mkr(101), mkr(102)]; let z = b_discard(a); }
    println("b/ctl");     { let a: Array[R, 2] = [mkr(111), mkr(112)]; let z = b_ctl(a); }
    println("end")
}
"##,
        &[
            "s/bind",
            "  in",
            "  d1",
            "  d2",
            "s/two",
            "  in:zz",
            "  d11",
            "  d12",
            "s/three",
            "  in",
            "  d21",
            "  d22",
            "  d23",
            "s/noheap",
            "  in",
            "  dN31",
            "  dN32",
            "s/read",
            "  in:41",
            "  d41",
            "  d42",
            "n/str",
            "  in:s51",
            "n/tuple",
            "  in",
            "  d71",
            "  d72",
            "b/local",
            "  d91",
            "  d92",
            "  in",
            "b/discard",
            "  in",
            "  d101",
            "  d102",
            "b/ctl",
            "  in",
            "  d111",
            "  d112",
            "end",
        ],
        "sweep_b_2026_09_23_6_1",
    );
}

/// B-2026-09-27-112: `B-2026-09-27-112_1`.
#[test]
fn asan_sweep_b_2026_09_27_112_1() {
    assert_clean_asan_run(
        r##"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn rb(a: S, f: bool) -> i64 { let mut v: Vec[S] = Vec.new(); if f { v.push(a) } println("in"); 5 }
fn main() {
    println(f"k{rb(mk(1), true)}");
    println(f"k{rb(mk(2), false)}");
    let x = mk(3);
    println(f"k{rb(x, true)}");
    println("end")
}
"##,
        &["d1", "in", "k5", "in", "d2", "k5", "d3", "in", "k5", "end"],
        "sweep_b_2026_09_27_112_1",
    );
}

/// B-2026-09-27-112: `B-2026-09-27-112_2`.
#[test]
fn asan_sweep_b_2026_09_27_112_2() {
    assert_clean_asan_run(
        r##"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn rb(a: S, f: bool) -> i64 { let mut v: Vec[S] = Vec.new(); if f { v.push(a) } println("in"); 5 }
fn main() {
    println(f"k{rb(mk(1), true)}");
    println(f"k{rb(mk(2), false)}");
    let x = mk(3);
    println(f"k{rb(x, false)}");
    println("end")
}
"##,
        &["d1", "in", "k5", "in", "d2", "k5", "in", "d3", "k5", "end"],
        "sweep_b_2026_09_27_112_2",
    );
}

/// B-2026-09-27-112: `B-2026-09-27-112_3`.
#[test]
fn asan_sweep_b_2026_09_27_112_3() {
    assert_clean_asan_run(
        r##"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn rb(a: S, f: bool) -> i64 { let mut v: Vec[S] = Vec.new(); if f { v.push(a) } println("in"); 5 }
fn main() {
    println(f"k{rb(mk(1), true)}");
    println(f"k{rb(mk(2), false)}");
    
    println(f"k{rb(mk(3), true)}");
    println("end")
}
"##,
        &["d1", "in", "k5", "in", "d2", "k5", "d3", "in", "k5", "end"],
        "sweep_b_2026_09_27_112_3",
    );
}

/// B-2026-09-28-35: `B-2026-09-28-35_1`.
#[test]
fn asan_sweep_b_2026_09_28_35_1() {
    assert_clean_asan_run(
        r##"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn id(a: Result[S, i64]) -> Result[S, i64] { a }
fn ido(a: Option[S]) -> Option[S] { a }
fn peek(a: Result[S, i64]) -> i64 { match id(a) { Ok(x) => x.r.id + x.s.len(), Err(e) => 0 } }
fn main() {
    let a: Result[S, i64] = Ok(mk(1)); let k = peek(a); println(f"k{k}");
    println("end")
}
"##,
        &["d1", "k30", "end"],
        "sweep_b_2026_09_28_35_1",
    );
}

/// B-2026-09-28-35: `B-2026-09-28-35_2`.
#[test]
fn asan_sweep_b_2026_09_28_35_2() {
    assert_clean_asan_run(
        r##"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn id(a: Result[S, i64]) -> Result[S, i64] { a }
fn ido(a: Option[S]) -> Option[S] { a }
fn peek(a: Result[S, i64]) -> i64 { match id(a) { Ok(x) => x.r.id + x.s.len(), Err(_) => 0 } }
fn main() {
    let a: Result[S, i64] = Ok(mk(2)); let k = peek(a); println(f"k{k}");
    println("end")
}
"##,
        &["d2", "k31", "end"],
        "sweep_b_2026_09_28_35_2",
    );
}

/// B-2026-09-28-35: `B-2026-09-28-35_3`.
#[test]
fn asan_sweep_b_2026_09_28_35_3() {
    assert_clean_asan_run(
        r##"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn id(a: Result[S, i64]) -> Result[S, i64] { a }
fn ido(a: Option[S]) -> Option[S] { a }
fn peek(a: Result[S, i64]) -> i64 { match id(a) { Ok(x) => x.r.id + x.s.len(), Err(e) => e } }
fn main() {
    let a: Result[S, i64] = Ok(mk(3)); let k = peek(a); println(f"k{k}");
    println("end")
}
"##,
        &["d3", "k32", "end"],
        "sweep_b_2026_09_28_35_3",
    );
}

/// B-2026-09-28-35: `B-2026-09-28-35_4`.
#[test]
fn asan_sweep_b_2026_09_28_35_4() {
    assert_clean_asan_run(
        r##"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn id(a: Result[S, i64]) -> Result[S, i64] { a }
fn ido(a: Option[S]) -> Option[S] { a }
fn peek(a: Result[S, i64]) -> i64 { match id(a) { Ok(x) => x.r.id, Err(e) => 0 } }
fn main() {
    let k = peek(Ok(mk(4))); println(f"k{k}");
    println("end")
}
"##,
        &["d4", "k4", "end"],
        "sweep_b_2026_09_28_35_4",
    );
}

/// B-2026-09-20-64: `B-2026-09-20-64_1`.
#[test]
fn asan_sweep_b_2026_09_20_64_1() {
    assert_clean_asan_run(
        r##"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i } }
enum Slot[T] { S(T), N }
fn seen(x: Slot[Array[R, 2]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }
fn main() { let a: Array[R, 2] = [mkr(1), mkr(2)]; seen(Slot.S(a)); println("end") }
"##,
        &["x1", "dR1", "dR2", "end"],
        "sweep_b_2026_09_20_64_1",
    );
}

/// B-2026-09-29-86: `B-2026-09-29-86_1`.
#[test]
fn asan_sweep_b_2026_09_29_86_1() {
    assert_clean_asan_run(
        r##"shared struct ShIn { s: String }
struct Q { i: ShIn, n: i64, s: String }
fn mkq(k: i64) -> Q { Q { i: ShIn { s: f"inner-heap-string-longer-than-sso-{k}" }, n: k, s: f"outer-heap-string-longer-than-sso-{k}" } }
fn main() { let a = Some(mkq(1)); let r = match a { Some(Q { i, n, s }) => n, None => 0 }; println(f"a{r}"); println("end"); }
"##,
        &["a1", "end"],
        "sweep_b_2026_09_29_86_1",
    );
}

/// B-2026-09-27-61: `B-2026-09-27-61_1`.
#[test]
fn asan_sweep_b_2026_09_27_61_1() {
    assert_clean_asan_run(
        r##"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"tag-string-longer-than-sso-{i}" } }
fn two(a: R, b: R) -> i64 { return a.id + b.id; }
fn one(b: R) -> i64 { return b.id; }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
impl E {
  fn both(self, o: R) -> i64    { match self { E.A(r) => { return two(r, o); } E.B => { return 0; } } }
  fn onlyo(self, o: R) -> i64   { match self { E.A(r) => { return one(o); } E.B => { return 0; } } }
  fn nomatch(self, o: R) -> i64 { return one(o); }
  fn fresh(self, o: R) -> i64   { match self { E.A(r) => { return two(mk(7), o); } E.B => { return 0; } } }
}
fn main() {
    { let a: E = E.A(mk(6)); println(f"  x{a.both(mk(60))}") }
}
"##,
        &["dR6", "  dR60", "  x66", "  dE"],
        "sweep_b_2026_09_27_61_1",
    );
}

/// B-2026-09-27-61: `B-2026-09-27-61_2`.
#[test]
fn asan_sweep_b_2026_09_27_61_2() {
    assert_clean_asan_run(
        r##"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"tag-string-longer-than-sso-{i}" } }
fn two(a: R, b: R) -> i64 { return a.id + b.id; }
fn one(b: R) -> i64 { return b.id; }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
impl E {
  fn both(self, o: R) -> i64    { match self { E.A(r) => { return two(r, o); } E.B => { return 0; } } }
  fn onlyo(self, o: R) -> i64   { match self { E.A(r) => { return one(o); } E.B => { return 0; } } }
  fn nomatch(self, o: R) -> i64 { return one(o); }
  fn fresh(self, o: R) -> i64   { match self { E.A(r) => { return two(mk(7), o); } E.B => { return 0; } } }
}
fn main() {
    { let a: E = E.A(mk(1)); println(f"  x{a.onlyo(mk(10))}") }
    { let a: E = E.A(mk(2)); println(f"  x{a.nomatch(mk(20))}") }
    { let a: E = E.A(mk(3)); println(f"  x{a.fresh(mk(30))}") }
}
"##,
        &[
            "dR10", "  x10", "  dE", "  dR1", "  dR20", "  x20", "  dE", "  dR2", "  dR7",
            "  dR30", "  x37", "  dE", "  dR3",
        ],
        "sweep_b_2026_09_27_61_2",
    );
}

/// B-2026-09-29-80: `B-2026-09-29-80_1`.
#[test]
fn asan_sweep_b_2026_09_29_80_1() {
    assert_clean_asan_run(
        r##"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mk(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
struct Q[U] { u: U, n: i64 }
fn f[U](q: Q[U]) -> i64 { let x = q.u; let Q { n, .. } = q; return n }
fn main() { let r = f(Q { u: mk(9), n: 2 }); println(f"r{r}"); println("end") }
"##,
        &["dS9", "r2", "end"],
        "sweep_b_2026_09_29_80_1",
    );
}

/// B-2026-09-26-39: `B-2026-09-26-39_1`.
#[test]
fn asan_sweep_b_2026_09_26_39_1() {
    assert_clean_asan_run(
        r##"enum Gh[T] { Y(T, String), N }
fn gh(g: Gh[Array[String, 2]]) { println("  in") }
fn main() {
    { let a: Array[String, 2] = [f"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", f"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"];
      let w: Gh[Array[String, 2]] = Gh.Y(a, f"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"); gh(w); println("  mid") }
    println("done")
}
"##,
        &["in", "  mid", "done"],
        "sweep_b_2026_09_26_39_1",
    );
}

/// B-2026-09-26-39: `B-2026-09-26-39_2`.
#[test]
fn asan_sweep_b_2026_09_26_39_2() {
    assert_clean_asan_run(
        r##"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum Gh[T] { Y(T, String), N }
fn gh(g: Gh[Array[R, 2]]) { println("  in") }
fn main() {
    { let a: Array[R, 2] = [R { id: 1 }, R { id: 2 }];
      let w: Gh[Array[R, 2]] = Gh.Y(a, f"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"); gh(w); println("  mid") }
    println("done")
}
"##,
        &["in", "dR1", "dR2", "  mid", "done"],
        "sweep_b_2026_09_26_39_2",
    );
}

/// B-2026-09-26-51: `B-2026-09-26-51_1`.
#[test]
fn asan_sweep_b_2026_09_26_51_1() {
    assert_clean_asan_run(
        r##"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}n{self.id}") } }
fn mkd(n: i64) -> D { D { id: n, name: f"heap-name-string-longer-than-sso-{n}" } }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
fn maybew(x: W, c: bool) -> Option[W] { if c { return Some(x); } None }
fn gcsw[T](v: mut ref Vec[T], x: T, c: bool) { if c { v.push(x); } }
fn main() { let w = mkw(7); let o = maybew(w, true); println(f"o{o.is_some()}"); println("end") }
"##,
        &["otrue", "dD107n107", "dD7n7", "end"],
        "sweep_b_2026_09_26_51_1",
    );
}

/// B-2026-09-26-51: `B-2026-09-26-51_2`.
#[test]
fn asan_sweep_b_2026_09_26_51_2() {
    assert_clean_asan_run(
        r##"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}n{self.id}") } }
fn mkd(n: i64) -> D { D { id: n, name: f"heap-name-string-longer-than-sso-{n}" } }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
fn maybew(x: W, c: bool) -> Option[W] { if c { return Some(x); } None }
fn gcsw[T](v: mut ref Vec[T], x: T, c: bool) { if c { v.push(x); } }
fn main() { let mut v: Vec[W] = Vec.new(); let w = mkw(7); gcsw(mut v, w, true); println(f"o{v.len()}"); println("end") }
"##,
        &["o1", "dD107n107", "dD7n7", "end"],
        "sweep_b_2026_09_26_51_2",
    );
}

/// B-2026-09-29-48: `B-2026-09-29-48_1`.
#[test]
fn asan_sweep_b_2026_09_29_48_1() {
    assert_clean_asan_run(
        r##"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mk(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
struct W { u: S2 }
fn f(q0: W) -> i64 { let mut q = q0; let x = q.u; q.u = mk(8); return x.id + q.u.id }
fn main() { println(f"x{f(W { u: mk(9) })}"); println("end") }
"##,
        &["dS9", "dS8", "x17", "end"],
        "sweep_b_2026_09_29_48_1",
    );
}

/// B-2026-09-29-48: `B-2026-09-29-48_2`.
#[test]
fn asan_sweep_b_2026_09_29_48_2() {
    assert_clean_asan_run(
        r##"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mk(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
struct W { u: S2 }
fn f(q0: W) -> i64 { let mut q = q0; let x = q.u; q.u = mk(8); return x.id + q.u.id }
fn main() { let w = W { u: mk(9) }; println(f"x{f(w)}"); println("end") }
"##,
        &["dS9", "dS8", "x17", "end"],
        "sweep_b_2026_09_29_48_2",
    );
}

/// B-2026-09-29-48: `B-2026-09-29-48_3`.
#[test]
fn asan_sweep_b_2026_09_29_48_3() {
    assert_clean_asan_run(
        r##"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mk(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
struct W { u: S2 }
fn f(q0: W) -> i64 { let mut q = q0; let x = q.u; q.u = mk(8); return 1 }
fn main() { println(f"x{f(W { u: mk(9) })}"); println("end") }
"##,
        &["dS9", "dS8", "x1", "end"],
        "sweep_b_2026_09_29_48_3",
    );
}

/// B-2026-09-29-48: `B-2026-09-29-48_4`.
#[test]
fn asan_sweep_b_2026_09_29_48_4() {
    assert_clean_asan_run(
        r##"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mk(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
struct W { u: S2 }
fn f(q0: W) -> i64 { let mut q = q0; q.u = mk(8); return q.u.id }
fn main() { println(f"x{f(W { u: mk(9) })}"); println("end") }
"##,
        &["dS9", "dS8", "x8", "end"],
        "sweep_b_2026_09_29_48_4",
    );
}

/// B-2026-09-23-41: `B-2026-09-23-41_1`.
#[test]
fn asan_sweep_b_2026_09_23_41_1() {
    assert_clean_asan_run(
        r##"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn f(a: Option[R], c: bool) -> Option[R] { if c { a } else { None } }
fn main() { let b = f(Some(mkr(1)), false); println("got"); println("end-main") }
"##,
        &["d1", "got", "end-main"],
        "sweep_b_2026_09_23_41_1",
    );
}

/// B-2026-09-23-41: `B-2026-09-23-41_2`.
#[test]
fn asan_sweep_b_2026_09_23_41_2() {
    assert_clean_asan_run(
        r##"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn f(a: Option[R], c: bool) -> Option[R] { if c { return a } None }
fn main() { let b = f(Some(mkr(1)), false); println("got"); println("end-main") }
"##,
        &["d1", "got", "end-main"],
        "sweep_b_2026_09_23_41_2",
    );
}

/// B-2026-09-23-41: `B-2026-09-23-41_3`.
#[test]
fn asan_sweep_b_2026_09_23_41_3() {
    assert_clean_asan_run(
        r##"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn f(a: Option[R], c: bool) -> Option[R] { let r: Option[R] = if c { a } else { None }; println("mid"); r }
fn main() { let b = f(Some(mkr(1)), false); println("got"); println("end-main") }
"##,
        &["mid", "d1", "got", "end-main"],
        "sweep_b_2026_09_23_41_3",
    );
}

/// B-2026-09-27-34: `B-2026-09-27-34_1`.
#[test]
fn asan_sweep_b_2026_09_27_34_1() {
    assert_clean_asan_run(
        r##"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn id(a: Result[S, i64]) -> Result[S, i64] { a }
fn f(a: Result[S, i64]) -> i64 { let b = id(a); match b { Ok(x) => x.r.id, Err(e) => e } }
fn main() { let a: Result[S, i64] = Ok(mk(1)); let k = f(a); println(f"k{k}"); println("end") }
"##,
        &["d1", "k1", "end"],
        "sweep_b_2026_09_27_34_1",
    );
}

/// B-2026-09-28-26: `B-2026-09-28-26_1`.
#[test]
fn asan_sweep_b_2026_09_28_26_1() {
    assert_clean_asan_run(
        r##"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn peek(o: Option[Q]) { match o { Option.Some(t) => { println(f"  r:{t.r.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn ttake(o: Option[(R, R)]) { match o { Option.Some(t) => { let x = t.0; println(f"  r:{x.v}s:{t.1.v}") } Option.None => { println("  n") } } }
fn h(a: Option[Q]) { peek(a); let a = Option.Some((mkr(7), mkr(8))); ttake(a); println("  out") }
fn main() { h(Option.Some(Q { r: mkr(5), s: mkr(6) })); println("end") }
"##,
        &[
            "r:50s:60",
            "  r:70s:80",
            "  dR7v70",
            "  dR8v80",
            "  out",
            "  dR6v60",
            "  dR5v50",
            "end",
        ],
        "sweep_b_2026_09_28_26_1",
    );
}

/// B-2026-09-29-65: `B-2026-09-29-65_1`.
#[test]
fn asan_sweep_b_2026_09_29_65_1() {
    assert_clean_asan_run(
        r##"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn q1(t: Option[S], c: bool) -> i64 { if c { let u = match t { Option.Some(v) => v, n => n.unwrap() }; return u.id }; let t = Option.Some(mks(99)); println("q1"); return 1 }
fn main() { let r = q1(Option.Some(mks(7)), false); println(f"r{r}"); println("end") }
"##,
        &["dS99", "q1", "dS7", "r1", "end"],
        "sweep_b_2026_09_29_65_1",
    );
}

/// B-2026-09-29-65: `B-2026-09-29-65_2`.
#[test]
fn asan_sweep_b_2026_09_29_65_2() {
    assert_clean_asan_run(
        r##"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn q2(t: Option[S], c: bool) -> i64 { if c { let u = match t { Option.Some(v) => v, n => n.unwrap() }; return u.id }; println("q2"); return 1 }
fn main() { let r = q2(Option.Some(mks(8)), false); println(f"r{r}"); println("end") }
"##,
        &["q2", "dS8", "r1", "end"],
        "sweep_b_2026_09_29_65_2",
    );
}

/// B-2026-09-26-24: `B-2026-09-26-24_1`.
#[test]
fn asan_sweep_b_2026_09_26_24_1() {
    assert_clean_asan_run(
        r##"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
enum HoS { FullS(S2), EmptyS }
fn rd(x: ref S2) { println(f"rd{x.id}") }
fn mk2(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
fn mkHoS(s: S2) -> HoS { return HoS.FullS(s) }
fn go() { let o = HoS.FullS(mk2(9)); match o { HoS.FullS(x) => rd(x), HoS.EmptyS => println("e") } }
fn main() { go(); println("end") }
"##,
        &["rd9", "dS9", "end"],
        "sweep_b_2026_09_26_24_1",
    );
}

/// B-2026-09-26-24: `B-2026-09-26-24_2`.
#[test]
fn asan_sweep_b_2026_09_26_24_2() {
    assert_clean_asan_run(
        r##"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
enum HoS { FullS(S2), EmptyS }
fn rd(x: ref S2) { println(f"rd{x.id}") }
fn mk2(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
fn mkHoS(s: S2) -> HoS { return HoS.FullS(s) }
fn go() { let o = HoS.FullS(mk2(9)); if let HoS.FullS(x) = o { rd(x) } }
fn main() { go(); println("end") }
"##,
        &["rd9", "dS9", "end"],
        "sweep_b_2026_09_26_24_2",
    );
}

/// B-2026-09-26-24: `B-2026-09-26-24_3`.
#[test]
fn asan_sweep_b_2026_09_26_24_3() {
    assert_clean_asan_run(
        r##"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
enum HoS { FullS(S2), EmptyS }
fn rd(x: ref S2) { println(f"rd{x.id}") }
fn mk2(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
fn mkHoS(s: S2) -> HoS { return HoS.FullS(s) }
fn go() { let s = mk2(9); let o = mkHoS(s); match o { HoS.FullS(x) => rd(x), HoS.EmptyS => println("e") } }
fn main() { go(); println("end") }
"##,
        &["rd9", "dS9", "end"],
        "sweep_b_2026_09_26_24_3",
    );
}

/// B-2026-09-26-24: `B-2026-09-26-24_4`.
#[test]
fn asan_sweep_b_2026_09_26_24_4() {
    assert_clean_asan_run(
        r##"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
enum HoS { FullS(S2), EmptyS }
fn rd(x: ref S2) { println(f"rd{x.id}") }
fn mk2(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
fn mkHoS(s: S2) -> HoS { return HoS.FullS(s) }
fn main() { let o = HoS.FullS(mk2(9)); match o { HoS.FullS(x) => rd(x), HoS.EmptyS => println("e") }; println("end") }
"##,
        &["rd9", "dS9", "end"],
        "sweep_b_2026_09_26_24_4",
    );
}

/// B-2026-10-01-22: `B-2026-10-01-22_ct`.
#[test]
fn asan_sweep_b_2026_10_01_22_ct() {
    assert_clean_asan_run(
        r##"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn cm(x: R, c: bool) -> Vec[R] { let mut v = Vec[x]; if c { return v } return Vec[mk(9)] }
fn main() { let a = cm(mk(1), true); println(f"k{a.len()}") }
"##,
        &["k1", "dR1"],
        "sweep_b_2026_10_01_22_ct",
    );
}

/// B-2026-10-01-22: `B-2026-10-01-22_q`.
#[test]
fn asan_sweep_b_2026_10_01_22_q() {
    assert_clean_asan_run(
        r##"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn cm(x: R, c: bool) -> Vec[R] { let mut v = Vec[x]; if c { return v } return Vec[mk(9)] }
fn ci(x: R, c: bool) -> Vec[R] { let v = Vec[x]; if c { return v } return Vec[mk(9)] }
fn f7(x: R) -> Vec[R] { let mut v = Vec[x]; v = Vec[mk(8)]; return v }
fn g7(x: R) { let mut v = Vec[x]; v = Vec[mk(8)]; println("in") }
fn h7() { let w = mk(3); let mut v = Vec[w]; v = Vec[mk(8)]; println("in") }
fn main() {
  println("-ct"); let a = cm(mk(1), true); println(f"k{a.len()}")
  println("-cf"); let b = cm(mk(2), false); println(f"k{b.len()}")
  println("-it"); let c = ci(mk(4), true); println(f"k{c.len()}")
  println("-if"); let d = ci(mk(5), false); println(f"k{d.len()}")
  println("-p7"); let e = f7(mk(17)); println(f"k{e.len()}")
  println("-r3"); g7(mk(6)); println("k")
  println("-r5"); h7(); println("k")
}
"##,
        &[
            "-ct", "k1", "dR1", "-cf", "dR2", "k1", "dR9", "-it", "k1", "dR4", "-if", "dR5", "k1",
            "dR9", "-p7", "dR17", "k1", "dR8", "-r3", "dR8", "in", "dR6", "k", "-r5", "dR3", "dR8",
            "in", "k",
        ],
        "sweep_b_2026_10_01_22_q",
    );
}
