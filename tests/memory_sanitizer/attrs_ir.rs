//! inline/section/linkage attributes and emitted-IR shape -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer attrs_ir::
//!
//! New fixtures about inline/section/linkage attributes and emitted-IR shape belong in this file.

use super::*;

/// B-2026-09-04-1's probe — moving a `Result[<inline struct>, _]` local, or a
/// destructure leaf over one, by REBIND or as a match arm's TAIL VALUE.
///
/// No destructure is needed: a plain `let r: Result[R, String] = Result.Ok(..)`
/// aborted on `let c = r;` (`rebind`, glibc `free(): double free detected in
/// tcache 2` on an ordinary build) and ran a phantom body over freed memory on
/// `let g = match r { Ok(x) => x, .. }` (`esc`, `escerr`, `esciflet`: `dR3/`
/// with an empty tag, then the real line). `R { id, tag: String }` is four words,
/// INLINE in a five-word `Result` and BOXED in a three-word `Option` — the axis
/// every neighbouring control sits on: `optesc` (boxed), `strreb` / `stresc`
/// (direct String), `call` (the arg site retracts), `inner` (the `let` site
/// retracts). `treb` / `tesc` / `freb` / `fesc` are the tuple and struct-field
/// leaves over the same type; `trebu` is the rebind whose destination is never
/// read (the leak the first cut of this fix opened). The `w*` cells are the
/// seven-word boxed twin of each.
///
/// ASAN twin of `e2e_inline_struct_result_transfer_disarms_its_source` (tests/codegen.rs): the cells that aborted did so
/// on a double free, and the fresh-source cells leaked, so the pin here is
/// the balance itself; the stdout expectation is the same as the E2E's.
#[test]
fn asan_inline_struct_result_transfer_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct W { id: i64, x: String, y: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.x}{self.y}") } }
fn mkw(n: i64) -> W { return W { id: n, x: f"x{n}", y: f"y{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct HoW { a: R, b: Result[W, String] }
fn eat(x: R) { println(f"  eat{x.id}") }

fn rebind()   { let r: Result[R, String] = Result.Ok(mk(1)); let c = r;
                match c { Result.Ok(x) => println(f"  ok{x.id}"), Result.Err(e) => println(f"  er{e}") } }
fn rebindu()  { let r: Result[R, String] = Result.Ok(mk(2)); let c = r; println("  m") }
fn esc()      { let r: Result[R, String] = Result.Ok(mk(3)); let g = match r { Result.Ok(x) => x, Result.Err(e) => mk(0) }; println(f"  got{g.id}") }
fn escerr()   { let r: Result[String, R] = Result.Err(mk(4)); let g = match r { Result.Ok(x) => mk(0), Result.Err(e) => e }; println(f"  got{g.id}") }
fn esciflet() { let r: Result[R, String] = Result.Ok(mk(5)); let g = if let Result.Ok(x) = r { x } else { mk(0) }; println(f"  got{g.id}") }
fn call()     { let r: Result[R, String] = Result.Ok(mk(6)); match r { Result.Ok(x) => eat(x), Result.Err(e) => println(f"  er{e}") } }
fn inner()    { let r: Result[R, String] = Result.Ok(mk(7)); match r { Result.Ok(x) => { let g = x; println(f"  got{g.id}") }, Result.Err(e) => println(f"  er{e}") } }
fn strreb()   { let r: Result[String, String] = Result.Ok("s8"); let c = r;
                match c { Result.Ok(x) => println(f"  ok{x}"), Result.Err(e) => println(f"  er{e}") } }
fn stresc()   { let r: Result[String, i64] = Result.Ok("s9"); let g = match r { Result.Ok(x) => x, Result.Err(e) => "z" }; println(f"  got{g}") }
fn optesc()   { let r: Option[R] = Option.Some(mk(10)); let g = match r { Option.Some(x) => x, Option.None => mk(0) }; println(f"  got{g.id}") }
fn treb()     { let t: (R, Result[R, String]) = (mk(11), Result.Ok(mk(111))); let (a, b) = t; let c = b;
                match c { Result.Ok(x) => println(f"  ok{x.id}"), Result.Err(e) => println(f"  er{e}") } }
fn trebu()    { let t: (R, Result[R, String]) = (mk(12), Result.Ok(mk(112))); let (a, b) = t; let c = b; println("  m") }
fn tesc()     { let t: (R, Result[R, String]) = (mk(13), Result.Ok(mk(113))); let (a, b) = t;
                let g = match b { Result.Ok(x) => x, Result.Err(e) => mk(0) }; println(f"  got{g.id}") }
fn freb()     { let h = HoRes { a: mk(14), b: Result.Ok(mk(114)) }; let HoRes { a, b } = h; let c = b;
                match c { Result.Ok(x) => println(f"  ok{x.id}"), Result.Err(e) => println(f"  er{e}") } }
fn fesc()     { let h = HoRes { a: mk(15), b: Result.Ok(mk(115)) }; let HoRes { a, b } = h;
                let g = match b { Result.Ok(x) => x, Result.Err(e) => mk(0) }; println(f"  got{g.id}") }
fn wreb()     { let r: Result[W, String] = Result.Ok(mkw(16)); let c = r;
                match c { Result.Ok(x) => println(f"  ok{x.id}"), Result.Err(e) => println(f"  er{e}") } }
fn wesc()     { let r: Result[W, String] = Result.Ok(mkw(17)); let g = match r { Result.Ok(x) => x, Result.Err(e) => mkw(0) }; println(f"  got{g.id}") }
fn wtreb()    { let t: (R, Result[W, String]) = (mk(18), Result.Ok(mkw(118))); let (a, b) = t; let c = b;
                match c { Result.Ok(x) => println(f"  ok{x.id}"), Result.Err(e) => println(f"  er{e}") } }
fn wfreb()    { let h = HoW { a: mk(19), b: Result.Ok(mkw(119)) }; let HoW { a, b } = h; let c = b;
                match c { Result.Ok(x) => println(f"  ok{x.id}"), Result.Err(e) => println(f"  er{e}") } }
fn wfesc()    { let h = HoW { a: mk(20), b: Result.Ok(mkw(120)) }; let HoW { a, b } = h;
                let g = match b { Result.Ok(x) => x, Result.Err(e) => mkw(0) }; println(f"  got{g.id}") }

fn main() {
  println("rebind");   rebind()
  println("rebindu");  rebindu()
  println("esc");      esc()
  println("escerr");   escerr()
  println("esciflet"); esciflet()
  println("call");     call()
  println("inner");    inner()
  println("strreb");   strreb()
  println("stresc");   stresc()
  println("optesc");   optesc()
  println("treb");     treb()
  println("trebu");    trebu()
  println("tesc");     tesc()
  println("freb");     freb()
  println("fesc");     fesc()
  println("wreb");     wreb()
  println("wesc");     wesc()
  println("wtreb");    wtreb()
  println("wfreb");    wfreb()
  println("wfesc");    wfesc()
  println("done")
}
"#,
        &[
            "rebind",
            "  ok1",
            "dR1/t1",
            "rebindu",
            "dR2/t2",
            "  m",
            "esc",
            "  got3",
            "dR3/t3",
            "escerr",
            "  got4",
            "dR4/t4",
            "esciflet",
            "  got5",
            "dR5/t5",
            "call",
            "  eat6",
            "dR6/t6",
            "inner",
            "  got7",
            "dR7/t7",
            "strreb",
            "  oks8",
            "stresc",
            "  gots9",
            "optesc",
            "  got10",
            "dR10/t10",
            "treb",
            "dR11/t11",
            "  ok111",
            "dR111/t111",
            "trebu",
            "dR12/t12",
            "dR112/t112",
            "  m",
            "tesc",
            "dR13/t13",
            "  got113",
            "dR113/t113",
            "freb",
            "dR14/t14",
            "  ok114",
            "dR114/t114",
            "fesc",
            "dR15/t15",
            "  got115",
            "dR115/t115",
            "wreb",
            "  ok16",
            "dW16/x16y16",
            "wesc",
            "  got17",
            "dW17/x17y17",
            "wtreb",
            "dR18/t18",
            "  ok118",
            "dW118/x118y118",
            "wfreb",
            "dR19/t19",
            "  ok119",
            "dW119/x119y119",
            "wfesc",
            "dR20/t20",
            "  got120",
            "dW120/x120y120",
            "done",
        ],
        "asan_inline_struct_result_transfer_is_balanced",
    );
}

