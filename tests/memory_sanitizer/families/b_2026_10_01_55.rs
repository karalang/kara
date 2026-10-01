//! B-2026-10-01-55 -- a heap element read out of a named fixed `Array` into an
//! owning sink is deep-copied, as its `Vec` twin always was.

use super::*;

/// B-2026-10-01-55 — a `String` element read out of a named fixed `Array`
/// into an owning sink. Before the fix only the constant-index `return` and
/// tail over a local or by-value root were clean: a dynamic index, a tuple, a
/// `Some`, a `push` and every `ref`-param read handed the destination a
/// `{ptr,len,cap}` alias of the element, and the array's element drop and the
/// destination freed one buffer.
#[test]
fn asan_array_element_read_into_an_owning_sink_is_copied_string() {
    assert_clean_asan_run(
        r#"
fn mk(i: i64) -> String { return f"pppppppppppppppppppppp{i}"; }
fn show(x: ref String) -> String { return x.clone(); }
struct W { a: String }
fn local_ret0(i: i64) -> String { let a: Array[String, 2] = [mk(1), mk(2)]; return a[0]; }
fn local_reti(i: i64) -> String { let a: Array[String, 2] = [mk(1), mk(2)]; return a[i]; }
fn local_tail(i: i64) -> String { let a: Array[String, 2] = [mk(1), mk(2)]; a[1] }
fn local_tup(i: i64) -> (String, i64) { let a: Array[String, 2] = [mk(1), mk(2)]; return (a[0], 3); }
fn local_some(i: i64) -> Option[String] { let a: Array[String, 2] = [mk(1), mk(2)]; return Some(a[i]); }
fn local_push(i: i64) -> Vec[String] { let a: Array[String, 2] = [mk(1), mk(2)]; let mut v: Vec[String] = Vec.new(); v.push(a[0]); v.push(a[i]); return v; }
fn local_fld(i: i64) -> W { let a: Array[String, 2] = [mk(1), mk(2)]; return W { a: a[0] }; }
fn local_fldi(i: i64) -> W { let a: Array[String, 2] = [mk(1), mk(2)]; return W { a: a[i] }; }
fn own_ret0(p: Array[String, 2], i: i64) -> String { return p[0]; }
fn own_reti(p: Array[String, 2], i: i64) -> String { return p[i]; }
fn own_tail(p: Array[String, 2], i: i64) -> String { p[1] }
fn own_tup(p: Array[String, 2], i: i64) -> (String, i64) { return (p[0], 3); }
fn own_some(p: Array[String, 2], i: i64) -> Option[String] { return Some(p[i]); }
fn own_push(p: Array[String, 2], i: i64) -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(p[0]); v.push(p[i]); return v; }
fn own_fld(p: Array[String, 2], i: i64) -> W { return W { a: p[0] }; }
fn own_fldi(p: Array[String, 2], i: i64) -> W { return W { a: p[i] }; }
fn ref_ret0(p: ref Array[String, 2], i: i64) -> String { return p[0]; }
fn ref_reti(p: ref Array[String, 2], i: i64) -> String { return p[i]; }
fn ref_tail(p: ref Array[String, 2], i: i64) -> String { p[1] }
fn ref_tup(p: ref Array[String, 2], i: i64) -> (String, i64) { return (p[0], 3); }
fn ref_some(p: ref Array[String, 2], i: i64) -> Option[String] { return Some(p[i]); }
fn ref_push(p: ref Array[String, 2], i: i64) -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(p[0]); v.push(p[i]); return v; }
fn ref_fld(p: ref Array[String, 2], i: i64) -> W { return W { a: p[0] }; }
fn ref_fldi(p: ref Array[String, 2], i: i64) -> W { return W { a: p[i] }; }
fn main() {
    { let r = local_ret0(1); let s = show(r); println(f"local_ret0 {s}"); }
    { let r = local_reti(1); let s = show(r); println(f"local_reti {s}"); }
    { let r = local_tail(1); let s = show(r); println(f"local_tail {s}"); }
    { let r = local_tup(1); let s = show(r.0); println(f"local_tup {s}"); }
    { let r = local_some(1); let s = match r { Some(x) => show(x), None => "none" }; println(f"local_some {s}"); }
    { let r = local_push(1); let s = f"{show(r[0])}/{show(r[1])}"; println(f"local_push {s}"); }
    { let r = local_fld(1); let s = show(r.a); println(f"local_fld {s}"); }
    { let r = local_fldi(1); let s = show(r.a); println(f"local_fldi {s}"); }
    { let t: Array[String, 2] = [mk(1), mk(2)]; let r = own_ret0(t, 1); let s = show(r); println(f"own_ret0 {s}"); }
    { let t: Array[String, 2] = [mk(1), mk(2)]; let r = own_reti(t, 1); let s = show(r); println(f"own_reti {s}"); }
    { let t: Array[String, 2] = [mk(1), mk(2)]; let r = own_tail(t, 1); let s = show(r); println(f"own_tail {s}"); }
    { let t: Array[String, 2] = [mk(1), mk(2)]; let r = own_tup(t, 1); let s = show(r.0); println(f"own_tup {s}"); }
    { let t: Array[String, 2] = [mk(1), mk(2)]; let r = own_some(t, 1); let s = match r { Some(x) => show(x), None => "none" }; println(f"own_some {s}"); }
    { let t: Array[String, 2] = [mk(1), mk(2)]; let r = own_push(t, 1); let s = f"{show(r[0])}/{show(r[1])}"; println(f"own_push {s}"); }
    { let t: Array[String, 2] = [mk(1), mk(2)]; let r = own_fld(t, 1); let s = show(r.a); println(f"own_fld {s}"); }
    { let t: Array[String, 2] = [mk(1), mk(2)]; let r = own_fldi(t, 1); let s = show(r.a); println(f"own_fldi {s}"); }
    { let t: Array[String, 2] = [mk(1), mk(2)]; let r = ref_ret0(t, 1); let s = show(r); println(f"ref_ret0 {s}"); }
    { let t: Array[String, 2] = [mk(1), mk(2)]; let r = ref_reti(t, 1); let s = show(r); println(f"ref_reti {s}"); }
    { let t: Array[String, 2] = [mk(1), mk(2)]; let r = ref_tail(t, 1); let s = show(r); println(f"ref_tail {s}"); }
    { let t: Array[String, 2] = [mk(1), mk(2)]; let r = ref_tup(t, 1); let s = show(r.0); println(f"ref_tup {s}"); }
    { let t: Array[String, 2] = [mk(1), mk(2)]; let r = ref_some(t, 1); let s = match r { Some(x) => show(x), None => "none" }; println(f"ref_some {s}"); }
    { let t: Array[String, 2] = [mk(1), mk(2)]; let r = ref_push(t, 1); let s = f"{show(r[0])}/{show(r[1])}"; println(f"ref_push {s}"); }
    { let t: Array[String, 2] = [mk(1), mk(2)]; let r = ref_fld(t, 1); let s = show(r.a); println(f"ref_fld {s}"); }
    { let t: Array[String, 2] = [mk(1), mk(2)]; let r = ref_fldi(t, 1); let s = show(r.a); println(f"ref_fldi {s}"); }
    println("end");
}
"#,
        &[
            "local_ret0 pppppppppppppppppppppp1",
            "local_reti pppppppppppppppppppppp2",
            "local_tail pppppppppppppppppppppp2",
            "local_tup pppppppppppppppppppppp1",
            "local_some pppppppppppppppppppppp2",
            "local_push pppppppppppppppppppppp1/pppppppppppppppppppppp2",
            "local_fld pppppppppppppppppppppp1",
            "local_fldi pppppppppppppppppppppp2",
            "own_ret0 pppppppppppppppppppppp1",
            "own_reti pppppppppppppppppppppp2",
            "own_tail pppppppppppppppppppppp2",
            "own_tup pppppppppppppppppppppp1",
            "own_some pppppppppppppppppppppp2",
            "own_push pppppppppppppppppppppp1/pppppppppppppppppppppp2",
            "own_fld pppppppppppppppppppppp1",
            "own_fldi pppppppppppppppppppppp2",
            "ref_ret0 pppppppppppppppppppppp1",
            "ref_reti pppppppppppppppppppppp2",
            "ref_tail pppppppppppppppppppppp2",
            "ref_tup pppppppppppppppppppppp1",
            "ref_some pppppppppppppppppppppp2",
            "ref_push pppppppppppppppppppppp1/pppppppppppppppppppppp2",
            "ref_fld pppppppppppppppppppppp1",
            "ref_fldi pppppppppppppppppppppp2",
            "end",
        ],
        "b_2026_10_01_55_string",
    );
}