/// B-2026-08-29-1 (FIXED): a bare `String` / `Vec` payload inside an
/// `Option` or `Result` never had its buffer freed when the binding was
/// reassigned. `let mut vv: Option[String] = Some(s); vv = None;` leaked
/// the whole 38-byte buffer; `Option[Vec[i64]]` leaked 80; and
/// `Result[String, i64]` leaked 38 — in the directly-initialized and
/// moved-in spellings alike, storing `None`/`Err` or a fresh `Some`/`Ok`,
/// once per store in a loop.
///
/// THE BODIES WERE ALWAYS FINE; ONLY THE MEMORY WAS MISSING, and that split
/// is what hid this. B-2026-08-02-25 added the displaced payload's user
/// `Drop` BODIES walk at this exact site and recorded its reasoning for
/// stopping there: the memory "is already reclaimed by the untouched
/// `FreeInlineOptionPayload` / `BoxedEnumDrop` action — the pre-fix shape
/// leaked nothing under LSan — so a memory call here would double-free".
/// B-2026-08-07-4 corrected the BOXED half of that sentence. This is the
/// INLINE half, wrong for the same reason: the scope-exit action re-reads
/// the slot AFTER the store, so it frees only the LAST value and every
/// earlier one is orphaned. Verified separately that the bodies really are
/// correct — a user `impl Drop` on the payload fires exactly once, in the
/// right place, on interp, JIT and AOT, before and after the fix — so the
/// two channels are genuinely independent here.
///
/// THE HAZARD ROWS ARE WHY THE FIX EMITS THE QUEUED ACTION RATHER THAN A
/// HAND-ROLLED FREE. `match-hands-out`, `iflet-hands-out`,
/// `whilelet-consumes` and `result-hands-out` each move the payload to a
/// destination that then owns it; they were CLEAN before this change and a
/// naive free would double-free all four. Re-emitting the binding's own
/// `FreeInlineOptionPayload` / `FreeInlineResultPayload` makes that
/// structural instead of predicated: the action carries its own tag guard
/// (a consuming arm has already zeroed the source's tag), and a binding
/// whose registration was retracted has no action to find.
///
/// `struct-payload-control` is the axis, not a confirmation: an `Option`
/// whose payload is a heap-bearing STRUCT was already correct, as was a
/// plain `String` binding (`plain-string-control`, which has had a
/// displacement free via `lhs_is_tracked_vec` all along). It is
/// specifically the BARE `String`/`Vec` payload that had no arm.
///
/// Payloads are runtime-derived through `env.args().len()` and read with
/// `contains` rather than `len`, both deliberately — see the
/// B-2026-08-28-75 fixture above for what each guards against. `len` reads
/// the header, so an f-string's length folds and `-O2` deletes the
/// allocation, leaving the fixture asserting nothing.
#[test]
fn asan_reassigning_an_inline_optres_payload_frees_the_displaced_buffer() {
    const H: &str = "fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn slen(s: String) -> i64 { if s.contains(\"payload\") { s.len() } else { 0 } }\n";
    for (label, body, want) in [
            // ── the leaking family ──
            (
                "option-string-to-none",
                "fn nn() -> Option[String] { Option.None }\n\
                 fn f() -> i64 { let mut vv: Option[String] = Some(payload());\n\
                 \x20  let n = match vv { Some(s) => slen(s), None => 0 };\n\
                 \x20  vv = nn(); n + (if vv.is_some() { 1 } else { 0 }) }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            (
                "option-string-to-some",
                "fn f() -> i64 { let mut vv: Option[String] = Some(payload());\n\
                 \x20  let n = match vv { Some(s) => slen(s), None => 0 };\n\
                 \x20  vv = Some(\"zz\".to_string());\n\
                 \x20  n + (match vv { Some(s) => s.len(), None => 0 }) }\n\
                 fn main() { println(f()); }\n",
                "40",
            ),
            (
                "option-vec-i64",
                "fn nn() -> Option[Vec[i64]] { Option.None }\n\
                 fn f() -> i64 { let mut vv: Option[Vec[i64]] = Some([seed(), 2, 3, 4, 5, 6, 7, 8, 9, 10]);\n\
                 \x20  let n = match vv { Some(v) => v[0] + v[9], None => 0 };\n\
                 \x20  vv = nn(); n + (if vv.is_some() { 1 } else { 0 }) }\n\
                 fn main() { println(f()); }\n",
                "11",
            ),
            // A `Vec[String]` payload: the outer buffer AND its elements.
            (
                "option-vec-string",
                "fn nn() -> Option[Vec[String]] { Option.None }\n\
                 fn f() -> i64 { let mut vv: Option[Vec[String]] = Some([payload(), payload()]);\n\
                 \x20  let n = match vv { Some(v) => (if v[0].contains(\"payload\") { v[0].len() + v.len() } else { 0 }), None => 0 };\n\
                 \x20  vv = nn(); n + (if vv.is_some() { 1 } else { 0 }) }\n\
                 fn main() { println(f()); }\n",
                "40",
            ),
            (
                "result-string-to-err",
                "fn ee() -> Result[String, i64] { Err(7) }\n\
                 fn f() -> i64 { let mut vv: Result[String, i64] = Ok(payload());\n\
                 \x20  let n = match vv { Ok(s) => slen(s), Err(_) => 0 };\n\
                 \x20  vv = ee(); n + (match vv { Ok(_) => 1, Err(_) => 0 }) }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            (
                "result-string-to-ok",
                "fn f() -> i64 { let mut vv: Result[String, i64] = Ok(payload());\n\
                 \x20  let n = match vv { Ok(s) => slen(s), Err(_) => 0 };\n\
                 \x20  vv = Ok(\"zz\".to_string());\n\
                 \x20  n + (match vv { Ok(s) => s.len(), Err(_) => 0 }) }\n\
                 fn main() { println(f()); }\n",
                "40",
            ),
            // Moved in rather than directly initialized — the B-2026-08-28-75
            // axis, which is orthogonal to this one.
            (
                "moved-in-alias",
                "fn nn() -> Option[String] { Option.None }\n\
                 fn f() -> i64 { let value: Option[String] = Some(payload()); let mut vv = value;\n\
                 \x20  let n = match vv { Some(s) => slen(s), None => 0 };\n\
                 \x20  vv = nn(); n + (if vv.is_some() { 1 } else { 0 }) }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            // N stores orphaned N buffers, so the shape is linear.
            (
                "loop-reassign",
                "fn fresh(i: i64) -> Option[String] { Some(f\"payload-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\") }\n\
                 fn f() -> i64 { let mut vv: Option[String] = Some(payload()); let mut acc = 0; let mut i = 0;\n\
                 \x20  while i < 6 { acc = acc + (match vv { Some(s) => slen(s), None => 0 }); vv = fresh(i); i = i + 1; }\n\
                 \x20  acc + (match vv { Some(s) => slen(s), None => 0 }) }\n\
                 fn main() { println(f()); }\n",
                "266",
            ),
            // No read of the payload at all — `-O0`-only, like its sibling in
            // the B-2026-08-28-75 fixture and for the same reason.
            (
                "never-read",
                "fn nn() -> Option[String] { Option.None }\n\
                 fn f() -> i64 { let mut vv: Option[String] = Some(payload());\n\
                 \x20  vv = nn(); if vv.is_some() { 1 } else { 0 } }\n\
                 fn main() { println(f()); }\n",
                "0",
            ),
            // ── hazards: the payload is TAKEN before the store ──
            (
                "match-hands-out",
                "fn nn() -> Option[String] { Option.None }\n\
                 fn f() -> i64 { let mut vv: Option[String] = Some(payload());\n\
                 \x20  let k = match vv { Some(s) => s, None => \"zz\".to_string() };\n\
                 \x20  vv = nn(); slen(k) + (if vv.is_some() { 1 } else { 0 }) }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            (
                "iflet-hands-out",
                "fn nn() -> Option[String] { Option.None }\n\
                 fn f() -> i64 { let mut vv: Option[String] = Some(payload());\n\
                 \x20  let k = if let Some(s) = vv { s } else { \"zz\".to_string() };\n\
                 \x20  vv = nn(); slen(k) + (if vv.is_some() { 1 } else { 0 }) }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            (
                "whilelet-consumes",
                "fn nn() -> Option[String] { Option.None }\n\
                 fn take(s: String) -> i64 { slen(s) }\n\
                 fn f() -> i64 { let mut vv: Option[String] = Some(payload()); let mut acc = 0;\n\
                 \x20  while let Some(s) = vv { acc = acc + take(s); vv = nn(); } acc }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            (
                "result-hands-out",
                "fn ee() -> Result[String, i64] { Err(7) }\n\
                 fn f() -> i64 { let mut vv: Result[String, i64] = Ok(payload());\n\
                 \x20  let k = match vv { Ok(s) => s, Err(_) => \"zz\".to_string() };\n\
                 \x20  vv = ee(); slen(k) + (match vv { Ok(_) => 1, Err(_) => 0 }) }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            // ── boundaries and axes: clean before, clean after ──
            (
                "struct-payload-control",
                "struct D { s: String }\n\
                 fn dl(d: D) -> i64 { slen(d.s) }\n\
                 fn nn() -> Option[D] { Option.None }\n\
                 fn f() -> i64 { let mut vv: Option[D] = Some(D { s: payload() });\n\
                 \x20  let n = match vv { Some(d) => dl(d), None => 0 };\n\
                 \x20  vv = nn(); n + (if vv.is_some() { 1 } else { 0 }) }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            (
                "plain-string-control",
                "fn f() -> i64 { let mut s: String = payload();\n\
                 \x20  let n = if s.contains(\"payload\") { s.len() } else { 0 };\n\
                 \x20  s = \"zz\".to_string(); n + s.len() }\n\
                 fn main() { println(f()); }\n",
                "40",
            ),
            (
                "shared-payload-control",
                "shared struct N { s: String }\n\
                 fn nn() -> Option[N] { Option.None }\n\
                 fn f() -> i64 { let mut vv: Option[N] = Some(N { s: payload() });\n\
                 \x20  let n = match vv { Some(x) => (if x.s.contains(\"payload\") { x.s.len() } else { 0 }), None => 0 };\n\
                 \x20  vv = nn(); n + (if vv.is_some() { 1 } else { 0 }) }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            // `vv = vv` must not free the buffer it is about to store back.
            (
                "self-assign",
                "fn f() -> i64 { let mut vv: Option[String] = Some(payload()); vv = vv;\n\
                 \x20  match vv { Some(s) => slen(s), None => 0 } }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            // The RHS mentions the target, so the old value may have moved into
            // the callee; the free declines, as its siblings do.
            (
                "roundtrip",
                "fn pass(o: Option[String]) -> Option[String] { o }\n\
                 fn f() -> i64 { let mut vv: Option[String] = Some(payload()); vv = pass(vv);\n\
                 \x20  match vv { Some(s) => slen(s), None => 0 } }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            // No store at all: the scope-exit action is the only owner and was
            // always correct.
            (
                "never-stored",
                "fn f() -> i64 { let vv: Option[String] = Some(payload());\n\
                 \x20  match vv { Some(s) => slen(s), None => 0 } }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
        ] {
            assert_clean_asan_run(&format!("{H}{body}"), &[want], label);
        }
}

// ── Inline index of a fn-returned `Vec` — temp drop + element clone ──
//
// `names()[i]` indexes a fresh owned `Vec[String]` temporary inline
// (no intermediate binding). The element read shallow-aliases the
// temp's buffer, so codegen deep-clones the indexed `String` before
// dropping the temp Vec (buffer + every element's char heap). A
// missing clone → use-after-free on the printed value; a missing drop
// → leak of the buffer + the un-indexed elements; double-freeing the
// clone's source → double-free. Each `names()` allocates three
// Strings; only the indexed one's clone escapes, the rest and the
// buffer must free exactly once. (phase-11-stdlib-longtail.md)

#[test]
fn asan_inline_index_fn_returned_vec_string_no_leak() {
    assert_clean_asan_run(
        r#"
fn names() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alice"); v.push("bob"); v.push("carol");
    v
}
fn main() {
    println(names()[0]);
    println(names()[2]);
}
"#,
        &["alice", "carol"],
        "inline_index_fn_returned_vec_string",
    );
}

// Sibling of the above (B-2026-06-14-32, the by-value-argument consumer):
// the inline-temp-Vec heap-element clone passed DIRECTLY to a user fn
// (`sink(names()[0])`) — not just to `println` — must also free exactly
// once. The callee takes the `String` owned by value (which the callee does
// NOT free: owned String/Vec params land in `owned_vecstr_params`), so the
// caller-side `materialize_owned_temp` is the only thing reclaiming the
// clone. A missing materialization leaks the clone (Linux LSan); a stray
// second free (callee + caller both freeing) double-frees (ASAN, every
// host). Looping makes either fault unmistakable.
#[test]
fn asan_inline_index_fn_returned_vec_string_fn_arg_no_leak() {
    assert_clean_asan_run(
        r#"
fn names() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alice"); v.push("bob"); v.push("carol");
    v
}
fn sink(s: String) { println(s); }
fn main() {
    let mut i = 0i64;
    while i < 3 {
        let ns = names();
        sink(ns[0].clone());
        sink(ns[2].clone());
        i = i + 1;
    }
}
"#,
        &["alice", "carol", "alice", "carol", "alice", "carol"],
        "inline_index_fn_returned_vec_string_fn_arg",
    );
}

// ── First-class closure-value codegen (closure-value-codegen-fixes) ──
//
// Three pre-existing gaps that `collect_all_vec` surfaced, fixed in
// `src/codegen/closures.rs`: (1) a closure body that inline-constructs
// an enum variant (`|| Result.Ok(x)`) — return-type inference returned
// the payload type, not the enum, so the closure fn `ret`'d a mismatched
// type; (2) an f-string inside a closure body (`|| Result.Err(f"…")`) —
// the accumulator's cleanup leaked into the outer fn's frame
// (dominance verifier error); (3) direct closure-value call + match,
// a downstream symptom of (1). The cleanup-frame isolation that fixes
// (2) is the ASAN-sensitive change: an f-string moved into the
// returned `Result` must be freed exactly once (by the consumer's
// drop, NOT the closure), so this run guards against a double-free.

#[test]
fn asan_closure_inline_result_and_fstring_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let base: i64 = 100;
    let ok: Fn() -> Result[i64, String] = || Result.Ok(base + 1);
    let err: Fn() -> Result[i64, String] = || Result.Err(f"bad{base}");
    match ok() {
        Result.Ok(v) => { println(f"ok {v}"); }
        Result.Err(e) => { println(f"err {e}"); }
    }
    match err() {
        Result.Ok(v) => { println(f"ok {v}"); }
        Result.Err(e) => { println(f"err {e}"); }
    }
}
"#,
        &["ok 101", "err bad100"],
        "closure_inline_result_and_fstring",
    );
}

// Regression for the inline `m.get(k).unwrap().val` chain
// returning literal zero instead of the heap struct's val
// field. Pre-fix, `shared_type_for_call_like` only handled
// Identifier-receiver MethodCalls; a MethodCall whose object
// is itself a MethodCall (the unwrap-on-Map.get chain) fell
// through to the generic non-shared FieldAccess path, which
// compiled `.val` as i64 zero. The fix recognises
// `unwrap`/`expect` as a special case and recovers the inner
// T from `method_unwrap_inner_types[span]`; the bug-#8
// GEP+load+dec path then fires and the field is actually
// read.
//
// Together with `asan_map_get_shared_value_in_loop_no_alias_collapse`
// (which uses let-bindings) this covers both common reader
// shapes for `Map[K, Shared]` values.
#[test]
fn asan_map_get_unwrap_field_inline_chain() {
    assert_clean_asan_run(
        r#"
shared struct Node {
    val: i64,
    mut neighbors: Vec[Node],
}

fn main() {
    let mut visited: Map[i64, Node] = Map.new();
    let _ = visited.insert(0_i64, Node { val: 100, neighbors: Vec.new() });
    let _ = visited.insert(1_i64, Node { val: 200, neighbors: Vec.new() });
    println(visited.get(0_i64).unwrap().val);
    println(visited.get(1_i64).unwrap().val);
}
"#,
        &["100", "200"],
        "map_get_unwrap_field_inline_chain",
    );
}

#[test]
fn asan_inline_enum_field_struct_arg_no_leak() {
    // #22 (phase-12 self-hosting) — the #19 fresh-temp tail. An enum-field
    // struct constructed INLINE at a call site (`consume(W { tok: Tok.Id(..) })`,
    // no caller binding) whose callee CONSUMES the enum internally (`match
    // w.tok`) triggers the callee's entry-copy (`make_aggregate_param_callee_
    // owned`): the callee deep-copies the enum payload at entry and frees only
    // its own copy, leaving the inline temp's original heap for the caller. A
    // let-bound arg gets that caller drop at its binding site, but the inline
    // temp had no owner and leaked once per call. The enum payload is invisible
    // to the LLVM-type `aggregate_has_heap_field` gate (all-i64 words), so
    // `track_inline_owned_aggregate_arg` skipped the struct-literal arm; the fix
    // adds a SOURCE-level drop-heap gate, restricted to copy-supported structs
    // (an independent copy provably exists → distinct buffers, never a
    // double-free). Stresses: the bare enum-leaf struct arg (free fn + method
    // site), an enum leaf nested one struct deeper, and a direct-Vec struct arg
    // (regression — already worked via the LLVM gate, must stay clean). f-string
    // payloads keep the heap non-foldable; numeric arms guard behind `> 99999`.
    assert_clean_asan_run(
        r#"
enum Tok { Id(String), Int(i64) }
struct W { tok: Tok, n: i64 }
struct Inner { tok: Tok, k: i64 }
struct Outer { inner: Inner, n: i64 }
struct V { xs: Vec[i64], n: i64 }
struct Sink { total: i64 }
fn consume(w: W) -> i64 { match w.tok { Id(s) => s.len(), Int(z) => { if z > 99999 { 1 } else { 0 } } } }
fn consume_outer(o: Outer) -> i64 { match o.inner.tok { Id(s) => s.len(), Int(z) => { if z > 99999 { 1 } else { 0 } } } }
fn consume_vec(v: V) -> i64 { v.xs.len() }
fn mkv(n: i64) -> Vec[i64] { let mut a: Vec[i64] = Vec.new(); a.push(n); a.push(n + 1); a }
impl Sink {
    fn take(mut ref self, w: W) -> i64 { match w.tok { Id(s) => s.len(), Int(z) => { if z > 99999 { 1 } else { 0 } } } }
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    let mut sk = Sink { total: 0 };
    while i < 8 {
        // Bare enum-leaf struct, constructed inline as a free-fn arg (the #22 leak).
        acc = acc + consume(W { tok: Tok.Id(f"a-{i}"), n: i });

        // Same shape at a METHOD call site (shared arg-lowering path).
        acc = acc + sk.take(W { tok: Tok.Id(f"m-{i}"), n: i });

        // Enum leaf nested one struct deeper, inline.
        acc = acc + consume_outer(Outer { inner: Inner { tok: Tok.Id(f"o-{i}"), k: i }, n: i });

        // Direct-Vec struct arg, inline (regression — must stay clean).
        acc = acc + consume_vec(V { xs: mkv(i), n: i });

        i = i + 1;
    }
    if acc > 999999 { println("never"); }
    println("done");
}
"#,
        &["done"],
        "inline_enum_field_struct_arg_no_leak",
    );
}

#[test]
fn asan_user_drop_heap_enum_inline_temp_arg_leak_free() {
    // B-2026-06-10 carry-forward (enum arm): an inline enum temp with
    // BOTH a heap String payload and a user `impl Drop`, passed
    // directly as a call argument, registers the `karac_drop_<E>`
    // wrapper (user body) AND the payload-walking `__karac_drop_<E>`
    // on the same caller slot — complementary registrations, unlike
    // the struct case where the wrapper subsumes field cleanup. Must
    // be leak-clean (payload freed exactly once — the callee's entry
    // copy and the caller temp each free their own buffer) with the
    // user body firing once per temp. ≥36-byte payload defeats LSan's
    // short-string reachability masking.
    assert_clean_asan_run(
        r#"
enum Msg { Text(String), Nil }
impl Drop for Msg {
    fn drop(mut ref self) {
        println(1);
    }
}
fn consume(m: Msg) {}
fn main() {
    consume(Msg.Text("this is a long heap string payload over 36 bytes"));
    consume(Msg.Nil);
    println(0);
}
"#,
        &["1", "1", "0"],
        "user_drop_heap_enum_inline_temp_arg_leak_free",
    );
}

#[test]
fn asan_inline_enum_ctor_call_arg_no_leak_no_double_free() {
    // B-2026-06-12-10: an inline enum-variant constructor passed by value as
    // a call argument (`wrap(Tok.V(mk()))`) — and the method form
    // (`m.wrap(Tok.V(mk()))`, the shape the self-hosted lexer hits via
    // `self.make_spanned(Token.StringLiteral(value))`) — is a fresh owned
    // temp the callee owns by deep-copy. The caller still owns the temp and
    // must drop it; that caller-side drop was missing, leaking the variant's
    // String payload once per call (the dominant self-hosted-lexer leak).
    // The let-bound form (`let t = Tok.V(mk()); wrap(t)`) was already clean,
    // so this guards the now-symmetric inline path. On Linux CI this faults
    // under LeakSanitizer if the drop regresses; on macOS (no LSan) it is the
    // double-free gate — re-dropping the callee-owned copy would fault here.
    // Loops so any per-iteration imbalance accumulates into a fault.
    assert_clean_asan_run(
        r#"
enum Tok { V(String), Empty }
struct Wrap { t: Tok, n: i64 }
struct Maker { id: i64 }
fn mk() -> String {
    let mut s = "".to_string();
    s.push_str("inline_enum_ctor_arg_payload");
    s
}
fn wrap_free(t: Tok) -> Wrap {
    Wrap { t: t, n: 1 }
}
impl Maker {
    fn wrap(ref self, t: Tok) -> Wrap {
        Wrap { t: t, n: self.id }
    }
}
fn tlen(w: Wrap) -> i64 {
    match w.t {
        V(s) => s.len(),
        Empty => 0,
    }
}
fn main() {
    let m = Maker { id: 1 };
    let mut total: i64 = 0;
    let mut i = 0;
    while i < 50 {
        let a = wrap_free(Tok.V(mk()));
        let b = m.wrap(Tok.V(mk()));
        total = total + tlen(a) + tlen(b);
        i = i + 1;
    }
    println(total);
}
"#,
        &["2800"],
        "inline_enum_ctor_call_arg_no_leak_no_double_free",
    );
}

#[test]
fn asan_boxelem_vec_result_inline_struct_scope_drop_no_leak() {
    // Slice 3u: `Vec[Result[Holder, i64]]` — Holder (4 words) FITS
    // Result's 5-word area, so the Ok payload is INLINE — the
    // struct-payload flavor the 3q gate (String/Vec overlays only)
    // declined. The payload words overlay w0.. contiguously, so the
    // element drop GEPs to w0 and calls the struct's drop in place.
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn build(n: i64) -> Vec[Result[Holder, i64]] {
    let mut v: Vec[Result[Holder, i64]] = Vec.new();
    v.push(Ok(Holder { name: f"holder payload padded beyond thirty-six bytes {n}", id: n }));
    v.push(Err(n));
    v
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let v = build(i);
        println(v.len());
        i = i + 1;
    };
}
"#,
        &["2", "2", "2"],
        "boxelem_vec_result_inline_struct_scope_drop_no_leak",
    );
}

#[test]
fn asan_boxelem_vec_option_inline_struct_scope_drop_no_leak() {
    // Slice 3u: `Vec[Option[Pair]]` — Pair (3 words) fits Option's
    // inline area: the Option-side inline-STRUCT payload flavor.
    assert_clean_asan_run(
        r#"
struct Pair { s: String }
fn build(n: i64) -> Vec[Option[Pair]] {
    let mut v: Vec[Option[Pair]] = Vec.new();
    v.push(Some(Pair { s: f"pair payload padded beyond thirty-six bytes {n}" }));
    v.push(None);
    v
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let v = build(i);
        println(v.len());
        i = i + 1;
    };
}
"#,
        &["2", "2", "2"],
        "boxelem_vec_option_inline_struct_scope_drop_no_leak",
    );
}

/// B-2026-08-06-32 — a heap box nested inside a `Result`'s INLINE payload
/// area, which neither enum level's own boxing predicate names.
///
/// `Result[Option[Option[i64]], i64]`: the `Ok` payload is 4 LLVM words
/// against Result's 5-word area, so it is stored INLINE and
/// `boxed_enum_payload_variants` reports no boxed variant for the binding's
/// own type — correct, as far as it goes. But that inline value is itself an
/// `Option` whose 4-word inner outgrew Option's 3-word area, so
/// `coerce_to_payload_words` boxed it during construction. The let site asks
/// about the OUTER type only, so nobody registered a drop: 32 B per
/// construction.
///
/// THE SHAPE IS THE ONLY ONE OF ITS KIND, and that is a consequence of the
/// seeded areas rather than a choice. An `Option` value is always exactly 4
/// words and a `Result` always 6, so against Option's 3-word area a nested
/// enum NEVER fits (it is boxed at the outer level, where the existing
/// action owns it) and against Result's 5-word area an `Option` ALWAYS
/// does. `Result[Option[Wide], E]` is therefore the only way a box comes to
/// rest inside an inline area. The `nobox` arm below is the other side of
/// that predicate — `Result[Option[i64], E]` stores its inner scalar inline
/// and allocates nothing, so it must stay unregistered; freeing there would
/// free a word that was never a pointer.
///
/// NOBODY ELSE OWNS IT, which is what makes a let-site registration safe
/// here where the sibling rows had to fight over the owner. Measured at
/// `-O0` before the fix, every candidate owner leaked it identically:
/// matched in place, bound out by an arm, moved into a struct literal,
/// pushed into a `Vec`, passed by value, returned from a callee. So the
/// source binding is not merely the best owner, it is the only one, and it
/// is deliberately kept OUT of `boxed_enum_payload_vars` so a move cannot
/// disarm it in favour of a destination that registers nothing.
///
/// The arms that are not the headline leak are here because each one broke
/// during implementation, and every one was measured rather than reasoned:
///
///   * BOUND PASSTHROUGH (`let back = idr(d)`) and CHAINED passthrough. The
///     callee hands the box straight back, so source and result hold ONE
///     pointer. Registering for the result too aborts with a glibc double
///     free at `-O0` — i.e. omitting this rule turns the leak into
///     corruption. The chain needs the alias resolved at the RECORD site;
///     resolving only at lookup leaves `r2` unarmed and it registers a
///     second owner.
///   * DISCARDED passthrough (`idr(f);`) — the over-suppression control for
///     the two above. The source IS sole owner there, so a rule that
///     disarmed it instead would leak.
///   * HEAP-BEARING inner (`Option[Option[String]]` built at runtime). The
///     free this registers is BOX-ONLY. Also running the payload's drop was
///     implemented and double-freed at BOTH opt levels, because the arm that
///     binds the String out already owns it — the envelope is what was
///     unowned, and the envelope is all this frees.
///   * PAYLOAD-ABSENT sources (`Ok(None)`, `Err(n)`). Both tag guards are
///     load-bearing: an `Err` leaves the Ok-side words holding an integer,
///     and reading one as a pointer frees a scalar.
///   * ERR-SIDE nesting, since `Result` boxes per variant against one area.
///
/// NOT COVERED, deliberately, and each still leaks exactly as it did before
/// this change rather than being papered over: a nested box with no binding
/// at all (a fresh temp argument, a `Vec.push`), one inside a STRUCT that is
/// itself inline in the area, the interior box of a doubly-boxed
/// `Option[Option[Option[i64]]]`, and a binding that ESCAPES by return. The
/// last is the sharp one — freeing an escaped box is a use-after-free, so
/// the registration is retracted at both return spellings and the shape
/// stays a leak on purpose. Its direct-boxed sibling is worse (a double free
/// on `main` today) and is filed separately.
///
/// COVERAGE, stated because it is weaker than the assertion looks. Against
/// the pre-fix compiler this leaks 8,960 B in 280 blocks (7 boxes × 40
/// iterations) at `KARAC_OPT_LEVEL=0` and is CLEAN at the default `-O2`,
/// where every box folds away — so the memory half is carried entirely by
/// the `-O0` leg (`scripts/asan-o0-leg.sh`, B-2026-08-04-17). What the
/// `-O2` run still asserts is the accumulated value and the allocation
/// floor, so a leak traded for a wrong answer, or a fixture optimized into
/// nothing, fails there. The double-free directions fail at either level.
///
/// The expected value is COMPUTED, not read off a run: every payload is
/// seeded from the opaque `env.args().len()`, seven arms subtract the seed
/// back out to leave `i`, the discarded and String arms contribute 1 each
/// and the `Ok(None)` arm -1 — `7i + 1` per iteration, so
/// `7 * (0+…+39) + 40 = 5500`.
#[test]
fn asan_box_nested_in_result_inline_payload_area_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
fn cls(r: Result[Option[Option[i64]], i64]) -> i64 {
    match r {
        Result.Ok(Option.Some(Option.Some(x))) => x,
        Result.Ok(_) => -1,
        Result.Err(e) => e,
    }
}
fn clserr(r: Result[i64, Option[Option[i64]]]) -> i64 {
    match r {
        Result.Ok(k) => k,
        Result.Err(Option.Some(Option.Some(x))) => x,
        Result.Err(_) => -1,
    }
}
fn clsstr(r: Result[Option[Option[String]], i64]) -> i64 {
    match r {
        Result.Ok(Option.Some(Option.Some(s))) => if s.len() > 0 { 1 } else { 0 },
        Result.Ok(_) => -1,
        Result.Err(e) => e,
    }
}
fn nobox(r: Result[Option[i64], i64]) -> i64 {
    match r {
        Result.Ok(Option.Some(x)) => x,
        Result.Ok(Option.None) => -1,
        Result.Err(e) => e,
    }
}
fn idr(r: Result[Option[Option[i64]], i64]) -> Result[Option[Option[i64]], i64] { r }

fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a: Result[Option[Option[i64]], i64] = Result.Ok(Option.Some(Option.Some(n + i)));
        let va = match a {
            Result.Ok(Option.Some(Option.Some(x))) => x,
            Result.Ok(_) => -1,
            Result.Err(e) => e,
        };
        acc = acc + va - n;

        let b: Result[Option[Option[i64]], i64] = Result.Ok(Option.Some(Option.Some(n + i)));
        acc = acc + cls(b) - n;

        let c: Result[i64, Option[Option[i64]]] = Result.Err(Option.Some(Option.Some(n + i)));
        acc = acc + clserr(c) - n;

        let d: Result[Option[Option[i64]], i64] = Result.Ok(Option.Some(Option.Some(n + i)));
        let back = idr(d);
        acc = acc + cls(back) - n;

        let e1: Result[Option[Option[i64]], i64] = Result.Ok(Option.Some(Option.Some(n + i)));
        let r1 = idr(e1);
        let r2 = idr(r1);
        acc = acc + cls(r2) - n;

        let f: Result[Option[Option[i64]], i64] = Result.Ok(Option.Some(Option.Some(n + i)));
        idr(f);
        acc = acc + 1;

        let g: Result[Option[Option[i64]], i64] = Result.Ok(Option.None);
        acc = acc + cls(g);

        let h: Result[Option[Option[i64]], i64] = Result.Err(n + i);
        acc = acc + cls(h) - n;

        let k: Result[Option[i64], i64] = Result.Ok(Option.Some(n + i));
        acc = acc + nobox(k) - n;

        let s = f"p{n + i}";
        let m: Result[Option[Option[String]], i64] = Result.Ok(Option.Some(Option.Some(s)));
        acc = acc + clsstr(m);

        i = i + 1;
    }
    println(acc);
}
"#,
        &["5500"],
        "box_nested_in_result_inline_payload_area",
        30,
    );
}

/// B-2026-08-06-26 — a boxed `Result` payload must not ALSO get the
/// INLINE-payload cleanup.
///
/// Split out of B-2026-08-06-21, which narrowed this shape (double free at
/// both opt levels -> -O0 only) without closing it. The residue was a
/// different mechanism entirely: `track_inline_result_payload_cleanup`
/// guards against a boxed payload by consulting `boxed_enum_payload_vars`,
/// a set keyed by BINDING NAME — and a binding introduced from a CALL
/// result (`let rbk = idres(r);`) is never in it. The inline action was
/// therefore registered for a heap-BOXED payload, and its drop ran
/// `__karac_drop_struct_Wide` over `&slot.w0` — the word holding the box
/// POINTER — reading the struct's `String` out of whatever followed and
/// calling `free` on it. Valgrind: `Invalid free()`.
///
/// Found by the diff the row prescribed: `main` emitted TWO
/// `__karac_drop_struct_Wide` calls where the passing `Option` twin emits
/// exactly ONE, on the source binding that really does own the box.
///
/// The fix gates on the payload TYPE — `llvm_type_word_count(T) > area`,
/// the same predicate `coerce_to_payload_words` boxes on — rather than on
/// the name set, so it closes the class rather than the one binding shape.
/// The two sides are gated INDEPENDENTLY, which the `Result[Wide, String]`
/// case below pins: a boxed `Ok` beside an inline-heap `Err` must keep the
/// `Err` drop it still needs.
///
/// -O0-ONLY, deliberately noted: at `-O2` this program is clean before and
/// after, so THIS FIXTURE CANNOT CATCH THE BUG ON THE DEFAULT LEG. It is
/// red only under `KARAC_OPT_LEVEL=0` — i.e. it is gated by
/// `scripts/asan-o0-leg.sh` (B-2026-08-04-17), which is exactly the
/// population that leg exists to cover.
///
/// Floored per B-2026-08-04-17 — opaque `env.args().len()` seed,
/// runtime-built payloads, `contains` byte reads.
#[test]
fn asan_boxed_result_payload_no_inline_cleanup() {
    assert_clean_asan_run_min_allocs(
        r#"
struct Wide { a: i64, b: i64, c: i64, d: i64, e: i64, s: String }
fn mk(n: i64, i: i64) -> String {
    let mut s: String = String.new();
    s.push_str("payload-");
    s.push_str((n + i).to_string());
    s.push_str("-padding-to-force-heap");
    s
}
fn idres(r: Result[Wide, i64]) -> Result[Wide, i64] { r }
fn iderr(r: Result[i64, Wide]) -> Result[i64, Wide] { r }
fn idmix(r: Result[Wide, String]) -> Result[Wide, String] { r }
fn main() {
    let n = env.args().len() as i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40 {
        // boxed Ok payload, arm BINDS it — the headline shape
        let r: Result[Wide, i64] = Result.Ok(Wide { a: 1, b: 2, c: 3, d: 4, e: 5, s: mk(n, i) });
        let rbk = idres(r);
        match rbk { Result.Ok(x) => { if x.s.contains("payload") { acc = acc + 1; } } _ => { acc = acc - 1; } }
        // the same on the Err side
        let e: Result[i64, Wide] = Result.Err(Wide { a: 1, b: 2, c: 3, d: 4, e: 5, s: mk(n, i) });
        let ebk = iderr(e);
        match ebk { Result.Err(x) => { if x.s.contains("payload") { acc = acc + 1; } } _ => { acc = acc - 1; } }
        // boxed Ok + INLINE-heap Err: the per-side gate must keep the Err drop
        let m: Result[Wide, String] = if i % 2 == 0 {
            Result.Ok(Wide { a: 1, b: 2, c: 3, d: 4, e: 5, s: mk(n, i) })
        } else { Result.Err(mk(n, i)) };
        let mbk = idmix(m);
        match mbk {
            Result.Ok(x) => { if x.s.contains("payload") { acc = acc + 1; } }
            Result.Err(t) => { if t.contains("payload") { acc = acc + 1; } }
        }
        i = i + 1;
    }
    println(acc);
}
"#,
        &["120"],
        "boxed_result_payload_no_inline_cleanup",
        100,
    );
}

/// B-2026-08-06-27 — the INLINE sibling of the passthrough double free.
///
/// B-2026-08-06-21 fixed the heap-BOXED payload and asserted the inline
/// path was already correct on this shape. That assertion rested on a
/// control whose payload was a string LITERAL — rodata, nothing to free —
/// so it could not have failed however wrong the ownership was. With a
/// runtime-built payload the inline path double-freed identically, at the
/// DEFAULT -O2 as well as -O0. A control that cannot fail is worth less
/// than no control, and an allocation floor does not catch it because the
/// control is not the thing being floored.
///
/// TWO owners had to go, and the second only shows up with a payload-
/// BINDING arm — a non-binding `Some(_)` arm was clean after the first half
/// alone, which is exactly the pair that proved the second was needed:
///
///   1. the let-site registration for the result binding. That site is
///      gated on the RHS being any `Call`, so a PASSTHROUGH call slipped
///      through even though it aliases an existing binding — the very thing
///      the `rhs_is_fresh_inline_enum` arm beside it excludes, for this
///      same double-free reason.
///   2. the consuming arm's disarm, which is keyed on the SCRUTINEE's name.
///      With (1) skipped the result owns nothing, so the disarm found
///      nothing and the source stayed armed against a payload the arm
///      binding now owned. `passthrough_owner_alias` forwards it.
///
/// Both `Option` and `Result` are here because each has its own disarm and
/// only the pair proves the forwarding is not Option-specific. `peek(e)` is
/// a NON-passthrough consuming call and the last arm a fresh TEMP: neither
/// has a second owner, both were always clean, and both must stay that way
/// — the fix must not disarm an owner that is the only one.
///
/// Floored per B-2026-08-04-17 (566 allocations at -O2).
#[test]
fn asan_inline_payload_passthrough_arg_single_owner() {
    assert_clean_asan_run_min_allocs(
        r#"
fn idopt(o: Option[String]) -> Option[String] { o }
fn idres(r: Result[String, i64]) -> Result[String, i64] { r }
fn peek(o: Option[String]) -> i64 { match o { Option.Some(s) => s.len(), Option.None => -1 } }
fn mk(n: i64, i: i64) -> String {
    let mut s: String = String.new();
    s.push_str("payload-");
    s.push_str((n + i).to_string());
    s.push_str("-padding-to-force-heap");
    s
}
fn main() {
    let n = env.args().len() as i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40 {
        let a: Option[String] = Option.Some(mk(n, i));
        let abk = idopt(a);
        match abk { Option.Some(x) => { if x.contains("payload") { acc = acc + 1; } } _ => { acc = acc - 1; } }
        let b: Option[String] = Option.Some(mk(n, i));
        let bbk = idopt(b);
        match bbk { Option.Some(_) => { acc = acc + 1; } _ => { acc = acc - 1; } }
        let r: Result[String, i64] = Result.Ok(mk(n, i));
        let rbk = idres(r);
        match rbk { Result.Ok(x) => { if x.ends_with("heap") { acc = acc + 1; } } _ => { acc = acc - 1; } }
        let e: Option[String] = Option.Some(mk(n, i));
        acc = acc + peek(e);
        let t = idopt(Option.Some(mk(n, i)));
        match t { Option.Some(x) => { if x.contains("payload") { acc = acc + 1; } } _ => { acc = acc - 1; } }
        i = i + 1;
    }
    println(acc);
}
"#,
        &["1431"],
        "inline_payload_passthrough_arg_single_owner",
        400,
    );
}

#[test]
fn asan_inline_map_get_unwrap_heap_value_no_double_free() {
    // B-2026-07-15-26: an INLINE `map.get(k).unwrap()` whose value is a heap
    // type (`Map[K, String]` / `Map[K, Vec[..]]`) packs a BORROW of the
    // bucket's `{ptr,len,cap}`. Consumed inline (a println arg, a method
    // receiver, a call arg) the temporary's own `cap > 0` free-guard freed
    // that buffer — which the map's scope-exit per-entry drop ALSO freed →
    // double-free (SIGABRT), while binding to a `let` first was clean. Fixed
    // by zeroing the borrow view's `cap` at the unwrap so every consumer's
    // free-guard skips it (the map stays the sole owner; reads use ptr+len).
    // Exercise all three inline consumption modes over String and Vec values
    // in a loop (a per-iteration double-free trips ASAN; a per-iteration
    // strand — if the map ever STOPPED owning the value — accumulates a leak).
    assert_clean_asan_run(
        r#"
fn takes(s: String) -> i64 {
    s.len()
}
fn main() {
    let mut ms: Map[i64, String] = Map.new();
    ms.insert(1, "alpha".to_string());
    ms.insert(2, "bb".to_string());
    let mut mv: Map[i64, Vec[i64]] = Map.new();
    mv.insert(1, [1, 2, 3, 4]);
    mv.insert(2, [9]);
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 60 {
        acc = acc + ms.get(1).unwrap().len();
        acc = acc + takes(ms.get(2).unwrap());
        acc = acc + mv.get(1).unwrap().len();
        acc = acc + mv.get(2).unwrap().len();
        i = i + 1;
    }
    println(acc);
    println(ms.get(1).unwrap());
}
"#,
        &["720", "alpha"],
        "inline_map_get_unwrap_heap_value_no_double_free",
    );
}

#[test]
fn asan_inline_vec_first_last_unwrap_heap_elem_no_double_free() {
    // B-2026-08-08-20: the Vec sibling of the map case above, and a
    // demonstration of how a misnamed predicate hides a bug. The inline
    // cap-zero fired on `unwrap_receiver_is_nonshared_heap_value_map_get`,
    // whose `method == "get"` test read as "Map.get" but ALSO covered
    // `Vec.get` by accident — the receiver's type is resolved through
    // `var_elem_type_exprs`, which records a Vec var's ELEMENT type in the
    // same slot it records a Map var's VALUE type. So `v.get(0).unwrap()`
    // was protected while `v.first()` / `v.last()` — the same shallow
    // element load in `vec_method.rs`, the same `build_option_some_via_phis`
    // — were not. Consumed INLINE the borrow view kept its real `cap`, its
    // free-guard freed the Vec's own element buffer, and the Vec's
    // scope-exit per-element drop freed it again: `free(): double free
    // detected in tcache 2` under JIT and AOT at every opt level, while
    // `--interp` printed correctly and `let s = v.first().unwrap();` was
    // clean (the let path is borrow-elided on its own, which is exactly what
    // made this look like an accessor bug rather than a position bug).
    //
    // Covers both element kinds (String and Vec) and every inline
    // consumption mode that reproduced: bare call arg, `expect` instead of
    // `unwrap`, an f-string hole, and a `.len()` chain. `get` rides along so
    // the arm that already worked stays wired. The loop makes a
    // per-iteration double-free trip ASAN and a per-iteration strand — had
    // the fix over-corrected into "nobody owns this" — accumulate a leak.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut vs: Vec[String] = Vec.new();
    vs.push("alpha".to_string());
    vs.push("bb".to_string());
    let mut vv: Vec[Vec[i64]] = Vec.new();
    vv.push([1, 2, 3, 4]);
    vv.push([9]);
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 60 {
        acc = acc + vs.first().unwrap().len();
        acc = acc + vs.last().unwrap().len();
        acc = acc + vv.first().unwrap().len();
        acc = acc + vv.last().unwrap().len();
        acc = acc + vs.get(0).unwrap().len();
        i = i + 1;
    }
    println(acc);
    println(vs.first().unwrap());
    println(vs.last().expect("empty"));
    println(f"[{vs.first().unwrap()}]");
}
"#,
        &["1020", "alpha", "bb", "[alpha]"],
        "inline_vec_first_last_unwrap_heap_elem_no_double_free",
    );
}