/// B-2026-10-01-55 — the plain-struct element spelling of the cells above.
#[test]
fn asan_array_element_read_into_an_owning_sink_is_copied_plain_struct() {
    assert_clean_asan_run(
        r#"
struct Q { id: i64, s: String }
fn mk(i: i64) -> Q { return Q { id: i, s: f"pppppppppppppppppppppp{i}" }; }
fn show(x: ref Q) -> String { return f"q{x.id}:{x.s.len()}"; }
struct W { a: Q }
fn local_ret0(i: i64) -> Q { let a: Array[Q, 2] = [mk(1), mk(2)]; return a[0]; }
fn local_reti(i: i64) -> Q { let a: Array[Q, 2] = [mk(1), mk(2)]; return a[i]; }
fn local_tail(i: i64) -> Q { let a: Array[Q, 2] = [mk(1), mk(2)]; a[1] }
fn local_tup(i: i64) -> (Q, i64) { let a: Array[Q, 2] = [mk(1), mk(2)]; return (a[0], 3); }
fn local_some(i: i64) -> Option[Q] { let a: Array[Q, 2] = [mk(1), mk(2)]; return Some(a[i]); }
fn local_push(i: i64) -> Vec[Q] { let a: Array[Q, 2] = [mk(1), mk(2)]; let mut v: Vec[Q] = Vec.new(); v.push(a[0]); v.push(a[i]); return v; }
fn local_fld(i: i64) -> W { let a: Array[Q, 2] = [mk(1), mk(2)]; return W { a: a[0] }; }
fn local_fldi(i: i64) -> W { let a: Array[Q, 2] = [mk(1), mk(2)]; return W { a: a[i] }; }
fn own_ret0(p: Array[Q, 2], i: i64) -> Q { return p[0]; }
fn own_reti(p: Array[Q, 2], i: i64) -> Q { return p[i]; }
fn own_tail(p: Array[Q, 2], i: i64) -> Q { p[1] }
fn own_tup(p: Array[Q, 2], i: i64) -> (Q, i64) { return (p[0], 3); }
fn own_some(p: Array[Q, 2], i: i64) -> Option[Q] { return Some(p[i]); }
fn own_push(p: Array[Q, 2], i: i64) -> Vec[Q] { let mut v: Vec[Q] = Vec.new(); v.push(p[0]); v.push(p[i]); return v; }
fn own_fld(p: Array[Q, 2], i: i64) -> W { return W { a: p[0] }; }
fn own_fldi(p: Array[Q, 2], i: i64) -> W { return W { a: p[i] }; }
fn ref_ret0(p: ref Array[Q, 2], i: i64) -> Q { return p[0]; }
fn ref_reti(p: ref Array[Q, 2], i: i64) -> Q { return p[i]; }
fn ref_tail(p: ref Array[Q, 2], i: i64) -> Q { p[1] }
fn ref_tup(p: ref Array[Q, 2], i: i64) -> (Q, i64) { return (p[0], 3); }
fn ref_some(p: ref Array[Q, 2], i: i64) -> Option[Q] { return Some(p[i]); }
fn ref_push(p: ref Array[Q, 2], i: i64) -> Vec[Q] { let mut v: Vec[Q] = Vec.new(); v.push(p[0]); v.push(p[i]); return v; }
fn ref_fld(p: ref Array[Q, 2], i: i64) -> W { return W { a: p[0] }; }
fn ref_fldi(p: ref Array[Q, 2], i: i64) -> W { return W { a: p[i] }; }
fn main() {
    { let r = local_ret0(1); let s = show(r); println(f"local_ret0 {s}"); }
    { let r = local_reti(1); let s = show(r); println(f"local_reti {s}"); }
    { let r = local_tail(1); let s = show(r); println(f"local_tail {s}"); }
    { let r = local_tup(1); let s = show(r.0); println(f"local_tup {s}"); }
    { let r = local_some(1); let s = match r { Some(x) => show(x), None => "none" }; println(f"local_some {s}"); }
    { let r = local_push(1); let s = f"{show(r[0])}/{show(r[1])}"; println(f"local_push {s}"); }
    { let r = local_fld(1); let s = show(r.a); println(f"local_fld {s}"); }
    { let r = local_fldi(1); let s = show(r.a); println(f"local_fldi {s}"); }
    { let t: Array[Q, 2] = [mk(1), mk(2)]; let r = own_ret0(t, 1); let s = show(r); println(f"own_ret0 {s}"); }
    { let t: Array[Q, 2] = [mk(1), mk(2)]; let r = own_reti(t, 1); let s = show(r); println(f"own_reti {s}"); }
    { let t: Array[Q, 2] = [mk(1), mk(2)]; let r = own_tail(t, 1); let s = show(r); println(f"own_tail {s}"); }
    { let t: Array[Q, 2] = [mk(1), mk(2)]; let r = own_tup(t, 1); let s = show(r.0); println(f"own_tup {s}"); }
    { let t: Array[Q, 2] = [mk(1), mk(2)]; let r = own_some(t, 1); let s = match r { Some(x) => show(x), None => "none" }; println(f"own_some {s}"); }
    { let t: Array[Q, 2] = [mk(1), mk(2)]; let r = own_push(t, 1); let s = f"{show(r[0])}/{show(r[1])}"; println(f"own_push {s}"); }
    { let t: Array[Q, 2] = [mk(1), mk(2)]; let r = own_fld(t, 1); let s = show(r.a); println(f"own_fld {s}"); }
    { let t: Array[Q, 2] = [mk(1), mk(2)]; let r = own_fldi(t, 1); let s = show(r.a); println(f"own_fldi {s}"); }
    { let t: Array[Q, 2] = [mk(1), mk(2)]; let r = ref_ret0(t, 1); let s = show(r); println(f"ref_ret0 {s}"); }
    { let t: Array[Q, 2] = [mk(1), mk(2)]; let r = ref_reti(t, 1); let s = show(r); println(f"ref_reti {s}"); }
    { let t: Array[Q, 2] = [mk(1), mk(2)]; let r = ref_tail(t, 1); let s = show(r); println(f"ref_tail {s}"); }
    { let t: Array[Q, 2] = [mk(1), mk(2)]; let r = ref_tup(t, 1); let s = show(r.0); println(f"ref_tup {s}"); }
    { let t: Array[Q, 2] = [mk(1), mk(2)]; let r = ref_some(t, 1); let s = match r { Some(x) => show(x), None => "none" }; println(f"ref_some {s}"); }
    { let t: Array[Q, 2] = [mk(1), mk(2)]; let r = ref_push(t, 1); let s = f"{show(r[0])}/{show(r[1])}"; println(f"ref_push {s}"); }
    { let t: Array[Q, 2] = [mk(1), mk(2)]; let r = ref_fld(t, 1); let s = show(r.a); println(f"ref_fld {s}"); }
    { let t: Array[Q, 2] = [mk(1), mk(2)]; let r = ref_fldi(t, 1); let s = show(r.a); println(f"ref_fldi {s}"); }
    println("end");
}
"#,
        &[
            "local_ret0 q1:23",
            "local_reti q2:23",
            "local_tail q2:23",
            "local_tup q1:23",
            "local_some q2:23",
            "local_push q1:23/q2:23",
            "local_fld q1:23",
            "local_fldi q2:23",
            "own_ret0 q1:23",
            "own_reti q2:23",
            "own_tail q2:23",
            "own_tup q1:23",
            "own_some q2:23",
            "own_push q1:23/q2:23",
            "own_fld q1:23",
            "own_fldi q2:23",
            "ref_ret0 q1:23",
            "ref_reti q2:23",
            "ref_tail q2:23",
            "ref_tup q1:23",
            "ref_some q2:23",
            "ref_push q1:23/q2:23",
            "ref_fld q1:23",
            "ref_fldi q2:23",
            "end",
        ],
        "b_2026_10_01_55_plain_struct",
    );
}