#[test]
fn asan_inline_index_map_get_unwrap_vec_value_no_leak() {
    // B-2026-07-15-27: inline-indexing a `map.get(k).unwrap()` Vec value
    // (`m.get(k).unwrap()[i]`) is lowered by materializing the `{ptr,len,
    // cap=0}` borrow view (B-2026-07-15-26) into a synth Vec local, reading
    // the element, deep-cloning it when heap, and — crucially — NOT dropping
    // the temp (the map owns the buffer + elements). Two failure modes this
    // guards, both in a loop so they accumulate/trip:
    //   * dropping the borrow temp would re-drain the map's elements
    //     (`emit_vec_drop_fn` walks 0..len before the cap-guarded free) →
    //     double-free (ASAN trips);
    //   * the deep-cloned heap element handed to a by-value consumer (a
    //     `takes(String)` call arg, a let-binding) must be freed by that
    //     consumer, or it leaks once per iteration (LSan catches the strand).
    // Covers a scalar-elem value (`Vec[i64]`), a heap-elem value
    // (`Vec[String]`) as a `[i].method()` receiver / a by-value fn-call arg /
    // a let-binding, and a nested `Vec[Vec[i64]]`.
    assert_clean_asan_run(
        r#"
fn takes(s: String) -> i64 {
    s.len()
}
fn main() {
    let mut m: Map[i64, Vec[i64]] = Map.new();
    m.insert(1, [10, 20, 30]);
    let mut s: Map[i64, Vec[String]] = Map.new();
    s.insert(1, ["alpha", "beta", "gamma"]);
    let mut n: Map[i64, Vec[Vec[i64]]] = Map.new();
    n.insert(7, [[1, 2], [3, 4, 5]]);
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 60 {
        acc = acc + m.get(1).unwrap()[1];
        acc = acc + s.get(1).unwrap()[0].len();
        acc = acc + takes(s.get(1).unwrap()[2].clone());
        let w = s.get(1).unwrap()[1].clone();
        acc = acc + w.len();
        let inner = n.get(7).unwrap()[1].clone();
        acc = acc + inner[2];
        i = i + 1;
    }
    println(acc);
    println(s.get(1).unwrap()[0]);
}
"#,
        &["2340", "alpha"],
        "inline_index_map_get_unwrap_vec_value_no_leak",
    );
}

/// B-2026-08-15-6 — the deep-cloned heap element of an inline-temp-`Vec`
/// index leaks when the index is an f-string INTERPOLATION operand, while
/// the identical expression as a direct `println` argument is clean.
///
/// `compile_inline_temp_vec_index_ex` must deep-clone a non-Copy element (it
/// drains the temp buffer right after the read, so a borrowed element would
/// dangle) and then de-registers the synth local, so the clone arrives at its
/// consumer with no binding and no cleanup of its own. Every consuming
/// position therefore has to name it; the argument gate had since
/// B-2026-06-14-32, the f-string part renderer had not.
///
/// Both element kinds are pinned because they leak through DIFFERENT arms of
/// `fstr_render_part` and needed separate fixes. A `String` element reaches
/// the fresh-owned-temp arm; a `Vec` element is intercepted earlier by
/// `try_compile_vec_display`, whose own producer list
/// (`print_vec_operand_is_owned_temp`) ruled out every `Index` as a place
/// read. That second half is also the row's one correction: for a `Vec`
/// element the DIRECT print argument leaks too, so it is not an f-string bug
/// there — hence the direct spellings below, which would have gone on passing
/// had only the f-string been fixed.
///
/// The last four lines are the double-free direction, and they are the reason
/// the producer list is a list rather than "not an identifier": `b.xs`,
/// `grid[1]` and `names[1]` hand back a container's own `{ptr,len,cap}`, and
/// tracking one of those was exactly B-2026-08-14-30's hard double free.
#[test]
fn asan_fstring_inline_temp_vec_index_element_no_leak() {
    assert_clean_asan_run(
        r#"
struct B { xs: Vec[i64] }
fn names() -> Vec[String] {
    let v: Vec[String] = ["alphaalphaalphaalpha", "betabetabetabetabeta"];
    return v;
}
fn mkrows() -> Vec[Vec[i64]] {
    let v: Vec[Vec[i64]] = [[1, 2, 3, 4], [5, 6, 7, 8]];
    return v;
}
fn main() {
    let b = B { xs: [7, 8, 9] };
    let grid: Vec[Vec[i64]] = [[1, 2], [3, 4]];
    let held: Vec[String] = ["gammagammagamma", "deltadeltadelta"];
    let mut k = 0;
    while k < 20 {
        println(f"{names()[1]}");
        println(names()[1]);
        println(f"{mkrows()[1]}");
        println(mkrows()[1]);
        println(f"{b.xs}");
        println(f"{grid[1]}");
        println(f"{held[1]}");
        println(held[1]);
        k = k + 1;
    }
    println("done");
}
"#,
        &[
            "betabetabetabetabeta",
            "betabetabetabetabeta",
            "[5, 6, 7, 8]",
            "[5, 6, 7, 8]",
            "[7, 8, 9]",
            "[3, 4]",
            "deltadeltadelta",
            "deltadeltadelta",
        ]
        .repeat(20)
        .into_iter()
        .chain(std::iter::once("done"))
        .collect::<Vec<_>>(),
        "asan_fstring_inline_temp_vec_index_element_no_leak",
    );
}

/// B-2026-08-15-14 — an INLINE container temporary in argument position
/// releases its elements, not just its buffer.
///
/// `materialize_owned_temp`'s Vec branch kept only the LLVM element type
/// and called `track_vec_var`, whose drain reaches an element only when the
/// element is itself a `{ptr,len,cap}` or has a direct Vec/String field.
/// Every other element kind — an RC handle, a Map handle, a struct whose
/// `shared` field the value drop skips by design, a nested Vec — was
/// invisible to it, so the temp freed its buffer and stranded one
/// allocation per ELEMENT.
///
/// THE UNIT IS THE ELEMENT, NOT THE CALL, which is what identifies the
/// container temp as the culprit: three elements through one call stranded
/// three RC boxes, one element through three calls stranded one. The
/// per-call clone/release pairing was balanced all along.
///
/// The NAMED spelling of the same clone (`let c = ns.clone(); agg(c)`) was
/// always clean — the `let` path already routes through the element-drop
/// chooser — so the two spellings of one operation disagreed about who
/// releases the elements. Both are here, and the named one is expected to
/// pass on either side of the fix: it is the control that says the defect
/// is the inline temporary rather than the clone.
///
/// The `Vec[Vec[String]]` case earns its place separately: the drain treats
/// a per-element drop fn as EXCLUSIVE of its inline recursion, so an
/// element routed through both would DOUBLE-FREE rather than leak. It is
/// the row that would catch that.
#[test]
fn asan_inline_container_temp_arg_releases_its_elements() {
    assert_clean_asan_run(
        r#"
shared struct Node { label: String }
struct Holder { n: Node }

fn count_nodes(ns: Vec[Node]) -> i64 { ns.len() }
fn count_holders(hs: Vec[Holder]) -> i64 { hs.len() }
fn count_maps(ms: Vec[Map[String, i64]]) -> i64 { ms.len() }
fn count_nested(vs: Vec[Vec[String]]) -> i64 { vs.len() }

fn main() {
    // Three elements, ONE call: the leak scaled with this number.
    let mut ns: Vec[Node] = Vec.new();
    ns.push(Node { label: "gammagammagamma" });
    ns.push(Node { label: "deltadeltadelta" });
    ns.push(Node { label: "epsilonepsilon0" });
    println(f"{count_nodes(ns.clone())}");

    // One element, THREE calls: it did not scale with this one.
    let mut one: Vec[Node] = Vec.new();
    one.push(Node { label: "zetazetazetazet" });
    println(f"{count_nodes(one.clone()) + count_nodes(one.clone()) + count_nodes(one.clone())}");

    // The named-binding control — clean before the fix as well.
    let c = one.clone();
    println(f"{count_nodes(c)}");

    // A struct whose `shared` field the plain value drop skips by design.
    let mut hs: Vec[Holder] = Vec.new();
    hs.push(Holder { n: Node { label: "etaetaetaetaeta" } });
    println(f"{count_holders(hs.clone())}");

    // A Map handle element — a bare pointer, equally invisible to the drain.
    let mut ms: Vec[Map[String, i64]] = Vec.new();
    let mut m: Map[String, i64] = Map.new();
    let _ = m.insert("thetathetatheta", 1);
    ms.push(m);
    println(f"{count_maps(ms.clone())}");

    // The double-free guard: the drain CAN see this one inline, so it must
    // not also get a per-element drop fn.
    let mut vs: Vec[Vec[String]] = Vec.new();
    let inner: Vec[String] = ["iotaiotaiotaiot", "kappakappakappa"];
    vs.push(inner);
    println(f"{count_nested(vs.clone())}");

    println("end");
}
"#,
        &["3", "3", "1", "1", "1", "1", "end"],
        "inline_container_temp_arg_releases_elements",
    );
}

/// B-2026-08-26-32 — `Map.get` with an INLINE TEMPORARY struct key that
/// owns heap leaked the temporary: one allocation per lookup.
///
/// The key is borrowed for the lookup and then discarded, so nothing takes
/// ownership of it — unlike `insert`, where the key is MOVED into the map
/// and the map's own drop reclaims it. The three controls below are what
/// make this a statement about the lookup path specifically rather than
/// about temporaries in general.
#[test]
fn asan_map_get_with_inline_temporary_struct_key_is_clean() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
struct Item { id: i64, name: String }
fn main() {
    let mut m: Map[Item, i64] = Map.new();
    m.insert(Item { id: 1i64, name: f"a-padding-padding-padding" }, 7i64);
    let mut j = 0i64;
    let mut hits = 0i64;
    while j < 5i64 {
        match m.get(Item { id: j, name: f"b-{j}-padding-padding-padding" }) {
            Some(v) => { hits = hits + v; }
            None => {}
        }
        j = j + 1i64;
    }
    println(f"{hits}{m.len()}");
}
"#,
        &["01"],
        "map-get-inline-temporary-struct-key",
    );
}

/// CONTROL: `insert` is unaffected, because the key is MOVED into the map.
/// A fix that drops the key temporary unconditionally would double-free
/// here, so this is the guard against over-correcting.
#[test]
fn asan_map_insert_with_inline_temporary_struct_key_is_clean() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
struct Item { id: i64, name: String }
fn main() {
    let mut m: Map[Item, i64] = Map.new();
    let mut j = 0i64;
    while j < 8i64 {
        m.insert(Item { id: j, name: f"k-{j}-padding-padding-padding" }, j);
        j = j + 1i64;
    }
    println(m.len());
}
"#,
        &["8"],
        "map-insert-inline-temporary-struct-key",
    );
}

/// The row measured `get` only and asked for these: `remove` and
/// `contains_key` take the key the same borrowed-then-discarded way.
#[test]
fn asan_map_remove_and_contains_key_with_inline_temporary_struct_key_are_clean() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
struct Item { id: i64, name: String }
fn main() {
    let mut m: Map[Item, i64] = Map.new();
    m.insert(Item { id: 1i64, name: f"a-padding-padding-padding" }, 7i64);
    let mut j = 0i64;
    let mut seen = 0i64;
    while j < 4i64 {
        if m.contains_key(Item { id: j, name: f"c-{j}-padding-padding-padding" }) {
            seen = seen + 1i64;
        }
        match m.remove(Item { id: j, name: f"d-{j}-padding-padding-padding" }) {
            Some(v) => { seen = seen + v; }
            None => {}
        }
        j = j + 1i64;
    }
    println(f"{seen}{m.len()}");
}
"#,
        &["01"],
        "map-remove-contains-key-inline-temporary-struct-key",
    );
}

/// CONTROL for the row's "a `Map[String, _]` looked up with a temporary
/// f-string is clean" claim — the one-level String path already drops its
/// temporary, so the defect is the struct wrapper, not temporaries as such.
#[test]
fn asan_map_get_with_inline_temporary_string_key_is_clean() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert(f"a-padding-padding-padding", 7i64);
    let mut j = 0i64;
    let mut hits = 0i64;
    while j < 5i64 {
        match m.get(f"b-{j}-padding-padding-padding") {
            Some(v) => { hits = hits + v; }
            None => {}
        }
        j = j + 1i64;
    }
    println(f"{hits}{m.len()}");
}
"#,
        &["01"],
        "map-get-inline-temporary-string-key",
    );
}

/// PROBE: a Set, whose `contains` takes a key the same borrowed way.
#[test]
fn asan_set_contains_with_inline_temporary_struct_key_is_clean() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
struct Item { id: i64, name: String }
fn main() {
    let mut s: Set[Item] = Set.new();
    s.insert(Item { id: 1i64, name: f"a-padding-padding-padding" });
    let mut j = 0i64;
    let mut seen = 0i64;
    while j < 5i64 {
        if s.contains(Item { id: j, name: f"s-{j}-padding-padding-padding" }) {
            seen = seen + 1i64;
        }
        j = j + 1i64;
    }
    println(f"{seen}{s.len()}");
}
"#,
        &["01"],
        "set-contains-inline-temporary-struct-key",
    );
}

/// PROBE: a key struct whose heap sits behind a `Vec` field rather than a
/// `String`, to check the drop is about heap ownership and not about the
/// String field specifically.
#[test]
fn asan_map_get_with_inline_temporary_vec_field_key_is_clean() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
struct Key { id: i64, parts: Vec[i64] }
fn main() {
    let mut m: Map[Key, i64] = Map.new();
    let mut seed: Vec[i64] = Vec.new();
    seed.push(1i64);
    m.insert(Key { id: 1i64, parts: seed }, 7i64);
    let mut j = 0i64;
    let mut hits = 0i64;
    while j < 5i64 {
        let mut p: Vec[i64] = Vec.new();
        p.push(j);
        p.push(j + 1i64);
        match m.get(Key { id: j, parts: p }) {
            Some(v) => { hits = hits + v; }
            None => {}
        }
        j = j + 1i64;
    }
    println(f"{hits}{m.len()}");
}
"#,
        &["01"],
        "map-get-inline-temporary-vec-field-key",
    );
}

/// B-2026-08-26-32, the remaining lookup sites. The row named `get`, and
/// asked for `remove`/`contains_key`; these three take a borrowed key the
/// same way and were fixed in the same pass, so they are pinned here rather
/// than left to be rediscovered: `Map.get_or`, `Set.remove`, and
/// `SortedMap.floor`/`ceiling` (whose pivot is compared and never stored).
#[test]
fn asan_map_get_or_with_inline_temporary_struct_key_is_clean() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
struct Item { id: i64, name: String }
fn main() {
    let mut m: Map[Item, i64] = Map.new();
    m.insert(Item { id: 1i64, name: f"a-padding-padding-padding" }, 7i64);
    let mut j = 0i64;
    let mut acc = 0i64;
    while j < 5i64 {
        acc = acc + m.get_or(Item { id: j, name: f"g-{j}-padding-padding-padding" }, 0i64);
        j = j + 1i64;
    }
    println(f"{acc}{m.len()}");
}
"#,
        &["01"],
        "map-get-or-inline-temporary-struct-key",
    );
}

#[test]
fn asan_set_remove_with_inline_temporary_struct_key_is_clean() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
struct Item { id: i64, name: String }
fn main() {
    let mut s: Set[Item] = Set.new();
    s.insert(Item { id: 1i64, name: f"a-padding-padding-padding" });
    let mut j = 0i64;
    let mut gone = 0i64;
    while j < 5i64 {
        if s.remove(Item { id: j, name: f"r-{j}-padding-padding-padding" }) {
            gone = gone + 1i64;
        }
        j = j + 1i64;
    }
    println(f"{gone}{s.len()}");
}
"#,
        &["01"],
        "set-remove-inline-temporary-struct-key",
    );
}

/// B-2026-08-26-32, the `SortedMap` lookup pivot. `floor`/`ceiling` COMPARE
/// the pivot and never store it — the same borrowed-then-discarded shape as
/// `get` — and share the one `free_fresh_owned_str_arg` call site that the
/// aggregate free is paired with. Note the result is the ENTRY pair
/// `Option[(Item, i64)]`, whose key is the map's own copy, so freeing the
/// pivot cannot touch it.
#[test]
fn asan_sorted_map_floor_with_inline_temporary_struct_pivot_is_clean() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq, Ord)]
struct Item { id: i64, name: String }
fn main() {
    let mut m: SortedMap[Item, i64] = SortedMap.new();
    m.insert(Item { id: 5i64, name: f"a-padding-padding-padding" }, 7i64);
    let mut j = 0i64;
    let mut acc = 0i64;
    while j < 4i64 {
        match m.floor(Item { id: j, name: f"f-{j}-padding-padding-padding" }) {
            Some(kv) => { acc = acc + kv.1; }
            None => {}
        }
        j = j + 1i64;
    }
    println(f"{acc}{m.len()}");
}
"#,
        &["01"],
        "sorted-map-floor-inline-temporary-struct-pivot",
    );
}

/// B-2026-08-26-32, the ENUM key. Found while fixing the struct leg: on the
/// identical program shape the struct key lost 0 bytes and the enum key
/// still lost 135 B in 5 blocks under valgrind. Same defect, different
/// payload — an enum-variant temporary owning heap is borrowed by the
/// lookup and discarded exactly like a struct one.
#[test]
fn asan_map_get_with_inline_temporary_enum_key_is_clean() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
enum Tag { Named { s: String }, Anon }
fn main() {
    let mut m: Map[Tag, i64] = Map.new();
    m.insert(Tag.Named { s: f"a-padding-padding-padding" }, 7i64);
    let mut j = 0i64;
    let mut hits = 0i64;
    while j < 5i64 {
        match m.get(Tag.Named { s: f"b-{j}-padding-padding-padding" }) {
            Some(v) => { hits = hits + v; }
            None => {}
        }
        j = j + 1i64;
    }
    println(f"{hits}{m.len()}");
}
"#,
        &["01"],
        "map-get-inline-temporary-enum-key",
    );
}

/// B-2026-08-27-6 — the MATCHING sibling of the two tests around it, and
/// the shape neither of them reached: both probe with keys that genuinely
/// differ from the inserted one, so the successful-lookup path through the
/// structural enum comparator never ran under ASAN at all.
///
/// It matters here because a HIT and a MISS retire the inline temporary
/// differently — a hit borrows the temporary's `String` payload for the
/// comparison and then still has to discard the temporary, while the
/// returned value comes from the key the map owns. Comparing by content
/// rather than by payload words is what puts a hit on this path at all.
#[test]
fn asan_map_get_with_a_matching_inline_temporary_enum_key_is_clean() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
enum Tag { Named { s: String }, Anon }
fn main() {
    let mut m: Map[Tag, i64] = Map.new();
    m.insert(Tag.Named { s: f"a-padding-padding-padding" }, 7i64);
    let mut j = 0i64;
    let mut hits = 0i64;
    while j < 5i64 {
        match m.get(Tag.Named { s: f"a-padding-padding-padding" }) {
            Some(v) => { hits = hits + v; }
            None => {}
        }
        j = j + 1i64;
    }
    println(f"{hits}{m.len()}");
}
"#,
        &["351"],
        "map-get-matching-inline-temporary-enum-key",
    );
}

/// B-2026-09-09-22 — an `Option`/`Result` payload of `Vec[<element owning
/// heap>]` released its elements twice. ASAN is the right gate for this
/// one: the failure is a double free, so a regression aborts here rather
/// than producing a wrong answer.
///
/// The fix REMOVES a free, which is the direction that trades a double
/// free for a leak — so every cell is also valgrind-clean at
/// `KARAC_OPT_LEVEL=0` and `2`, and LSan on the Linux CI leg is what keeps
/// that true.
#[test]
fn asan_inline_optres_vec_payload_drains_its_elements_once() {
    // 1 — the row's own shape.
    assert_clean_asan_run(
            "fn plainV(x: Option[Vec[Vec[String]]]) {\n\
             \x20   match x { Some(t) => { println(f\"s:{t[0][0]}\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let v: Vec[Vec[String]] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
             \x20   plainV(Some(v));\n\
             }\n",
            &["s:aaaaaaaa0"],
            "b22-option-arm-indexed",
        );
    // 2 — the `Result` half, which shares the overlay emitter.
    assert_clean_asan_run(
            "fn plainV(x: Result[Vec[Vec[String]], i64]) {\n\
             \x20   match x { Ok(t) => { println(f\"s:{t[0][0]}\") } Err(e) => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let v: Vec[Vec[String]] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
             \x20   plainV(Ok(v));\n\
             }\n",
            &["s:aaaaaaaa0"],
            "b22-result-ok-arm",
        );
    // 3 — one frame, no call, no index: the minimal reproducer, where a
    //     SINGLE cleanup action was performing both frees.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20   let o: Option[Vec[Vec[String]]] = Some([[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]]);\n\
             \x20   match o { Some(t) => { println(f\"s:{t.len()}\") } None => { println(\"n\") } }\n\
             }\n",
            &["s:2"],
            "b22-local-option-one-frame",
        );
    // 4 — CONTROL, and the leak direction: a `String` element resolves no
    //     drain, so only the one-level recursion runs. If the supersede
    //     gate were widened to skip that recursion unconditionally, this
    //     cell would leak both element buffers.
    assert_clean_asan_run(
        "fn plainV(x: Option[Vec[String]]) {\n\
             \x20   match x { Some(t) => { println(f\"s:{t[0]}\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let v: Vec[String] = [f\"aaaaaaaa0\", f\"bbbbbbbb1\"];\n\
             \x20   plainV(Some(v));\n\
             }\n",
        &["s:aaaaaaaa0"],
        "b22-string-element-control",
    );
    // 5 — CONTROL, same leak direction one level deeper: a `Vec[i64]`
    //     element also has no drain, and its inner buffers are freed by
    //     the recursion alone.
    assert_clean_asan_run(
            "fn plainV(x: Option[Vec[Vec[i64]]]) {\n\
             \x20   match x { Some(t) => { println(f\"s:{t[0][1]}\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let v: Vec[Vec[i64]] = [[10, 11], [20]];\n\
             \x20   plainV(Some(v));\n\
             }\n",
            &["s:11"],
            "b22-scalar-inner-control",
        );
}

/// B-2026-09-10-1 — the callee's entry copy of an owned
/// `Option`/`Result` `Vec[T]` payload was element-deep only for a
/// name-listed element (`String`/`Vec`/`Map`/`Set`), so a user-struct
/// element was flat-memcpy'd and both frames freed the same field heap.
///
/// The fix ADDS a deep copy, which is the direction that trades a double
/// free for a leak — so the controls here matter as much as the failing
/// cells: cells 4 and 5 have no heap to duplicate and no envelope at all,
/// and LSan on the Linux CI leg is what keeps the new clone owned.
#[test]
fn asan_inline_optres_vec_payload_entry_copy_is_element_deep() {
    const S: &str = "struct S { s: String }\n";
    // 1 — the minimal reproducer: the param is never read.
    assert_clean_asan_run(
            &format!(
                "{S}fn plainV(x: Option[Vec[S]]) {{ println(\"in\"); }}\n\
                 fn main() {{ plainV(Some([S {{ s: f\"aaaaaaaa0\" }}, S {{ s: f\"bbbbbbbb1\" }}])); }}\n"
            ),
            &["in"],
            "b1-option-param-untouched",
        );
    // 2 — the `Result` half, which shares the resolution.
    assert_clean_asan_run(
            &format!(
                "{S}fn plainV(x: Result[Vec[S], i64]) {{\n\
                 \x20   match x {{ Ok(t) => {{ println(f\"s:{{t.len()}}\") }} Err(e) => {{ println(\"n\") }} }}\n\
                 }}\n\
                 fn main() {{ plainV(Ok([S {{ s: f\"aaaaaaaa0\" }}, S {{ s: f\"bbbbbbbb1\" }}])); }}\n"
            ),
            &["s:2"],
            "b1-result-ok-half",
        );
    // 3 — the arm reads an element through, so the copy must be correct
    //     and not merely balanced.
    assert_clean_asan_run(
            &format!(
                "{S}fn plainV(x: Option[Vec[S]]) {{\n\
                 \x20   match x {{ Some(t) => {{ println(f\"s:{{t[0].s}}\") }} None => {{ println(\"n\") }} }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let v: Vec[S] = [S {{ s: f\"aaaaaaaa0\" }}, S {{ s: f\"bbbbbbbb1\" }}];\n\
                 \x20   plainV(Some(v));\n\
                 }}\n"
            ),
            &["s:aaaaaaaa0"],
            "b1-arm-reads-element",
        );
    // 4 — CONTROL, leak direction: a no-heap element must not gain a
    //     clone it would then have to own.
    assert_clean_asan_run(
            "struct S3 { n: i64 }\n\
             fn plainV(x: Option[Vec[S3]]) {\n\
             \x20   match x { Some(t) => { println(f\"s:{t.len()}\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() { plainV(Some([S3 { n: 1 }, S3 { n: 2 }])); }\n",
            &["s:2"],
            "b1-no-heap-element-control",
        );
    // 5 — CONTROL: the same payload with no envelope, which never reaches
    //     the changed resolution.
    assert_clean_asan_run(
        &format!(
            "{S}fn plainV(x: Vec[S]) {{ println(f\"s:{{x.len()}}\"); }}\n\
                 fn main() {{ plainV([S {{ s: f\"aaaaaaaa0\" }}, S {{ s: f\"bbbbbbbb1\" }}]); }}\n"
        ),
        &["s:2"],
        "b1-bare-vec-argument-control",
    );
}

/// B-2026-09-13-18 — an `Array[T, N]` `Option` payload that FITS the inline
/// payload area was owned by nobody.
///
/// TWO INDEPENDENT GAPS, and the first alone does not close the row.
///
/// (1) ADMISSION. `inline_heap_payload_elem` — the `{ptr,len,cap}` overlay
/// gate behind `FreeInlineOptionPayload` — admitted `String`/`str`,
/// `Vec`/`VecDeque` and a transparent single-heap-field wrapper struct, and
/// never consulted `array_elem_and_len`. An `Array[String, 1]` is exactly
/// three words, so it is stored INLINE and every registration
/// B-2026-09-13-2 added declined it by construction (each reads word 0 as a
/// box pointer behind a width guard). A one-element array is admitted on
/// precisely the transparent-wrapper argument the struct arm already makes:
/// it lays out bit-identically to its single element at payload offset 0.
///
/// (2) DELIVERY, which is what the `Some(_)` cell below isolates. Fixing
/// admission alone made the wildcard arm clean and left `Some(a)` leaking:
/// binding the payload out DISARMS the source
/// (`suppress_inline_option_payload_cleanup` zeroes the overlay's cap to
/// transfer ownership) and nothing downstream took delivery, because
/// `bind_pattern_values`' `track_vec_var` arm — which owns the `Vec` and
/// `String` payloads of the identical program — has no array peer.
///
/// THE REGISTRATION IS PAIRED WITH THE DISARM rather than placed at the
/// binding site, and that is not a style choice. `bind_pattern_values` runs
/// BEFORE the suppression and cannot know whether the source will be
/// disarmed; registering there also covers the BOXED routes, whose
/// interiors already have owners. Measured: doing so double-freed — 3
/// codegen and 7 memory_sanitizer fixtures, ASAN `attempting double-free`.
///
/// CELLS. `wild` pins gap (1) — it was the first to go clean and would stay
/// clean if gap (2) regressed, so it is what keeps the two halves
/// distinguishable. `read` and `moved` pin gap (2) in both directions: a
/// missing owner leaks, and an owner registered without the disarm
/// double-frees. `scalar` is the control that must stay a no-op — an
/// `Array[i64, 3]` also fits the area and has nothing to free; it declines
/// because the recursion bottoms out on a primitive. `vec`/`str` are the
/// payload shapes that were always clean and must remain so.
///
/// NOT COVERED HERE, because it needed a registration this fixture's two
/// gaps do not reach: the FRESH-TEMP scrutinee spelling
/// (`match mk(j) { .. }` with no binding in between). It is fixed and has
/// its own fixture,
/// `asan_freshtemp_inline_option_scrutinee_has_exactly_one_owner` below.
#[test]
fn asan_inline_array_option_payload_has_exactly_one_owner() {
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> Option[Array[String, 1]] {
    if n < 0 { return None; }
    return Some(Array[f"row-aaaaaaaaaaaaaaaa-{n}"]);
}
fn mkscalar(n: i64) -> Option[Array[i64, 3]] {
    if n < 0 { return None; }
    return Some(Array[n, n + 1, n + 2]);
}
fn mkvec(n: i64) -> Option[Vec[String]] {
    if n < 0 { return None; }
    let mut v: Vec[String] = [];
    v.push(f"vec-aaaaaaaaaaaaaaaa-{n}");
    return Some(v);
}
fn mkstr(n: i64) -> Option[String] {
    if n < 0 { return None; }
    return Some(f"str-aaaaaaaaaaaaaaaa-{n}");
}

fn main() {
    let mut j: i64 = 0;
    while j < 4 {
        let w = mk(j);
        match w { Some(_) => { println("wild"); } None => { println("none"); } }
        let r = mk(j);
        match r { Some(a) => { println(f"read:{a[0]}"); } None => { println("none"); } }
        let m = mk(j);
        match m { Some(a) => { let t = a; println(f"moved:{t[0]}"); } None => { println("none"); } }
        let lo: Option[Array[String, 1]] = Some(Array[f"loc-aaaaaaaaaaaaaaaa-{j}"]);
        match lo { Some(a) => { println(f"local:{a[0]}"); } None => { println("none"); } }
        mk(j);
        let s = mkscalar(j);
        match s { Some(a) => { println(f"scalar:{a[0]}"); } None => { println("none"); } }
        let v = mkvec(j);
        match v { Some(a) => { println(f"vec:{a[0]}"); } None => { println("none"); } }
        let t = mkstr(j);
        match t { Some(a) => { println(f"str:{a}"); } None => { println("none"); } }
        j = j + 1;
    }
    println("end");
}
"#,
        &[
            "wild",
            "read:row-aaaaaaaaaaaaaaaa-0",
            "moved:row-aaaaaaaaaaaaaaaa-0",
            "local:loc-aaaaaaaaaaaaaaaa-0",
            "scalar:0",
            "vec:vec-aaaaaaaaaaaaaaaa-0",
            "str:str-aaaaaaaaaaaaaaaa-0",
            "wild",
            "read:row-aaaaaaaaaaaaaaaa-1",
            "moved:row-aaaaaaaaaaaaaaaa-1",
            "local:loc-aaaaaaaaaaaaaaaa-1",
            "scalar:1",
            "vec:vec-aaaaaaaaaaaaaaaa-1",
            "str:str-aaaaaaaaaaaaaaaa-1",
            "wild",
            "read:row-aaaaaaaaaaaaaaaa-2",
            "moved:row-aaaaaaaaaaaaaaaa-2",
            "local:loc-aaaaaaaaaaaaaaaa-2",
            "scalar:2",
            "vec:vec-aaaaaaaaaaaaaaaa-2",
            "str:str-aaaaaaaaaaaaaaaa-2",
            "wild",
            "read:row-aaaaaaaaaaaaaaaa-3",
            "moved:row-aaaaaaaaaaaaaaaa-3",
            "local:loc-aaaaaaaaaaaaaaaa-3",
            "scalar:3",
            "vec:vec-aaaaaaaaaaaaaaaa-3",
            "str:str-aaaaaaaaaaaaaaaa-3",
            "end",
        ],
        "asan_inline_array_option_payload_has_exactly_one_owner",
    );
}