/// B-2026-10-01-55 — an element whose type runs a user `Drop` body. A by-value
/// `Array` param of such an element is CALLER-RETAINED, so even the
/// constant-index `return p[0]` was an alias here and double freed. The body
/// grows its own `String`: under the old alias that realloc'd the buffer the
/// destination still held, so each destination printing its original text is
/// what shows the two values are independent (two bodies for one read is the
/// documented permitted copy, B-2026-09-20-54).
#[test]
fn asan_array_element_read_into_an_owning_sink_is_copied_drop_struct() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { self.s.push_str("-grown-past-the-original-capacity"); println(f"dR{self.id}"); } }
fn mk(i: i64) -> R { return R { id: i, s: f"pppppppppppppppppppppp{i}" }; }
fn show(x: ref R) -> String { return f"r{x.id}:{x.s}"; }
struct W { a: R }
fn local_ret0(i: i64) -> R { let a: Array[R, 2] = [mk(1), mk(2)]; return a[0]; }
fn local_reti(i: i64) -> R { let a: Array[R, 2] = [mk(1), mk(2)]; return a[i]; }
fn local_tail(i: i64) -> R { let a: Array[R, 2] = [mk(1), mk(2)]; a[1] }
fn local_tup(i: i64) -> (R, i64) { let a: Array[R, 2] = [mk(1), mk(2)]; return (a[0], 3); }
fn local_some(i: i64) -> Option[R] { let a: Array[R, 2] = [mk(1), mk(2)]; return Some(a[i]); }
fn local_push(i: i64) -> Vec[R] { let a: Array[R, 2] = [mk(1), mk(2)]; let mut v: Vec[R] = Vec.new(); v.push(a[0]); v.push(a[i]); return v; }
fn local_fld(i: i64) -> W { let a: Array[R, 2] = [mk(1), mk(2)]; return W { a: a[0] }; }
fn local_fldi(i: i64) -> W { let a: Array[R, 2] = [mk(1), mk(2)]; return W { a: a[i] }; }
fn own_ret0(p: Array[R, 2], i: i64) -> R { return p[0]; }
fn own_reti(p: Array[R, 2], i: i64) -> R { return p[i]; }
fn own_tail(p: Array[R, 2], i: i64) -> R { p[1] }
fn own_tup(p: Array[R, 2], i: i64) -> (R, i64) { return (p[0], 3); }
fn own_some(p: Array[R, 2], i: i64) -> Option[R] { return Some(p[i]); }
fn own_push(p: Array[R, 2], i: i64) -> Vec[R] { let mut v: Vec[R] = Vec.new(); v.push(p[0]); v.push(p[i]); return v; }
fn own_fld(p: Array[R, 2], i: i64) -> W { return W { a: p[0] }; }
fn own_fldi(p: Array[R, 2], i: i64) -> W { return W { a: p[i] }; }
fn ref_ret0(p: ref Array[R, 2], i: i64) -> R { return p[0]; }
fn ref_reti(p: ref Array[R, 2], i: i64) -> R { return p[i]; }
fn ref_tail(p: ref Array[R, 2], i: i64) -> R { p[1] }
fn ref_tup(p: ref Array[R, 2], i: i64) -> (R, i64) { return (p[0], 3); }
fn ref_some(p: ref Array[R, 2], i: i64) -> Option[R] { return Some(p[i]); }
fn ref_push(p: ref Array[R, 2], i: i64) -> Vec[R] { let mut v: Vec[R] = Vec.new(); v.push(p[0]); v.push(p[i]); return v; }
fn ref_fld(p: ref Array[R, 2], i: i64) -> W { return W { a: p[0] }; }
fn ref_fldi(p: ref Array[R, 2], i: i64) -> W { return W { a: p[i] }; }
fn main() {
    { let r = local_ret0(1); let s = show(r); println(f"local_ret0 {s}"); }
    { let r = local_reti(1); let s = show(r); println(f"local_reti {s}"); }
    { let r = local_tail(1); let s = show(r); println(f"local_tail {s}"); }
    { let r = local_tup(1); let s = show(r.0); println(f"local_tup {s}"); }
    { let r = local_some(1); let s = match r { Some(x) => show(x), None => "none" }; println(f"local_some {s}"); }
    { let r = local_push(1); let s = f"{show(r[0])}/{show(r[1])}"; println(f"local_push {s}"); }
    { let r = local_fld(1); let s = show(r.a); println(f"local_fld {s}"); }
    { let r = local_fldi(1); let s = show(r.a); println(f"local_fldi {s}"); }
    { let t: Array[R, 2] = [mk(1), mk(2)]; let r = own_ret0(t, 1); let s = show(r); println(f"own_ret0 {s}"); }
    { let t: Array[R, 2] = [mk(1), mk(2)]; let r = own_reti(t, 1); let s = show(r); println(f"own_reti {s}"); }
    { let t: Array[R, 2] = [mk(1), mk(2)]; let r = own_tail(t, 1); let s = show(r); println(f"own_tail {s}"); }
    { let t: Array[R, 2] = [mk(1), mk(2)]; let r = own_tup(t, 1); let s = show(r.0); println(f"own_tup {s}"); }
    { let t: Array[R, 2] = [mk(1), mk(2)]; let r = own_some(t, 1); let s = match r { Some(x) => show(x), None => "none" }; println(f"own_some {s}"); }
    { let t: Array[R, 2] = [mk(1), mk(2)]; let r = own_push(t, 1); let s = f"{show(r[0])}/{show(r[1])}"; println(f"own_push {s}"); }
    { let t: Array[R, 2] = [mk(1), mk(2)]; let r = own_fld(t, 1); let s = show(r.a); println(f"own_fld {s}"); }
    { let t: Array[R, 2] = [mk(1), mk(2)]; let r = own_fldi(t, 1); let s = show(r.a); println(f"own_fldi {s}"); }
    { let t: Array[R, 2] = [mk(1), mk(2)]; let r = ref_ret0(t, 1); let s = show(r); println(f"ref_ret0 {s}"); }
    { let t: Array[R, 2] = [mk(1), mk(2)]; let r = ref_reti(t, 1); let s = show(r); println(f"ref_reti {s}"); }
    { let t: Array[R, 2] = [mk(1), mk(2)]; let r = ref_tail(t, 1); let s = show(r); println(f"ref_tail {s}"); }
    { let t: Array[R, 2] = [mk(1), mk(2)]; let r = ref_tup(t, 1); let s = show(r.0); println(f"ref_tup {s}"); }
    { let t: Array[R, 2] = [mk(1), mk(2)]; let r = ref_some(t, 1); let s = match r { Some(x) => show(x), None => "none" }; println(f"ref_some {s}"); }
    { let t: Array[R, 2] = [mk(1), mk(2)]; let r = ref_push(t, 1); let s = f"{show(r[0])}/{show(r[1])}"; println(f"ref_push {s}"); }
    { let t: Array[R, 2] = [mk(1), mk(2)]; let r = ref_fld(t, 1); let s = show(r.a); println(f"ref_fld {s}"); }
    { let t: Array[R, 2] = [mk(1), mk(2)]; let r = ref_fldi(t, 1); let s = show(r.a); println(f"ref_fldi {s}"); }
    println("end");
}
"#,
        &[
            "dR1",
            "dR2",
            "dR1",
            "local_ret0 r1:pppppppppppppppppppppp1",
            "dR1",
            "dR2",
            "dR2",
            "local_reti r2:pppppppppppppppppppppp2",
            "dR1",
            "dR2",
            "dR2",
            "local_tail r2:pppppppppppppppppppppp2",
            "dR1",
            "dR2",
            "dR1",
            "local_tup r1:pppppppppppppppppppppp1",
            "dR1",
            "dR2",
            "dR2",
            "local_some r2:pppppppppppppppppppppp2",
            "dR1",
            "dR2",
            "dR1",
            "dR2",
            "local_push r1:pppppppppppppppppppppp1/r2:pppppppppppppppppppppp2",
            "dR1",
            "dR2",
            "dR1",
            "local_fld r1:pppppppppppppppppppppp1",
            "dR1",
            "dR2",
            "dR2",
            "local_fldi r2:pppppppppppppppppppppp2",
            "dR1",
            "dR2",
            "dR1",
            "own_ret0 r1:pppppppppppppppppppppp1",
            "dR1",
            "dR2",
            "dR2",
            "own_reti r2:pppppppppppppppppppppp2",
            "dR1",
            "dR2",
            "dR2",
            "own_tail r2:pppppppppppppppppppppp2",
            "dR1",
            "dR2",
            "dR1",
            "own_tup r1:pppppppppppppppppppppp1",
            "dR1",
            "dR2",
            "dR2",
            "own_some r2:pppppppppppppppppppppp2",
            "dR1",
            "dR2",
            "dR1",
            "dR2",
            "own_push r1:pppppppppppppppppppppp1/r2:pppppppppppppppppppppp2",
            "dR1",
            "dR2",
            "dR1",
            "own_fld r1:pppppppppppppppppppppp1",
            "dR1",
            "dR2",
            "dR2",
            "own_fldi r2:pppppppppppppppppppppp2",
            "dR1",
            "dR2",
            "dR1",
            "ref_ret0 r1:pppppppppppppppppppppp1",
            "dR1",
            "dR2",
            "dR2",
            "ref_reti r2:pppppppppppppppppppppp2",
            "dR1",
            "dR2",
            "dR2",
            "ref_tail r2:pppppppppppppppppppppp2",
            "dR1",
            "dR2",
            "dR1",
            "ref_tup r1:pppppppppppppppppppppp1",
            "dR1",
            "dR2",
            "dR2",
            "ref_some r2:pppppppppppppppppppppp2",
            "dR1",
            "dR2",
            "dR1",
            "dR2",
            "ref_push r1:pppppppppppppppppppppp1/r2:pppppppppppppppppppppp2",
            "dR1",
            "dR2",
            "dR1",
            "ref_fld r1:pppppppppppppppppppppp1",
            "dR1",
            "dR2",
            "dR2",
            "ref_fldi r2:pppppppppppppppppppppp2",
            "end",
        ],
        "b_2026_10_01_55_drop_struct",
    );
}