/// B-2026-09-13-18, final spelling — a FRESH-TEMP inline-`Option`
/// scrutinee was owned by nobody. `match mk(j) { Some(a) => .. }` over
/// `fn mk(..) -> Option[Array[String, 1]]`, with no binding in between,
/// leaked its payload; so did the `Some(_)` wildcard arm, which is what
/// proved it was never a delivery problem. The `let`-bound spelling one
/// line away (`let o = mk(j); match o { .. }`) was already clean through
/// `track_inline_option_payload_var`.
///
/// THE PREAMBLE SIMPLY HAD NO PEER FOR IT. It carries
/// `track_freshtemp_boxed_enum_scrutinee` for a boxed payload,
/// `track_freshtemp_inline_result_scrutinee` for `Result`, and
/// `track_freshtemp_shared_option_scrutinee` for `Option[shared]` — and
/// nothing for a plain inline `Option`. The `Result` tracker's own comment
/// even records the consequence, that B-2026-08-29-6's `Option` spelling
/// was fixed "by the source-retains classification alone, because no
/// scrutinee registrar claims an inline `Option` temp". Adding one changes
/// that premise, which is why the new tracker copies that comment's
/// passthrough exclusion rather than assuming it unnecessary — `pass` below
/// is its cell.
///
/// IT WAS NOT ARRAY-ONLY, which the two bonus cells pin. `str` and `vec`
/// are `Option[String]` and `Option[Vec[String]]` fresh temps, and their
/// WILDCARD arms leaked on the pre-fix compiler too (51 B / 3 blocks and
/// 288 B + 51 indirect over three rounds) — the bound arms were clean
/// because `track_vec_var` owns the binding. So the hole was every inline
/// payload shape under a non-consuming arm, not the array shape the row
/// was filed on.
///
/// THE DISARM IS PAIRED WITH THE REGISTRATION, in the arm loop, for the
/// reason the sibling fixture above records at length: a registration
/// placed one step from its disarm double-freed 10 fixtures. The fresh-temp
/// disarm needs its own entry point because the named-source one resolves
/// its slot from an `ExprKind::Identifier` scrutinee, and a temp has no
/// name.
///
/// CELLS THAT MUST NOT DOUBLE-FREE, each a channel this registration could
/// have collided with, and all clean before AND after: `pass` (a
/// passthrough call is a fresh temp only in shape — the named source stays
/// armed), `held` (the `let`-bound spelling, which already had an owner),
/// `wrap` (a transparent single-heap-field struct the overlay frees
/// through), `node` (a `shared` payload, which routes to the RC tracker),
/// `scalar` (an `Array[i64, 3]` with nothing to free), `str-read` (a
/// borrow-only arm, where disarming would free the buffer out from under
/// the read), and `guard-*` (a guarded arm). `Map.get` and `Vec.pop`
/// scrutinees are covered by the borrow and pop paths and were verified
/// clean in both directions outside this fixture.
///
/// Measured whole-program at `-O0`: 486 B in 12 blocks plus 66 B indirect
/// in 3 before, clean after.
///
/// STILL LEAKING AND DELIBERATELY ABSENT: `Option[Array[String, 2]]`, whose
/// payload is six words and therefore BOXED, loses 246 B in 9 blocks plus
/// 102 B indirect in 6 — identically before and after, so it belongs to the
/// boxed scrutinee path rather than this one, and is filed separately.
#[test]
fn asan_freshtemp_inline_option_scrutinee_has_exactly_one_owner() {
    assert_clean_asan_run(
        r#"
struct Wrap { s: String }
shared struct Node { name: String }

fn mkarr(n: i64) -> Option[Array[String, 1]] {
    if n < 0 { return None; }
    return Some([f"arr-aaaaaaaaaaaaaaaa-{n}"]);
}
fn mkstr(n: i64) -> Option[String] {
    if n < 0 { return None; }
    return Some(f"str-bbbbbbbbbbbbbbbb-{n}");
}
fn mkvec(n: i64) -> Option[Vec[String]] {
    if n < 0 { return None; }
    let mut v: Vec[String] = Vec.new();
    v.push(f"vec-cccccccccccccccc-{n}");
    return Some(v);
}
fn mkscalar(n: i64) -> Option[Array[i64, 3]] {
    if n < 0 { return None; }
    return Some([n, n + 1, n + 2]);
}
fn mkwrap(n: i64) -> Option[Wrap] {
    if n < 0 { return None; }
    return Some(Wrap { s: f"wrp-dddddddddddddddd-{n}" });
}
fn mknode(n: i64) -> Option[Node] {
    if n < 0 { return None; }
    return Some(Node { name: f"nod-eeeeeeeeeeeeeeee-{n}" });
}
fn passthru(o: Option[String]) -> Option[String] { return o; }

fn main() {
    let mut n: i64 = 0;
    while n < 3 {
        match mkarr(n) { Some(a) => { println(f"arr-moved:{a[0]}"); } None => {} }
        match mkarr(n) { Some(_) => { println("arr-wild"); } None => {} }
        match mkstr(n) { Some(s) => { println(f"str-moved:{s}"); } None => {} }
        match mkstr(n) { Some(_) => { println("str-wild"); } None => {} }
        match mkstr(n) { Some(s) => { println(f"str-read:{s.len()}"); } None => {} }
        match mkvec(n) { Some(v) => { println(f"vec-moved:{v[0]}"); } None => {} }
        match mkvec(n) { Some(_) => { println("vec-wild"); } None => {} }
        match mkscalar(n) { Some(a) => { println(f"scalar:{a[0]}"); } None => {} }
        match mkwrap(n) { Some(w) => { println(f"wrap:{w.s}"); } None => {} }
        match mknode(n) { Some(d) => { println(f"node:{d.name}"); } None => {} }

        let src: Option[String] = Some(f"pas-ffffffffffffffff-{n}");
        match passthru(src) { Some(s) => { println(f"pass:{s}"); } None => {} }

        let held = mkarr(n);
        match held { Some(a) => { println(f"held:{a[0]}"); } None => {} }

        match mkstr(n) { Some(s) if n > 0 => { println(f"guard-hi:{s}"); } Some(s) => { println(f"guard-lo:{s}"); } None => {} }

        n = n + 1;
    }
    println("end");
}
"#,
        &[
            "arr-moved:arr-aaaaaaaaaaaaaaaa-0",
            "arr-wild",
            "str-moved:str-bbbbbbbbbbbbbbbb-0",
            "str-wild",
            "str-read:22",
            "vec-moved:vec-cccccccccccccccc-0",
            "vec-wild",
            "scalar:0",
            "wrap:wrp-dddddddddddddddd-0",
            "node:nod-eeeeeeeeeeeeeeee-0",
            "pass:pas-ffffffffffffffff-0",
            "held:arr-aaaaaaaaaaaaaaaa-0",
            "guard-lo:str-bbbbbbbbbbbbbbbb-0",
            "arr-moved:arr-aaaaaaaaaaaaaaaa-1",
            "arr-wild",
            "str-moved:str-bbbbbbbbbbbbbbbb-1",
            "str-wild",
            "str-read:22",
            "vec-moved:vec-cccccccccccccccc-1",
            "vec-wild",
            "scalar:1",
            "wrap:wrp-dddddddddddddddd-1",
            "node:nod-eeeeeeeeeeeeeeee-1",
            "pass:pas-ffffffffffffffff-1",
            "held:arr-aaaaaaaaaaaaaaaa-1",
            "guard-hi:str-bbbbbbbbbbbbbbbb-1",
            "arr-moved:arr-aaaaaaaaaaaaaaaa-2",
            "arr-wild",
            "str-moved:str-bbbbbbbbbbbbbbbb-2",
            "str-wild",
            "str-read:22",
            "vec-moved:vec-cccccccccccccccc-2",
            "vec-wild",
            "scalar:2",
            "wrap:wrp-dddddddddddddddd-2",
            "node:nod-eeeeeeeeeeeeeeee-2",
            "pass:pas-ffffffffffffffff-2",
            "held:arr-aaaaaaaaaaaaaaaa-2",
            "guard-hi:str-bbbbbbbbbbbbbbbb-2",
            "end",
        ],
        "asan_freshtemp_inline_option_scrutinee_has_exactly_one_owner",
    );
}

/// B-2026-09-20-49 — a `shared`/`par` enum's INLINE TUPLE payload has exactly
/// one owner, in every spelling that reaches it.
///
/// THE ROW'S OWN FRAMING IS WRONG AND THE FIXTURE IS NOT, which is worth
/// saying at the top because the row's title says `BoxedTuple`. Read from
/// the emitted IR rather than from the classifier: `shared enum Sh {
/// S((String, i64)), N }` lays out as
/// `%karac.shared.Sh = type { i64, i64, i64, i64, i64, i64 }` — rc, tag and
/// the tuple's four words INLINE, with no payload `malloc` anywhere in the
/// program. `payload_word_count_for_type_expr`'s tuple arm SUMS its
/// elements, so this shape never reaches the oversize-boxing path at all.
/// Nothing walked it because `emit_shared_enum_rc_drop_fn`'s
/// `field_is_walkable` classifies only `TypeKind::Path` shapes and a tuple
/// field has no path head, so the enum declined its rc-drop fn entirely and
/// `emit_rc_dec` plain-`free`d the shell.
///
/// The NON-SHARED twin of every cell below is clean on `main`, which is
/// what makes this a hole rather than a missing feature.
///
/// MEASURED at `KARAC_OPT_LEVEL=0 KARAC_AUTO_PAR=0` under
/// `valgrind --leak-check=full`, with an invalid-read and invalid-free
/// column on every cell, over 35 cells A/B'd against the unfixed tree by
/// NAME. Definitely-lost bytes, control -> fixed:
///
/// ```text
///   A temp source, arm reads                26 -> 0
///   B named source, arm reads               27 -> 0
///   C handle outlives the block             28 -> 0
///   D handle leaves the frame               29 -> 0
///   E three constructions in a `while`      99 -> 0
///   F two `Sh` elements of a `Vec`          68 -> 0
///   G constructed and never matched         37 -> 0
///   K the arm passes it to a free fn        28 -> 0
///   M struct-shaped variant, arm reads      30 -> 0
///   N a guard, then the arm reads           35 -> 0
///   P `par enum`, the Arc release path      30 -> 0
///   Q an arm reading ONE SCALAR ELEMENT     26 -> 0   (see below)
///
///   MUST STAY DECLINED -- clean on BOTH arms:
///   H the arm binds the payload into a local  0 -> 0
///   I the arm RETURNS the payload             0 -> 0
///   J a scalar tuple, no interior at all      0 -> 0
/// ```
///
/// THE LAST THREE ARE THE REAL GATE ON THIS CHANGE. The box walking its own
/// payload is only half of the fix: two of the fifteen original cells were
/// already clean because the ARM handed the payload to an owner, and
/// walking without standing that owner down double-frees exactly those. The
/// choice of which side gives way is forced rather than stylistic — in `I`
/// the payload OUTLIVES the box, whose release runs at that function's
/// exit, so retracting the arm's owner would hand the caller a freed buffer.
///
/// AND THE BOX IS NOT EMPTIED TO ACHIEVE THAT. Zeroing the box's payload
/// words is the obvious mirror — it is what the non-shared path does and
/// what the `BoxedArray` channel's alias disarm does — and it is silent
/// wrong output here, because a shared box is reached by every handle.
/// Measured: a second handle read `0` where `--interp` and the pre-fix tree
/// both print `7`, valgrind clean on every column, so no memory instrument
/// in this project could see it. The box RE-OWNS a deep copy instead. That
/// cell is pinned separately, as a codegen E2E, because the only spellings
/// that expose it draw an ownership warning and this fixture stays
/// warning-free.
///
/// CELL Q IS THE SECOND REGRESSION THIS FIXTURE GUARDS, and it is
/// B-2026-09-10-23's trap arriving by a different door. `fn peek(s: Sh) ->
/// i64 { match s { Sh.S(y) => { return y.1; } .. } }` reads the tuple's
/// `i64` and nothing else. The bare syntactic classifier calls any
/// projection off the binding a partial move, so the box handed its payload
/// to a binding that took nothing and had no cleanup — 26 B lost, on the
/// fix's own first cut. The leaf-aware `copy_read` policy that row
/// introduced is what answers it correctly.
///
/// EVERY CELL'S STRING HAS A DISTINCT LENGTH, so a future regression's byte
/// count names which cell moved instead of leaving a total to apportion.
///
/// CELL R IS THE THIRD REGRESSION, AND IT IS THE ONE THAT NEARLY GOT
/// WRITTEN DOWN AS A NON-EVENT. A scrutinee that is a struct FIELD
/// (`match w.h { Sh.S(x) => println(x.1) }`, read-only) measured 34 B lost
/// both before this fix and after it, so the first pass recorded it as
/// "unchanged, the walk never fired". The emitted IR says the walk DID
/// fire and the 34 B is a DIFFERENT OBJECT: that path reached
/// `suppress_destructured_struct_field_enum_cleanup`, which passed a
/// literal `None` for `arm_reads_only`, and `None` re-owns — so the box
/// allocated a deep copy and the ORIGINAL was left owned by nobody. The
/// alloc count is what separates the two readings and a byte total never
/// can: 12 allocs / 11 frees against a binding-scrutinee twin's 11 / 11.
/// Every caller holding the arm's body now computes the flag, and the cell
/// goes to 0 with the re-own no longer emitted at all.
///
/// `None` re-owning is the SAFE side of that choice rather than an
/// oversight: not re-owning under a MOVING arm double-frees, while
/// re-owning under a READ-ONLY arm only leaks. The `let … else` leg still
/// passes `None` because its binding outlives the construct and there is
/// no body to read there — which is exactly why `e06` below is still 31 B.
///
/// NOT COVERED, all three re-measured on the FIXED tree rather than
/// inferred: an `Array`-in-a-tuple payload (56 B) and an
/// `Option`-in-a-tuple payload (40 B), where `__karac_rc_drop_Sh` IS NOT
/// DEFINED AT ALL — the predicate above declines a tuple whose ELEMENT is
/// the non-`Path` shape, so those are this fix's own next spelling out;
/// and the `let else` spelling (31 B), where the walk fires, the re-own
/// fires on `None`, and 12 allocs / 11 frees say the lost block is the
/// copy. Those are B-2026-09-20-49's remainder and are filed separately.
#[test]
fn asan_shared_enum_inline_tuple_payload_has_an_owner() {
    assert_clean_asan_run_min_allocs(
        r#"
shared enum Sh { S((String, i64)), N }
shared enum St { S { a: (String, i64) }, N }
shared enum Sc { S((i64, i64)), N }
par enum Pr { S((String, i64)), N }

fn mkA(t: String) -> (String, i64) { return (f"A-{t}-a", 7); }
fn mkB(t: String) -> (String, i64) { return (f"B-{t}-aa", 7); }
fn mkC(t: String) -> (String, i64) { return (f"C-{t}-aaa", 7); }
fn mkD(t: String) -> (String, i64) { return (f"D-{t}-aaaa", 7); }
fn mkE(t: String) -> (String, i64) { return (f"E-{t}-aaaaa", 7); }
fn mkF(t: String) -> (String, i64) { return (f"F-{t}-aaaaaa", 7); }
fn mkG(t: String) -> (String, i64) { return (f"G-{t}-aaaaaaa", 7); }
fn mkH(t: String) -> (String, i64) { return (f"H-{t}-aaaaaaaa", 7); }
fn mkI(t: String) -> (String, i64) { return (f"I-{t}-aaaaaaaaa", 7); }
fn mkK(t: String) -> (String, i64) { return (f"K-{t}-aaaaaaaaaa", 7); }
fn mkL(t: String) -> (String, i64) { return (f"L-{t}-aaaaaaaaaaa", 7); }
fn mkM(t: String) -> (String, i64) { return (f"M-{t}-aaaaaaaaaaaa", 7); }
fn mkN(t: String) -> (String, i64) { return (f"N-{t}-aaaaaaaaaaaaa", 7); }
fn mkQ(t: String) -> (String, i64) { return (f"Q-{t}-aaaaaaaaaaaaaaa", 7); }
fn mkP(t: String) -> (String, i64) { return (f"P-{t}-aaaaaaaaaaaaaa", 7); }
fn mkR(t: String) -> (String, i64) { return (f"R-{t}-aaaaaaaaaaaaaaaa", 7); }
struct HolderR { h: Sh }

fn mkD2() -> Sh { let a = mkD("n"); return Sh.S(a); }
fn takeout(s: Sh) -> (String, i64) { match s { Sh.S(x) => { return x; } Sh.N => { return mkI("z"); } } }
fn takef(t: (String, i64)) { println(f"K:{t.0}"); }
fn peek(s: Sh) -> i64 { match s { Sh.S(y) => { return y.1; } Sh.N => { return -1; } } }

fn main() {
    { let s = Sh.S(mkA("t")); match s { Sh.S(x) => { println(f"A:{x.0}"); } Sh.N => { println("e"); } } }
    let b = mkB("n");
    { let s = Sh.S(b); match s { Sh.S(x) => { println(f"B:{x.0}"); } Sh.N => { println("e"); } } }
    let mut keep = Sh.N;
    { let c = mkC("n"); keep = Sh.S(c); }
    match keep { Sh.S(x) => { println(f"C:{x.0}"); } Sh.N => { println("e"); } }
    { let s = mkD2(); match s { Sh.S(x) => { println(f"D:{x.0}"); } Sh.N => { println("e"); } } }
    let mut i = 0;
    while i < 3 {
        { let s = Sh.S(mkE("t")); match s { Sh.S(x) => { println(f"E:{x.0}"); } Sh.N => { println("e"); } } }
        i = i + 1;
    }
    { let mut v: Vec[Sh] = Vec.new(); v.push(Sh.S(mkF("1"))); v.push(Sh.S(mkF("2"))); println(f"F:{v.len()}"); }
    { let s = Sh.S(mkG("t")); println("G:built"); }
    { let s = Sh.S(mkH("t")); match s { Sh.S(x) => { let u = x; println(f"H:{u.0}"); } Sh.N => { println("e"); } } }
    { let t = takeout(Sh.S(mkI("t"))); println(f"I:{t.0}"); }
    { let s = Sc.S((3, 4)); match s { Sc.S(x) => { println(f"J:{x.0}/{x.1}"); } Sc.N => { println("e"); } } }
    { let s = Sh.S(mkK("t")); match s { Sh.S(x) => { takef(x); } Sh.N => { println("e"); } } }
    { let l1 = Sh.S(mkL("t")); match l1 { Sh.S(x) => { let u = x; println(f"L:{u.0}"); } Sh.N => { println("e"); } } }
    { let q = Sh.S(mkQ("t")); println(f"Q:{peek(q)}"); }
    { let s = St.S { a: mkM("t") }; match s { St.S { a } => { println(f"M:{a.0}"); } St.N => { println("e"); } } }
    { let s = Sh.S(mkN("t")); match s { Sh.S(x) if x.1 > 3 => { println(f"N:{x.0}"); } Sh.S(y) => { println(f"N2:{y.0}"); } Sh.N => { println("e"); } } }
    { let s = Pr.S(mkP("t")); match s { Pr.S(x) => { println(f"P:{x.0}"); } Pr.N => { println("e"); } } }
    { let w = HolderR { h: Sh.S(mkR("t")) }; match w.h { Sh.S(x) => { println(f"R:{x.0}"); } Sh.N => { println("e"); } } }
    println("done");
}
"#,
        &[
            "A:A-t-a",
            "B:B-n-aa",
            "C:C-n-aaa",
            "D:D-n-aaaa",
            "E:E-t-aaaaa",
            "E:E-t-aaaaa",
            "E:E-t-aaaaa",
            "F:2",
            "G:built",
            "H:H-t-aaaaaaaa",
            "I:I-t-aaaaaaaaa",
            "J:3/4",
            "K:K-t-aaaaaaaaaa",
            "L:L-t-aaaaaaaaaaa",
            "Q:7",
            "M:M-t-aaaaaaaaaaaa",
            "N:N-t-aaaaaaaaaaaaa",
            "P:P-t-aaaaaaaaaaaaaa",
            "R:R-t-aaaaaaaaaaaaaaaa",
            "done",
        ],
        "asan_shared_enum_inline_tuple_payload_has_an_owner",
        30,
    );
}
