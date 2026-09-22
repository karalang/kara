//! inline/section/linkage attributes and emitted-IR shape -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen attrs_ir::
//!
//! New fixtures about inline/section/linkage attributes and emitted-IR shape belong in this file.

use super::*;

#[test]
fn e2e_inline_index_of_closure_vec_return() {
    // B-2026-07-18-43: an INLINE index of a closure-call result that returns
    // a Vec (`let g = || v; g()[i]`) failed codegen with "Index operator
    // applied to non-array type" — a closure callee has no
    // `fn_return_type_exprs` entry, and the `Call`/`Index` span collision
    // clobbers the element type. Binding to a temp first (`let r = g();
    // r[i]`) already worked. The closure's returned `Vec[T]` type is now
    // recorded at its let site (`closure_ret_vec_te`) and consulted by
    // `inline_temp_vec_te`. Covers a param capture, a local capture, a
    // Vec[String] element, and a block-body closure.
    if let Some(out) = run_program(
        "fn fp(v: Vec[i64]) -> i64 { let g = || v; g()[1] }\n\
             fn fl() -> i64 { let v = [10, 20, 30]; let g = || v; g()[2] }\n\
             fn fs(v: Vec[String]) -> String { let g = || v; g()[0] }\n\
             fn fb(v: Vec[i64]) -> i64 { let g = || { let n = 1; v }; g()[0] }\n\
             fn main() {\n\
                 println(fp([4, 5, 6]));\n\
                 println(fl());\n\
                 println(fs([\"a\".to_string(), \"b\".to_string()]));\n\
                 println(fb([7, 8]));\n\
             }",
    ) {
        assert_eq!(out, "5\n30\na\n7\n");
    }
}

/// B-2026-08-06-18: an INLINE u64 binop whose result exceeds `i64::MAX`
/// prints unsigned, matching the interpreter.
///
/// `println(u64.MAX - 1u64)` rendered -2 under both compiled backends while
/// the interpreter rendered 18446744073709551614 — and the SAME value bound
/// to a `let` first printed correctly on every surface, so the broken
/// spelling sat next to a working one with nothing to distinguish them.
///
/// The cause was not the print path being unaware of `u64`: it was that
/// `a - b` never reaches codegen as `ExprKind::Binary` at all.
/// `rewrite_binary` lowers every primitive binop to `u64.sub(a, b)`, and
/// `expr_is_unsigned_int`'s `Call` arm only understood an `Identifier`
/// callee, so the `Path` callee fell through to the signed default.
///
/// Covers the ops whose results can exceed i64::MAX (`-`, `&`, `|`, `^`,
/// `~`) plus the ones that cannot (`/`, `>>`) as controls, and a signed
/// binop to pin that signed rendering is untouched.
#[test]
fn e2e_inline_u64_binop_prints_unsigned() {
    let Some(out) = run_program(
        "fn main() {\n\
             \x20   let a: u64 = u64.MAX;\n\
             \x20   let one: u64 = 1u64;\n\
             \x20   println(a - one);\n\
             \x20   println(a & a);\n\
             \x20   println(a | one);\n\
             \x20   println(a ^ one);\n\
             \x20   println(~one);\n\
             \x20   println(f\"{a - one}\");\n\
             \x20   println(a / 2u64);\n\
             \x20   println(a >> 1u64);\n\
             \x20   let s: i64 = -5i64;\n\
             \x20   println(s - 1i64);\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "18446744073709551614\n\
             18446744073709551615\n\
             18446744073709551615\n\
             18446744073709551614\n\
             18446744073709551614\n\
             18446744073709551614\n\
             9223372036854775807\n\
             9223372036854775807\n\
             -6\n"
    );
}

/// B-2026-08-04-12 — `?` PROPAGATING an Err carries the payload's whole
/// width, not its first three words.
///
/// The non-converting branch of `compile_question`'s error staging built
/// its return words as a literal `vec![w0, w1, w2]`. `Err4 { msg: String,
/// code: i64 }` is 4 words and FITS `Result`'s 5-word area, so it stays
/// INLINE and never takes a pointer path — propagating it kept the String
/// and handed back garbage for `code` (measured as 0 and as 32 in two
/// programs, i.e. whatever was in the slot), silently, with `karac run`
/// correct.
///
/// Same three-word cap as B-2026-08-04-9's second defect, on the Err side.
/// The two controls are what make the axis legible: `Err6` is 6 words and
/// BOXES, so it rides a pointer in w0 and never depended on the cap; and
/// matching the same `Result` WITHOUT a `?` wrapper was always correct, so
/// it is the propagation and not the construction or the match.
///
/// A 3-word error struct is immune, which is most hand-written ones — the
/// reason this needs a payload-width axis of its own rather than one
/// representative error type.
#[test]
fn e2e_question_propagates_a_wide_inline_err_payload_whole() {
    let Some(out) = run_program(
        "struct Err6 { msg: String, codes: Vec[i64] }\n\
             struct Err4 { msg: String, code: i64 }\n\
             fn mk6(i: i64) -> Err6 {\n\
             \x20   let mut c: Vec[i64] = Vec.new();\n\
             \x20   c.push(i);\n\
             \x20   return Err6 { msg: f\"wide-err-{i}\", codes: c };\n\
             }\n\
             fn mk4(i: i64) -> Err4 { return Err4 { msg: f\"mid-err-{i}\", code: i }; }\n\
             fn fail6(i: i64) -> Result[i64, Err6] { return Result.Err(mk6(i)); }\n\
             fn fail4(i: i64) -> Result[i64, Err4] { return Result.Err(mk4(i)); }\n\
             fn prop6(i: i64) -> Result[i64, Err6] { let v = fail6(i)?; return Result.Ok(v); }\n\
             fn prop4(i: i64) -> Result[i64, Err4] { let v = fail4(i)?; return Result.Ok(v); }\n\
             fn main() {\n\
             \x20   match prop6(3i64) {\n\
             \x20       Result.Ok(v) => { println(f\"a:ok{v}\"); }\n\
             \x20       Result.Err(e) => { println(f\"a:{e.msg}:{e.codes.len()}\"); }\n\
             \x20   }\n\
             \x20   match prop4(4i64) {\n\
             \x20       Result.Ok(v) => { println(f\"b:ok{v}\"); }\n\
             \x20       Result.Err(e) => { println(f\"b:{e.msg}:{e.code}\"); }\n\
             \x20   }\n\
             \x20   match fail4(5i64) {\n\
             \x20       Result.Ok(v) => { println(f\"c:ok{v}\"); }\n\
             \x20       Result.Err(e) => { println(f\"c:{e.msg}:{e.code}\"); }\n\
             \x20   }\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a:wide-err-3:1\nb:mid-err-4:4\nc:mid-err-5:5\n");
}

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
#[test]
fn e2e_inline_struct_result_transfer_disarms_its_source() {
    let src = r#"struct R { id: i64, tag: String }
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
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            r#"rebind
  ok1
dR1/t1
rebindu
dR2/t2
  m
esc
  got3
dR3/t3
escerr
  got4
dR4/t4
esciflet
  got5
dR5/t5
call
  eat6
dR6/t6
inner
  got7
dR7/t7
strreb
  oks8
stresc
  gots9
optesc
  got10
dR10/t10
treb
dR11/t11
  ok111
dR111/t111
trebu
dR12/t12
dR112/t112
  m
tesc
dR13/t13
  got113
dR113/t113
freb
dR14/t14
  ok114
dR114/t114
fesc
dR15/t15
  got115
dR115/t115
wreb
  ok16
dW16/x16y16
wesc
  got17
dW17/x17y17
wtreb
dR18/t18
  ok118
dW118/x118y118
wfreb
dR19/t19
  ok119
dW119/x119y119
wfesc
dR20/t20
  got120
dW120/x120y120
done
"#
        )
    );
}

/// B-2026-09-16-1 — `s[a..b]` builds its descriptor inline instead of
/// through `karac_string_slice_into`.
///
/// The fast path re-implements the runtime's bounds test, its UTF-8
/// boundary test and its inline encoding in IR, and takes the call only
/// when one of them declines. Three ways that can go wrong and none of them
/// change the printed answer on the happy path alone, so the cases below
/// pin the edges:
///
///  * the EMPTY slice is `{null, 0, 0}` in the runtime, not an inline
///    empty — a descriptor that disagreed would compare unequal to
///    `String.new()`, which case 3 checks directly;
///  * 23 bytes is the inline capacity and 24 must fall to the heap route;
///  * a multibyte index that is not a char boundary must still reach the
///    runtime's fatal path, and one that IS a boundary must be sliced.
///
/// Case 8 is the aliasing question: the slice must own its bytes, so
/// growing the source afterwards cannot disturb it.
/// COVERS THE DEFAULT (`KARAC_SSO=0`) ARM ONLY, and the name has to be read
/// that way. `KARAC_SSO` is read once per process through a `OnceLock` at
/// codegen time and defaults to off; `tests/codegen.rs` compiles
/// in-process, so nothing here can turn it on. The inline/heap fast-path
/// routes this describes are covered at `KARAC_SSO=1` by
/// `test_sso_string_slice_routes_match_the_interpreter` in `tests/cli.rs`,
/// which spawns `karac`. Measured: with the heap route's result aggregate
/// deliberately pointing into the SOURCE buffer — a guaranteed double free
/// — every fixture in this file stayed green and the cli.rs one aborted.
#[test]
fn e2e_string_slice_inline_fast_path_matches_the_runtime() {
    let Some(out) = run_program(
        "fn main() {\n\
             \x20   let s = \"let mut fn if else while return for in match struct enum impl\";\n\
             \x20   let mut i = 0;\n\
             \x20   let mut n = 0;\n\
             \x20   while i < 20 { let t = s[i..(i + 3)]; n = n + t.len(); i = i + 1; }\n\
             \x20   println(f\"1 {n}\")\n\
             \x20   let e = s[5..5];\n\
             \x20   println(f\"2 {e.len()}\")\n\
             \x20   let e2 = String.new();\n\
             \x20   println(f\"3 {e == e2}\")\n\
             \x20   let a = s[0..23];\n\
             \x20   let b = s[0..24];\n\
             \x20   println(f\"4 {a.len()} {b.len()} [{a}] [{b}]\")\n\
             \x20   let full = s[0..s.len()];\n\
             \x20   println(f\"5 {full.len()} {full == s}\")\n\
             \x20   let m = \"h\\u{e9}llo w\\u{f6}rld\";\n\
             \x20   println(f\"6 [{m[0..1]}] [{m[1..3]}]\")\n\
             \x20   let mut src = String.new();\n\
             \x20   src.push_str(\"abcdefghij\");\n\
             \x20   let keep = src[2..6];\n\
             \x20   src.push_str(\"XXXXXXXXXX\");\n\
             \x20   println(f\"7 [{keep}]\")\n\
             \x20   println(\"end\")\n\
             }\n\
             ",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "1 60\n2 0\n3 true\n4 23 24 [let mut fn if else whil] [let mut fn if else while]\n\
             5 61 true\n6 [h] [\u{e9}]\n7 [cdef]\nend\n"
    );
}

/// B-2026-08-29-56 — a heap-carrying `Option` BOUND TO A LOCAL and returned
/// hands its payload to the caller; the callee must not free it on the way
/// out.
///
/// The binding's `let` arms a `FreeInlineOptionPayload` (cap-guarded at the
/// option's word 2). Seventeen CONSUMING positions retract that action when
/// the binding is moved away — struct-literal fields, call arguments, method
/// receivers, channel sends, a `let` RHS — and no ESCAPING position did, so
/// the callee freed the buffer on the way out and the caller freed it again:
/// `free(): double free detected in tcache 2`, on a program `--interp` runs
/// correctly. Same hole shape B-2026-08-28-15 found for the tuple-index peer
/// and B-2026-08-07-1 for the heap-BOXED payload; this is the inline sibling
/// of that one.
///
/// A double free aborts the process, so `run_program` returns `None` and
/// every row below fails as `None` against its expected string until the two
/// return positions are hooked.
///
/// Two rows exist because the ledger row asserted them CLEAN and measurement
/// disagreed: `caller-discards` (the caller never binds the result at all)
/// and `caller-holds` (`let r = collect(); println("held")`) both aborted, so
/// "the caller must consume the payload" was never an ingredient. The
/// `*-control` rows are shapes that were always clean and must stay so — a
/// bare-tail temporary registers no binding cleanup, a scalar payload has no
/// buffer, and a user enum takes an entirely different drop channel.
#[test]
fn e2e_option_local_returned_hands_its_inline_payload_to_the_caller() {
    const H: &str = "struct B { s: String }\n\
             enum E { A(String), B }\n\
             fn c_tail() -> Option[String] { let buf = Some(f\"zz\"); buf }\n\
             fn c_stmt() -> Option[String] { let buf = Some(f\"zz\"); return buf; }\n\
             fn c_bare() -> Option[String] { Some(f\"zz\") }\n\
             fn c_vec() -> Option[Vec[i64]] { let buf = Some([1, 2, 3]); buf }\n\
             fn c_struct() -> Option[B] { let buf = Some(B { s: f\"zz\" }); buf }\n\
             fn c_mut() -> Option[String] { let mut buf: Option[String] = None; buf = Some(f\"aa\"); buf }\n\
             fn c_two() -> Option[String] { let a = Some(f\"zz\"); let b = a; b }\n\
             fn c_int() -> Option[i64] { let buf = Some(7); buf }\n\
             fn c_enum() -> E { let buf = E.A(f\"zz\"); buf }\n\
             fn c_cond(n: i64) -> Option[String] {\n\
             \x20   let buf = Some(f\"zz\");\n\
             \x20   if n > 0 { return buf; }\n\
             \x20   match buf { Some(s) => println(f\"c{s}\"), None => println(\"cn\"), }\n\
             \x20   None\n\
             }\n";
    for (label, body, want) in [
        // THE ROW: the named binding returned as the body's tail.
        (
            "tail-local",
            "match c_tail() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "[zz]\npost\n",
        ),
        // The explicit-`return` spelling is a SEPARATE code path in
        // `exprs.rs`; it had the identical hole and needs its own hook.
        (
            "stmt-return",
            "match c_stmt() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "[zz]\npost\n",
        ),
        // Parity pin: the bare-tail spelling of the SAME value was always
        // clean (a temporary registers no binding cleanup). The fix makes
        // the two spellings agree rather than moving one of them.
        (
            "bare-tail-control",
            "match c_bare() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "[zz]\npost\n",
        ),
        // Not `String`-specific — any heap behind the payload.
        (
            "vec-payload",
            "match c_vec() { Some(v) => println(f\"{v.len()}\"), None => println(\"none\"), }\n",
            "3\npost\n",
        ),
        (
            "struct-payload",
            "match c_struct() { Some(b) => println(f\"[{b.s}]\"), None => println(\"none\"), }\n",
            "[zz]\npost\n",
        ),
        // The spelling `selfhost/src/parser.kara` actually uses (~18 sites):
        // an ANNOTATED `None` let, assigned later. It was shielded until
        // c793ad0 recorded the binding's concrete type instead of the
        // literal's, which is why this defect only then turned CI red.
        (
            "mut-annotated-then-assigned",
            "match c_mut() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "[aa]\npost\n",
        ),
        (
            "two-hop-let",
            "match c_two() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "[zz]\npost\n",
        ),
        // The ledger row called both of these clean. They were not: the
        // second free is the callee's own, so the caller need not touch the
        // value — or even bind it.
        ("caller-discards", "c_tail();\n", "post\n"),
        (
            "caller-holds",
            "let r = c_tail(); println(\"held\");\n",
            "held\npost\n",
        ),
        // CONTROLS that must stay clean: no heap behind the payload, and a
        // user enum (a different drop channel entirely).
        (
            "int-payload-control",
            "match c_int() { Some(n) => println(f\"{n}\"), None => println(\"none\"), }\n",
            "7\npost\n",
        ),
        (
            "user-enum-control",
            "match c_enum() { E.A(s) => println(f\"[{s}]\"), E.B => println(\"b\"), }\n",
            "[zz]\npost\n",
        ),
        // GUARD in the leak direction, and the reason the disarm is a
        // RUNTIME whole-slot zero rather than a compile-time queue retract:
        // one path returns the binding, the other consumes it. A static
        // retraction would strand the payload on the consuming path, trading
        // this double free for a leak, so both paths are pinned here.
        (
            "cond-returned",
            "match c_cond(1) { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "[zz]\npost\n",
        ),
        (
            "cond-consumed",
            "match c_cond(0) { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "czz\nnone\npost\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run_program(&src).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-07-31-6 — a `Result[T, E]` whose `Ok` payload is HEAP-BOXED must
/// not also get the INLINE payload cleanup.
///
/// `coerce_to_payload_words` spills a too-wide payload behind a pointer in
/// word 0, and `BoxedEnumDrop` walks it correctly through the box. The
/// inline overlay was registered on top, and it reads word 1 — the box
/// POINTER — as the first word of the payload struct. For `Res { name:
/// String, buf: Vec[i64] }` that made `karac_drop_Res` run over
/// `{box_ptr, 0, 0}`: an empty-`Res` body `karac run` never printed, and a
/// free of whatever those words held (benign only because they were zero).
///
/// `Res` needs TWO heap fields so the payload exceeds the inline word
/// budget and is actually boxed — a single-heap-field payload stays inline,
/// the overlay is then correct, and the test pins nothing.
#[test]
fn e2e_boxed_result_payload_runs_no_inline_overlay_drop() {
    let Some(out) = run_program(
        "struct Res { name: String, buf: Vec[i64] }\n\
             impl Drop for Res { fn drop(mut ref self) { println(f\"D {self.buf.len()}\"); } }\n\
             fn mkres() -> Res {\n\
             \x20   let mut b: Vec[i64] = Vec.new();\n\
             \x20   b.push(1i64);\n\
             \x20   return Res { name: \"payload\", buf: b };\n\
             }\n\
             fn main() {\n\
             \x20   let d: Result[Res, i64] = Result.Ok(mkres());\n\
             \x20   println(\"d\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    // Exactly one `D 1` line, with the REAL buf length — the payload body
    // now runs through the box via the Option/Result payload walk
    // (B-2026-07-30-11, landed), at the binding's NLL point on both
    // backends. What this test still pins is the -31-6 defect: no `D 0`
    // from the inline overlay reading the box POINTER as struct bytes,
    // and no second body line of any kind.
    assert_eq!(out, "D 1\nd\nend\n");
}

/// B-2026-08-22-27, the half that a per-hasher symbol name would NOT have
/// fixed: two `Map[i64, i64]` bindings with DIFFERENT hashers in one
/// module. They share a single `linkonce_odr` mono family by design, so
/// the emitted body cannot bake in either hasher — it has to read the one
/// the receiver was built with. Loading the stored pointer makes the
/// shared symbol correct for both instead of merely distinguishing them.
#[test]
fn two_maps_with_different_hashers_share_one_mono_symbol_safely() {
    let src = "fn main() {\n\
            \x20   let mut d: Map[i64, i64] = Map.new();\n\
            \x20   let mut f: Map[i64, i64, FxBuildHasher] = Map.new();\n\
            \x20   let mut i = 0;\n\
            \x20   while i < 500 { d.insert(i, i); f.insert(i, i * 2); i = i + 1; }\n\
            \x20   let mut bad = 0;\n\
            \x20   let mut j = 0;\n\
            \x20   while j < 500 {\n\
            \x20       if d.get(j).unwrap_or(-1) != j { bad = bad + 1; }\n\
            \x20       if f.get(j).unwrap_or(-1) != j * 2 { bad = bad + 1; }\n\
            \x20       j = j + 1;\n\
            \x20   }\n\
            \x20   println(bad);\n\
            }\n";
    assert_eq!(run_program(src).as_deref(), Some("0\n"));
}

#[test]
fn test_ir_generic_mono_symbols_are_distinct_per_float_width() {
    // Every float width instantiating one generic must get its OWN mono
    // symbol. `llvm_type_to_mangle_str` only ever special-cased `f32`, so
    // `half`, `bfloat` and `double` all answered "f64" — and the NAME
    // channel that exists to correct exactly this erasure for the narrow
    // INTS (`is_scalar_primitive_mangle_name`, B-2026-07-03-24) did not
    // list `f16`/`bf16` either. Both instantiations therefore mangled to
    // `g_add$f64`, the second reused the first's body, and codegen bridged
    // the mismatch at the CALL with `fpext`/`fptrunc`: `g_add` at bf16
    // silently computed at f16 (B-2026-08-30-36).
    //
    // The bodies were always typed correctly — a program using ONE of
    // {f16, bf16, f64} was fine — so only a program mixing two of them
    // showed it, which is why an isolated per-width test would not have
    // caught this and all four widths are instantiated here together.
    let ir = ir_for(
        "fn g_add[T: Add](a: T, b: T) -> T { a + b }\n\
             fn main() {\n\
                 let h: f16 = 1.5; let b: bf16 = 1.5;\n\
                 let s: f32 = 1.5; let d: f64 = 1.5;\n\
                 println(f\"{g_add(h,h)} {g_add(b,b)} {g_add(s,s)} {g_add(d,d)}\");\n\
             }",
    );
    for (sym, ty) in [
        ("g_add$f16", "half"),
        ("g_add$bf16", "bfloat"),
        ("g_add$f32", "float"),
        ("g_add$f64", "double"),
    ] {
        assert!(
            ir.contains(&format!("@\"{sym}\"({ty} ")),
            "expected a distinct `{sym}` monomorph taking `{ty}`; IR:\n{ir}"
        );
    }
}

#[test]
fn test_ir_generic_mono_symbols_are_distinct_per_128_bit_signedness() {
    // The integer twin of the float-width test above, and the sharper case
    // (B-2026-08-30-45). `llvm_type_to_mangle_str` renders an integer by BIT
    // WIDTH, and LLVM's `IntType` carries no signedness at all — so `i128`
    // and `u128` do not merely happen to collide, they are structurally
    // indistinguishable in that channel and no fix there is possible. The
    // NAME channel (`is_scalar_primitive_mangle_name`) is the only place the
    // declared type survives, and its list stopped at 64 bits.
    //
    // Asserted on the symbol rather than the argument type on purpose: BOTH
    // monomorphs take `i128` in the IR, because that is the whole point —
    // signedness is not in the LLVM type. Two distinct symbols is therefore
    // exactly what "these are different functions" can look like here.
    let ir = ir_for(
        "fn show[T](x: T) -> String { f\"{x}\" }\n\
             fn main() {\n\
                 let a: i128 = -7i128;\n\
                 let b: u128 = 340282366920938463463374607431768211455u128;\n\
                 println(show(a)); println(show(b));\n\
             }",
    );
    for sym in ["show$i128", "show$u128"] {
        assert!(
            ir.contains(&format!("@\"{sym}\"(")),
            "expected a distinct `{sym}` monomorph; IR:\n{ir}"
        );
    }
}

#[test]
fn test_ir_heuristic_inline_hint_on_small_helper() {
    // A small leaf helper with no user `#[inline]` gets a compiler-driven
    // `inlinehint` (phase-11 Codegen Optimization) — proves the
    // `inline_hints::compute` decision reaches the LLVM attribute.
    let ir =
        ir_for("fn helper(a: i64, b: i64) -> i64 { a + b }\nfn main() { let _ = helper(1, 2); }");
    assert!(
        ir.contains("inlinehint"),
        "expected a heuristic `inlinehint` attribute; IR:\n{ir}"
    );
}

#[test]
fn test_ir_user_inline_always_still_emitted() {
    // Regression guard on the user-hint path (the heuristic composes with
    // it, user wins): `#[inline(always)]` still lowers to `alwaysinline`.
    let ir = ir_for(
            "#[inline(always)]\nfn helper(a: i64, b: i64) -> i64 { a + b }\nfn main() { let _ = helper(1, 2); }",
        );
    assert!(
        ir.contains("alwaysinline"),
        "expected user `#[inline(always)]` → `alwaysinline`; IR:\n{ir}"
    );
}

#[test]
fn test_ir_target_feature_emits_function_attribute() {
    // Phase-11 `#[target_feature(enable = "...")]` (design.md § Multiversioning,
    // floor/ceiling composition): the attribute lowers to a per-function LLVM
    // `target-features` string attribute widening this function above the
    // module baseline. A comma-list yields `+`-prefixed features. The function
    // must be `unsafe fn` (validated in the parser); a plain function without
    // the attribute gets no such per-function attribute.
    let ir = ir_for(
        "#[target_feature(enable = \"avx2,bmi2\")]\n\
             unsafe fn hot(a: i64, b: i64) -> i64 { a + b }\n\
             fn cold_fn(a: i64) -> i64 { a }\n\
             fn main() { let _ = unsafe { hot(1, 2) }; let _ = cold_fn(3); }",
    );
    assert!(
        ir.contains("+avx2") && ir.contains("+bmi2"),
        "expected `#[target_feature]` → `+avx2,+bmi2` in a target-features attribute; IR:\n{ir}"
    );
    assert!(
        ir.contains("\"target-features\""),
        "expected an LLVM `target-features` function attribute; IR:\n{ir}"
    );
}

#[test]
fn test_ir_multiversion_method_and_generic_target_features() {
    // The method desugar synthesizes `$baseline` + `$<feat>` sibling methods
    // (mangled with the impl target) each tagged `target-features`; the
    // generic desugar's variants carry `target-features` through
    // monomorphization (declare_mono_function re-emits the attribute). Both
    // dispatch via `karac_cpu_supports`.
    let ir = ir_for_desugared(
        "struct Acc { base: i64 }\n\
             impl Acc {\n\
             #[multiversion(baseline, \"avx2\")]\n\
             fn dot(ref self, x: i64) -> i64 { self.base + x }\n\
             }\n\
             #[multiversion(baseline, \"avx512f\")]\n\
             fn gadd[T: Add](a: T, b: T) -> T { a + b }\n\
             fn main() {\n\
             let a = Acc { base: 1 };\n\
             let _ = a.dot(2);\n\
             let _ = gadd(3, 4);\n\
             }",
    );
    // Method variant target-feature + generic-mono target-feature both present.
    assert!(
        ir.contains("+avx2"),
        "expected the method variant's target-features (+avx2); IR:\n{ir}"
    );
    assert!(
        ir.contains("+avx512f"),
        "expected the generic-mono variant's target-features (+avx512f) — \
             declare_mono_function must re-emit the attribute; IR:\n{ir}"
    );
    assert!(
        ir.contains("karac_cpu_supports"),
        "expected the dispatch thunks to call karac_cpu_supports; IR:\n{ir}"
    );
}

#[test]
fn ir_vec_len_load_carries_range_metadata() {
    // B-2026-07-10-5: a Vec `.len()` load carries `!range [0, 2^61)` —
    // sound because every collection buffer allocation is capped at
    // `KARAC_MAX_ALLOC_BYTES = 2^61 - 1` bytes by the runtime wrappers
    // (`runtime/src/alloc.rs`), so `len <= cap <= cap * elem_size < 2^61`
    // for any non-zero-sized element. The fact lets LLVM fold overflow
    // checks on len-derived arithmetic (`n + 1`, `l + 1` under a
    // dominating bounds check), reaching instruction parity with
    // equal-safety rustc on the #76 two-pointer loop shape.
    let ir = ir_for(
        "fn tail(s: ref Vec[i64]) -> i64 {\n\
                 let n = s.len();\n\
                 n + 1\n\
             }\n\
             fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(3);\n\
                 println(tail(v));\n\
             }",
    );
    assert!(
        ir.contains("%vec.len = load i64, ptr %vec.len.ptr, align 8, !range"),
        "Vec .len() load must carry !range metadata, got IR:\n{ir}"
    );
    assert!(
        ir.contains("!{i64 0, i64 2305843009213693952}"),
        "!range bounds must be [0, 2^61), got IR:\n{ir}"
    );
}

#[test]
fn ir_zero_sized_elem_vec_len_has_no_range_metadata() {
    // `struct E {}` is zero-sized: a `Vec[E]` never allocates, so its
    // len/cap are NOT bounded by the allocator byte ceiling and the
    // `!range` claim would be unsound — the annotation must not fire.
    let ir = ir_for(
        "struct E {}\n\
             fn main() {\n\
                 let mut v: Vec[E] = Vec.new();\n\
                 v.push(E {});\n\
                 println(v.len());\n\
             }",
    );
    assert!(
        !ir.contains("!range"),
        "zero-sized-element Vec len loads must NOT carry !range, got IR:\n{ir}"
    );
}

#[test]
fn test_e2e_extern_link_name_binds_foreign_symbol() {
    // `#[link_name("strlen")]` redirects the emitted symbol from the
    // Kāra fn name (`measure`) to the foreign symbol (`strlen`), so a
    // snake_case Kāra name can bind a differently-spelled C symbol —
    // the mechanism the self-hosted LLVM-C binding needs to call the
    // PascalCase `LLVMContextCreate` family
    // (`docs/spikes/self-hosting-llvm-c-ffi.md` § Linking). `strlen`
    // also exercises the dedup-against-a-codegen-builtin path (codegen
    // pre-declares `strlen` in `Codegen::new`), proving the import
    // reuses that symbol instead of emitting a renamed `strlen.1`.
    let src = r#"
unsafe extern "C" {
    #[link_name("strlen")]
    fn measure(s: *const u8) -> i64;
}

fn main() {
    let s = c"hello, world";
    // Safety: `c"..."` is NUL-terminated; strlen reads to the NUL.
    let n = unsafe { measure(s.as_ptr()) };
    println(n);
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out.trim(), "12"); // "hello, world" — 12 bytes
    }
}

#[test]
fn test_ir_full_unroll_metadata_gated_on_small_constant_bound() {
    // The full-unroll hint is attached only to small constant-trip
    // counted loops. `while d <= 9` (constant step `d = d + 1`, bound 9
    // <= 32) gets `llvm.loop.unroll.full`; `while i < 100000` (bound far
    // over the cap) does NOT — its trip count is large, so unrolling
    // would bloat. Both are real loops; only the gate differs.
    let eligible = ir_for(
        "fn main() { let mut d = 1i64; let mut s = 0i64; \
             while d <= 9i64 { s = s + d; d = d + 1i64; } println(s); }",
    );
    assert!(
        eligible.contains("llvm.loop.unroll.full"),
        "small `while d <= 9` counted loop should carry the full-unroll hint; IR:\n{eligible}"
    );
    let ineligible = ir_for(
        "fn main() { let mut i = 0i64; let mut s = 0i64; \
             while i < 100000i64 { s = s + i; i = i + 1i64; } println(s); }",
    );
    assert!(
            !ineligible.contains("llvm.loop.unroll.full"),
            "large-bound `while i < 100000` loop must NOT carry the full-unroll hint; IR:\n{ineligible}"
        );
}

/// B-2026-08-13-10 — the bound may be a NAME bound to an immutable integer
/// literal, not only a literal written in the guard. `let k = 32i64; …
/// while j < k` is the commoner spelling of a counted loop, and requiring
/// a literal meant the very shape this hint exists for usually missed it:
/// LLVM had already constant-propagated the bound (`cmp $0x20` in the
/// disassembly) but unrolled by only 4 and kept a data-dependent branch
/// per element, where rustc and clang unroll the same loop whole and
/// branchlessly. Measured 2.33x on kata #265's O(n*k) DP once hinted
/// (605 -> 260 ms), which took kara from 1.5x behind equal-safety rustc to
/// ahead of it.
///
/// The three negatives are the gate, and each fails for its own reason:
/// a `mut` bound could be reassigned, a non-literal initializer is not a
/// compile-time constant, and a name over the cap is the same
/// bloat-refusal the literal path already makes.
#[test]
fn test_ir_full_unroll_metadata_accepts_a_const_local_bound() {
    let named = ir_for(
        "fn main() { let k = 32i64; let mut j = 0i64; let mut s = 0i64; \
             while j < k { s = s + j; j = j + 1i64; } println(s); }",
    );
    assert!(
        named.contains("llvm.loop.unroll.full"),
        "`let k = 32i64; while j < k` should carry the full-unroll hint; IR:\n{named}"
    );

    // `mut` bound: reassignable, so the name is not a constant.
    let mutable = ir_for(
        "fn main() { let mut k = 32i64; let mut j = 0i64; let mut s = 0i64; \
             while j < k { s = s + j; j = j + 1i64; } k = k + 1i64; println(s + k); }",
    );
    assert!(
        !mutable.contains("llvm.loop.unroll.full"),
        "`let mut k` bound must NOT be treated as a constant; IR:\n{mutable}"
    );

    // Non-literal initializer: not known at compile time.
    let computed = ir_for(
        "fn main() { let n = env.args().len() as i64; let k = n + 8i64; \
             let mut j = 0i64; let mut s = 0i64; \
             while j < k { s = s + j; j = j + 1i64; } println(s); }",
    );
    assert!(
        !computed.contains("llvm.loop.unroll.full"),
        "a computed bound must NOT carry the full-unroll hint; IR:\n{computed}"
    );

    // Over the cap: same refusal the literal path makes at 100000.
    let too_big = ir_for(
        "fn main() { let k = 100000i64; let mut j = 0i64; let mut s = 0i64; \
             while j < k { s = s + j; j = j + 1i64; } println(s); }",
    );
    assert!(
        !too_big.contains("llvm.loop.unroll.full"),
        "a const-local bound over the cap must NOT carry the hint; IR:\n{too_big}"
    );
}

#[test]
fn test_ir_partial_unroll_metadata_gated_on_scalar_recurrence_body() {
    // A runtime-trip counted loop whose body is pure-scalar (the
    // Fibonacci-recurrence shape, kata #70) carries
    // `llvm.loop.unroll.count` — LLVM 18's cost model wrongly declines
    // these loop-carried scalar recurrences, so karac forces a partial
    // unroll for a measured ~1.5× (B-2026-07-08-24). A MEMORY-bound loop
    // (array read/write in the body) must NOT: forcing an unroll count
    // there only bloats it, so the scalar-only body gate excludes it.
    let scalar = ir_for(
        "fn fib(n: i64) -> i64 { let mut a = 1i64; let mut b = 2i64; let mut i = 3i64; \
             while i <= n { let next = a + b; a = b; b = next; i = i + 1i64; } b } \
             fn main() { println(fib(40i64)); }",
    );
    assert!(
        scalar.contains("llvm.loop.unroll.count"),
        "scalar-recurrence `while i <= n` loop should carry the partial-unroll hint; IR:\n{scalar}"
    );
    let memory = ir_for(
        "fn main() { let mut v: Vec[i64] = Vec.filled(64i64, 0i64); let mut s = 0i64; \
             let mut c = 0i64; while c < 64i64 { s = s + v[c]; c = c + 1i64; } println(s); }",
    );
    assert!(
            !memory.contains("llvm.loop.unroll.count"),
            "memory-bound `while c < 64 {{ s += v[c] }}` loop must NOT carry the partial-unroll hint; IR:\n{memory}"
        );
}

#[test]
fn test_e2e_option_inline_payload_drop_paths() {
    // B-2026-06-10-6: an inline-heap `Option[String]` dropped without
    // being destructured must free its payload (no leak), and a
    // `match`/`if let` that binds the payload must NOT double-free it.
    // This test pins OUTPUT correctness across the let-unused, discard,
    // match-consume, and if-let-consume shapes (the memory-safety side
    // is pinned by `tests/memory_sanitizer.rs::asan_option_*`). A wrong
    // suppression would manifest here as a corrupted/empty print or a
    // crash before the expected lines.
    let out = run_program(
        r#"
fn mk(n: i64) -> Option[String] { Some(f"v{n}") }
fn main() {
    let unused = mk(1);          // freed at scope exit, never read
    mk(2);                        // discarded temp, freed at `;`
    let bound = mk(3);
    match bound {                 // arm binds payload → source free suppressed
        Some(s) => { println(s); }
        None => { println("none"); }
    };
    let cond = mk(4);
    if let Some(s) = cond {       // if-let binding → source free suppressed
        println(s);
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["v3", "v4", "end"]);
    }
}

#[test]
fn test_e2e_result_inline_payload_drop_paths() {
    // B-2026-06-10-6 Result follow-on: inline-heap `Result` payloads
    // (`Ok(String)` / `Err(String)`) dropped undestructured must free
    // (no leak), and a `match` binding the payload must not double-free.
    // Pins OUTPUT across the Ok-heap, Err-heap, and consumed shapes (the
    // memory side is pinned by `memory_sanitizer.rs::asan_result_*`).
    let out = run_program(
        r#"
fn ok_s(n: i64) -> Result[String, i64] { Ok(f"ok{n}") }
fn err_s(bad: bool) -> Result[i64, String] {
    if bad { Err(f"e{9}") } else { Ok(1i64) }
}
fn main() {
    let unused = ok_s(1);        // freed at scope exit, never read
    let _e = err_s(true);        // Err-side heap freed at scope exit
    let bound = ok_s(3);
    match bound {                 // arm binds payload → source free suppressed
        Ok(s) => { println(s); }
        Err(_n) => { println("err"); }
    };
    println("end");
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["ok3", "end"]);
    }
}

#[test]
fn test_ir_user_drop_fires_for_inline_temp_call_arg() {
    // B-2026-06-10 — a Drop-typed temporary materialized DIRECTLY as a
    // call argument (`consume(Guard { id: 1 })`) is caller-owned under the
    // caller-drops convention, exactly like a let-bound arg. Before the
    // fix, the inline-temp arg path (`track_inline_owned_aggregate_arg`)
    // never consulted `drop_method_keys`, so a heap-free `Guard`'s user
    // `drop` never fired and the temporary leaked. The caller's body must
    // now run `@karac_drop_Guard` once for the temp.
    let ir = ir_for(
        r#"
struct Guard { id: i64 }
impl Drop for Guard {
    fn drop(mut ref self) {}
}
fn consume(g: Guard) {}
fn main() {
    consume(Guard { id: 1 });
}
"#,
    );
    let main_body = function_body(&ir, "main").unwrap_or_else(|| {
        panic!("main body not found in IR:\n{}", ir);
    });
    assert!(
        main_body.contains("call void @karac_drop_Guard("),
        "expected `main` to call `@karac_drop_Guard(...)` for the inline \
             temporary argument; body was:\n{}",
        main_body
    );
    assert!(
        !main_body.contains("call void @__karac_drop_struct_Guard("),
        "main must NOT also emit a direct `@__karac_drop_struct_Guard` \
             call — UserDrop and StructDrop are mutually exclusive (the \
             wrapper handles field cleanup internally). body was:\n{}",
        main_body
    );
}

#[test]
fn test_e2e_user_drop_fires_once_for_inline_temp_call_arg() {
    // B-2026-06-10 behavioral check: a Drop-typed temporary passed
    // directly as a call argument must run its user `drop` EXACTLY once —
    // not zero (the original leak) and not twice (a double-drop from
    // registering both UserDrop and StructDrop). A side-effecting `drop`
    // body makes the count observable without needing LSan.
    let out = run_program(
        r#"
struct Guard { id: i64 }
impl Drop for Guard {
    fn drop(mut ref self) { println(f"drop {self.id}"); }
}
fn consume(g: Guard) {}
fn main() {
    consume(Guard { id: 7 });
    println("after");
}
"#,
    )
    .expect("program should compile and run");
    assert_eq!(
        out.matches("drop 7").count(),
        1,
        "inline-temp Guard drop must fire exactly once; got:\n{out}"
    );
    assert!(
        out.contains("after"),
        "program body should run to completion; got:\n{out}"
    );
}

#[test]
fn test_e2e_user_drop_fires_once_for_inline_temp_enum_call_arg() {
    // B-2026-06-10 carry-forward (enum arm): a Drop-typed ENUM
    // temporary passed directly as a call argument must run its user
    // `drop` exactly once, across all three fresh-temp shapes —
    // payload constructor `Sig.A(7)` (Call), unit variant `Sig.B`
    // (Path), and struct variant `Sig.C { x }` (StructLiteral with an
    // enum owner). Pre-fix, the Call arm registered only the
    // payload-walking EnumDrop (nothing at all for a heap-free enum),
    // so the user body never fired. The let-bound arg pins the
    // pre-existing binding path against double-drop regressions.
    let out = run_program(
        r#"
enum Sig { A(i64), B, C { x: i64 } }
impl Drop for Sig {
    fn drop(mut ref self) { println("dropped"); }
}
fn consume(s: Sig) {}
fn main() {
    consume(Sig.A(7));
    consume(Sig.B);
    consume(Sig.C { x: 3 });
    let g = Sig.B;
    consume(g);
    println("after");
}
"#,
    )
    .expect("program should compile and run");
    assert_eq!(
        out.matches("dropped").count(),
        4,
        "three inline enum temps + one let-bound binding must each \
             drop exactly once; got:\n{out}"
    );
    assert!(
        out.contains("after"),
        "program body should run to completion; got:\n{out}"
    );
}

#[test]
fn test_ir_interproc_row_helper_bce_skip_wiring() {
    // B-2026-08-05-6: `row_scan` walks a caller-owned buffer at a
    // caller-chosen offset. Every fact the intra-function converging skip
    // needs is a parameter value here, so `bce_length_pin` alone leaves
    // both checks standing. `bce_interproc` infers the precondition
    // `base + (len - 1) < v.len()`, discharges it at the one call site
    // (`base = i * len`, `i < n`, `v.len() >= n * len`), and installs the
    // same `ConvergingSkip` record — so the loads carry no check at all.
    let ir = ir_for(&interproc_row_helper_src(
        "while i < n { acc = acc + row_scan(v, i * len, len); i = i + 1i64; }",
    ));
    assert!(
        !ir.contains("vidx."),
        "expected the row-helper loads to carry NO bounds check once the \
             interprocedural precondition is discharged, but found a `vidx.` \
             block:\n{ir}"
    );
}

#[test]
fn test_ir_interproc_skip_refused_when_one_call_site_is_undischarged() {
    // ONE body is emitted, so ONE fact must cover every call. A second
    // call site passing an out-of-range `base` cannot discharge, and that
    // disqualifies the callee outright — the good site does NOT get a
    // specialised copy. Without this the program would read out of bounds
    // through the very call that is in range everywhere else.
    let ir = ir_for(&interproc_row_helper_src(
        "while i < n { acc = acc + row_scan(v, i * len, len); i = i + 1i64; }\n    \
             acc = acc + row_scan(v, n * len - 2i64, len);",
    ));
    assert!(
        ir.contains("vidx."),
        "expected the row-helper loads to KEEP their bounds check when a \
             second call site cannot discharge the precondition, got:\n{ir}"
    );
}

#[test]
fn test_ir_interproc_skip_refused_for_off_by_one_caller_bound() {
    // The caller's guard is `i <= n`, not `i < n`, so the last row starts
    // at `n * len` and runs one whole row past the buffer. The linear
    // cancellation must fail: `n * len - (n * len + len - 1)` is not a
    // positive constant.
    let ir = ir_for(&interproc_row_helper_src(
        "while i <= n { acc = acc + row_scan(v, i * len, len); i = i + 1i64; }",
    ));
    assert!(
        ir.contains("vidx."),
        "expected the row-helper loads to KEEP their bounds check under an \
             off-by-one caller bound, got:\n{ir}"
    );
}

#[test]
fn e2e_interproc_row_helper_reads_the_right_elements() {
    // The IR tests above assert the check is GONE; this one asserts the
    // loads still land on the right elements once it is. Every position
    // holds a distinct value (`k * k + 1`), so an off-by-one in either
    // index — or a row walked at the wrong base — changes the sum. Oracle
    // is the interpreter's answer for the same source.
    let out = run_program(
        "fn row_scan(v: ref Vec[i64], base: i64, len: i64) -> i64 {\n\
                 let mut lo = 0i64;\n\
                 let mut hi = len - 1i64;\n\
                 let mut acc = 0i64;\n\
                 while lo <= hi {\n\
                     acc = acc + v[base + lo] * 3i64 - v[base + hi];\n\
                     lo = lo + 1i64;\n\
                     hi = hi - 1i64;\n\
                 }\n\
                 return acc;\n\
             }\n\
             fn main() {\n\
                 let n = 7i64;\n\
                 let len = 5i64;\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 let mut k = 0i64;\n\
                 while k < n * len {\n\
                     v.push(k * k + 1i64);\n\
                     k = k + 1i64;\n\
                 }\n\
                 let mut acc = 0i64;\n\
                 let mut i = 0i64;\n\
                 while i < n {\n\
                     acc = acc + row_scan(v, i * len, len);\n\
                     i = i + 1i64;\n\
                 }\n\
                 println(acc);\n\
             }",
    );
    assert_eq!(out, Some("13594\n".to_string()));
}

#[test]
fn e2e_interproc_row_helper_still_panics_on_an_undischarged_call() {
    // The mirror image: a call site that cannot discharge keeps the check,
    // so an out-of-range row still TRAPS instead of reading past the
    // buffer. Note what the trapping run proves: the good rows printed
    // their correct sum first, so the surviving check is not a blanket
    // "analysis gave up" — the same body served both sites and caught only
    // the bad one.
    const BODY: &str = "fn row_scan(v: ref Vec[i64], base: i64, len: i64) -> i64 {\n\
                 let mut lo = 0i64;\n\
                 let mut hi = len - 1i64;\n\
                 let mut acc = 0i64;\n\
                 while lo <= hi {\n\
                     acc = acc + v[base + lo] - v[base + hi];\n\
                     lo = lo + 1i64;\n\
                     hi = hi - 1i64;\n\
                 }\n\
                 return acc;\n\
             }\n\
             fn main() {\n\
                 let n = 7i64;\n\
                 let len = 5i64;\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 let mut k = 0i64;\n\
                 while k < n * len {\n\
                     v.push(k * k + 1i64);\n\
                     k = k + 1i64;\n\
                 }\n\
                 let mut acc = 0i64;\n\
                 let mut i = 0i64;\n\
                 while i < n {\n\
                     acc = acc + row_scan(v, i * len, len);\n\
                     i = i + 1i64;\n\
                 }\n\
                 println(acc);\n";
    assert_eq!(
        run_program(&format!("{BODY}}}")),
        Some("-1428\n".to_string()),
        "control: the in-range program must run clean"
    );
    // The pre-trap row still prints on stdout; the trap itself is on
    // stderr (B-2026-08-23-17), so this needs the capturing harness.
    let trapped = run_program_capturing(&format!(
        "{BODY}    println(row_scan(v, n * len - 2i64, len));\n}}"
    ))
    .expect("program should build and run");
    assert!(
        trapped.stdout.starts_with("-1428\n") && trapped.stderr.contains("vec index out of bounds"),
        "the out-of-range row must still trap — the second call site cannot \
             discharge the precondition, so the bounds check must survive; got \
             stdout={:?} stderr={:?}",
        trapped.stdout,
        trapped.stderr
    );
}

#[test]
fn test_e2e_inline_enum_field_struct_arg() {
    // #22 (phase-12 self-hosting) — the #19 fresh-temp tail. An enum-field
    // struct constructed INLINE as a call argument (`consume(W { tok: Tok.Id(..) })`,
    // no caller binding) whose callee consumes the enum internally entry-copies
    // the param; the inline temp's original payload had no caller owner and
    // leaked. Correctness here is the payloads round-tripping through the bare
    // arg (free fn + method site), an enum leaf nested one struct deeper, and a
    // direct-Vec struct arg (regression). The leak is covered by
    // `asan_inline_enum_field_struct_arg_no_leak`.
    if let Some(out) = run_program(
        r#"
enum Tok { Id(String), Int(i64) }
struct W { tok: Tok, n: i64 }
struct Inner { tok: Tok, k: i64 }
struct Outer { inner: Inner, n: i64 }
struct V { xs: Vec[i64], n: i64 }
struct Sink { total: i64 }
fn consume(w: W) -> String { match w.tok { Id(s) => s, Int(z) => z.to_string() } }
fn consume_outer(o: Outer) -> String { match o.inner.tok { Id(s) => s, Int(z) => z.to_string() } }
fn consume_vec(v: V) -> i64 { v.xs.len() }
fn mkv(n: i64) -> Vec[i64] { let mut a: Vec[i64] = Vec.new(); a.push(n); a.push(n + 1); a }
impl Sink {
    fn take(mut ref self, w: W) -> String { match w.tok { Id(s) => s, Int(z) => z.to_string() } }
}
fn main() {
    println(consume(W { tok: Tok.Id("alpha".to_string()), n: 1 }));
    let mut sk = Sink { total: 0 };
    println(sk.take(W { tok: Tok.Id("mike".to_string()), n: 2 }));
    println(consume_outer(Outer { inner: Inner { tok: Tok.Id("oscar".to_string()), k: 3 }, n: 3 }));
    println(consume_vec(V { xs: mkv(10), n: 4 }).to_string());
    println(consume(W { tok: Tok.Int(7_i64), n: 5 }));
    println("ok");
}
"#,
    ) {
        assert_eq!(out, "alpha\nmike\noscar\n2\n7\nok\n");
    }
}

#[test]
fn test_ir_vec_result_inline_struct_elem_drop() {
    // Slice 3u: `Vec[Result[Holder, i64]]` — Holder FITS Result's 5-word
    // area (inline struct payload); the element drop GEPs to w0 and runs
    // the struct drop in place (no box branch on the Ok side).
    let src = r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut v: Vec[Result[Holder, i64]] = Vec.new();
    v.push(Ok(Holder { name: "a heap string padded out beyond thirty-six bytes!", id: 1 }));
    println(v.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("karac_drop_Result_Holder_i64"),
        "expected the inline-struct-payload element drop karac_drop_Result_Holder_i64; \
             got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_generic_collection_param_distinct_elements_no_symbol_collision() {
    // The same generic forwarder instantiated at String AND Vec[i64] must
    // produce DISTINCT element-aware symbols (not one erased `$struct` body
    // with the wrong stride) — the nested-call mangle token disambiguates.
    let out = run_program(
        "fn id[T](x: T) -> T { x }\n\
             fn fwd[T](x: T) -> T { id(x) }\n\
             fn main() {\n\
             \x20   println(fwd(f\"str\"));\n\
             \x20   let mut v: Vec[i64] = Vec.new(); v.push(42);\n\
             \x20   let w = fwd(v);\n\
             \x20   println(w[0].to_string());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "str\n42");
    }
}

// ── Linker control attributes ────────────────────────────────────────────

#[test]
fn test_ir_no_mangle_symbol_name_unchanged() {
    // `#[no_mangle]` is a no-op at the codegen layer (the compiler already
    // uses the source-level name as the LLVM symbol name) but we verify
    // the function still emits with its plain name.
    let ir = ir_for("#[unsafe(no_mangle)]\nfn keep_me() -> i64 { 42 }");
    assert!(
        ir.contains("@keep_me"),
        "function symbol should appear as @keep_me; IR: {}",
        ir
    );
}

#[test]
fn test_ir_link_section_sets_function_section() {
    // `#[link_section(".init_array")]` should set the `section` directive
    // on the LLVM function definition. Inkwell's macOS `set_section`
    // encodes a Mach-O `segment,section` pair and prefixes a `,` when
    // the supplied name doesn't already contain one — so we accept
    // both `section ".init_array"` (ELF) and `section ",.init_array"`
    // (Mach-O fallback).
    let ir = ir_for("#[unsafe(link_section(\".init_array\"))]\nfn ctor() -> i64 { 1 }");
    assert!(
        ir.contains("section \".init_array\"") || ir.contains("section \",.init_array\""),
        "expected section directive on @ctor; IR: {}",
        ir
    );
}

#[test]
fn test_ir_used_multiple_symbols_share_one_global() {
    // Two `#[used]` symbols should produce a single `@llvm.used` global
    // listing both — not two separate globals.
    let ir = ir_for(
        "#[used]\nfn a() -> i64 { 1 }\n\
             #[used]\nfn b() -> i64 { 2 }\n\
             fn main() {}",
    );
    let count = ir.matches("@llvm.used").count();
    assert_eq!(
        count, 1,
        "expected exactly one @llvm.used global, found {}; IR: {}",
        count, ir
    );
}

// ── Monomorphized Map[K, V] symbols (Slice 1) ──────────────────
//
// Slice 1a wires `compile_map_method` to route `Map[i64,
// i64].len()` through a per-K/V mono symbol
// (`karac_map_i64_i64_len`) emitted with `LinkOnceODR` linkage.
// Slice 1a's wrapper body forwards 1:1 to the erased
// `karac_map_len` runtime; Slice 1b replaces hot-path bodies
// (insert_old, get) with fully inlined LLVM. These tests pin the
// emission, mangling, linkage, and dispatch wiring — the
// foundation for Slices 1b-1c.

#[test]
fn test_ir_map_i64_i64_len_uses_mono_symbol() {
    // The mono symbol must be emitted and called from the main
    // body; the erased `karac_map_len` is allowed to remain
    // declared (the mono wrapper body delegates to it in 1a) but
    // the user-facing `m.len()` site routes through mono.
    let ir = ir_for(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    println(m.len());
}
"#,
    );
    assert!(
        ir.contains("@karac_map_i64_i64_len"),
        "mono len symbol should be emitted; IR:\n{}",
        ir
    );
    assert!(
        ir.contains("call i64 @karac_map_i64_i64_len"),
        "user-facing m.len() should dispatch through mono symbol; IR:\n{}",
        ir
    );
}

#[test]
fn test_ir_map_i64_i64_insert_uses_mono_symbol() {
    // Slice 1b.2a — Map[i64, i64].insert routes through the mono
    // `karac_map_i64_i64_insert_old` symbol; the calling
    // convention is value-based (i64 key + i64 val) rather than
    // the erased pointer-based shape. The mono body forwards to
    // the erased runtime today (1b.2b adds the inline fast path).
    let ir = ir_for(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 100_i64);
}
"#,
    );
    assert!(
        ir.contains("@karac_map_i64_i64_insert_old"),
        "mono insert_old symbol should be emitted; IR:\n{}",
        ir
    );
    // Define line should carry linkonce_odr per §3.2.
    let define_line = ir
        .lines()
        .find(|l| l.contains("@karac_map_i64_i64_insert_old") && l.starts_with("define"))
        .unwrap_or_else(|| panic!("could not find define for mono insert; IR:\n{}", ir));
    assert!(
        define_line.contains("linkonce_odr"),
        "mono insert should have linkonce_odr linkage; saw: {}",
        define_line
    );
    // The user-facing m.insert(...) site routes through mono.
    assert!(
        ir.contains("call i1 @karac_map_i64_i64_insert_old"),
        "user-facing m.insert(...) should dispatch through mono symbol; IR:\n{}",
        ir
    );
}

#[test]
fn test_ir_map_i64_i64_insert_body_has_inline_probe() {
    // Slice 1b.2b — the mono insert_old body inlines the load-factor
    // check, the hash call, and the linear-probe + i64 eq loop. Pin that
    // body shape.
    //
    // B-2026-08-22-27 CHANGED WHAT THE HASH ASSERTION SAYS. This test used
    // to require a DIRECT call to `karac_hash_i64`, "rather than through
    // the runtime's function-pointer dispatch" — and that requirement was
    // the bug, pinned. Baking one hash symbol into a body shared by every
    // `Map[i64, i64]` made a non-default-hasher map file keys under one
    // hash and probe under another, silently losing a contiguous tail of
    // them from as few as 16 keys.
    //
    // The hash is now loaded from the map's control block and called
    // indirectly, exactly as the Set monos and the String-key get already
    // did. What this test is actually FOR is unchanged and still asserted
    // below: the body inlines the probe rather than delegating to the
    // erased runtime. That is where the family's win comes from; the hash
    // was never the inlined part that mattered.
    let ir = ir_for(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 100_i64);
}
"#,
    );
    // Extract the mono insert_old body.
    let mut in_body = false;
    let mut body_lines: Vec<&str> = Vec::new();
    for line in ir.lines() {
        if line.starts_with("define") && line.contains("@karac_map_i64_i64_insert_old") {
            in_body = true;
            continue;
        }
        if in_body {
            if line.starts_with('}') {
                break;
            }
            body_lines.push(line);
        }
    }
    assert!(
        !body_lines.is_empty(),
        "could not extract mono insert body; IR:\n{}",
        ir
    );
    let body = body_lines.join("\n");
    // Load-factor branch label.
    assert!(
        body.contains("fast_path") && body.contains("slow_path"),
        "mono insert should have fast/slow path basic blocks; body:\n{}",
        body
    );
    // The hash comes from the map's OWN stored `hash_fn` (offset 56),
    // called indirectly — not from a symbol baked in at emission time.
    assert!(
        body.contains("i64 56") && body.contains("hash.fn"),
        "mono insert should load the stored hash_fn from the control block; body:\n{}",
        body
    );
    assert!(
        !body.contains("call i64 @karac_hash_"),
        "mono insert must NOT call a baked hash symbol — that is B-2026-08-22-27; body:\n{}",
        body
    );
    // Probe loop: status byte load + 3-way switch on EMPTY /
    // TOMBSTONE / OCCUPIED. The presence of `load i8` for the
    // status byte and an `icmp eq i8` against the empty/
    // tombstone sentinels distinguishes the inline probe from
    // a pure delegation body.
    assert!(
        body.contains("load i8"),
        "mono insert should load the status byte inline; body:\n{}",
        body
    );
    assert!(
        body.contains("icmp eq i64") || body.contains("icmp ne i64"),
        "mono insert should inline the i64 eq check; body:\n{}",
        body
    );
}

#[test]
fn test_ir_map_i32_i64_mono_symbol_for_char_key() {
    // Slice 2.1 — `Map[char, i64]` (char lowers to LLVM i32)
    // now routes through the `karac_map_i32_i64_*` mono symbol
    // family. The same family will serve `Map[i32, i64]` if
    // anyone instantiates it — both keys mangle to `i32` and
    // share the FNV-1a-over-4-bytes hash and 4-byte slot
    // layout, so dedupe is correct. We bind the char to a
    // local first because `ExprKind::CharLit` lowers to
    // `0_i64` in `compile_expr` (pre-existing gap from Slice
    // 1b's chars work) — the for-loop-bound char is i32 as
    // expected, so we route a real i32 key through mono.
    let ir = ir_for(
        r#"
fn main() {
    let mut m: Map[char, i64] = Map.new();
    for c in "abc".chars() {
        m.insert(c, 1_i64);
    }
    println(m.len());
}
"#,
    );
    assert!(
        ir.contains("@karac_map_i32_i64_insert_old"),
        "mono insert symbol for i32 key should be emitted; IR:\n{}",
        ir
    );
    // Calling convention is value-pass with i32 key + i64 val.
    let define_line = ir
        .lines()
        .find(|l| l.contains("@karac_map_i32_i64_insert_old") && l.starts_with("define"))
        .unwrap_or_else(|| panic!("could not find define for i32 mono insert; IR:\n{}", ir));
    assert!(
        define_line.contains("linkonce_odr"),
        "i32 mono insert should have linkonce_odr linkage; saw: {}",
        define_line
    );
    assert!(
        define_line.contains("i32") && define_line.contains("i64"),
        "i32 mono insert signature should carry i32 key + i64 val types; saw: {}",
        define_line
    );
    // Extract body; hash should now go through karac_hash_i32
    // (mangle-token-named helper), not karac_hash_i64.
    let mut in_body = false;
    let mut body_lines: Vec<&str> = Vec::new();
    for line in ir.lines() {
        if line.starts_with("define") && line.contains("@karac_map_i32_i64_insert_old") {
            in_body = true;
            continue;
        }
        if in_body {
            if line.starts_with('}') {
                break;
            }
            body_lines.push(line);
        }
    }
    let body = body_lines.join("\n");
    // B-2026-08-22-27 — the hash is the map's stored `hash_fn`, not a
    // baked `karac_hash_i32`. The old assertion is worth remembering for
    // the reason it was written: `char` and `i32` keys share this one body
    // because both hashed "FNV-1a over 4 bytes ... identical output for
    // identical input". True of the default hasher and of nothing else, so
    // a `Map[char, V, H]` desynced exactly as the i64 family did.
    assert!(
        body.contains("i64 56") && body.contains("hash.fn"),
        "i32 mono insert should load the stored hash_fn from the control block; body:\n{}",
        body
    );
    assert!(
        !body.contains("call i64 @karac_hash_"),
        "i32 mono insert must NOT call a baked hash symbol (B-2026-08-22-27); body:\n{}",
        body
    );
    assert!(
        body.contains("icmp eq i32"),
        "i32 mono insert should inline icmp eq on i32; body:\n{}",
        body
    );
}

/// B-2026-09-08-11 — two DISTINCT `Vec[weak T]` types must not share one
/// drop symbol. `display_mangle_te` had no `Weak` arm, so both fell to the
/// `unknown` fallback, both emitted `karac_drop_Vec_unknown`, and because
/// the drop and clone families MEMOISE on that mangled string the second
/// type requested was handed the FIRST type's function.
///
/// It was invisible rather than harmless: every weak-slot operation is
/// slot-type-agnostic (one nullable pointer, no read of the referent's
/// layout), so the function collided with happened to be the one you
/// wanted. This pins the NAMES rather than the behaviour, because the
/// behaviour is exactly what cannot go wrong yet — a behavioural assertion
/// here would pass just as well with the bug present.
#[test]
fn test_ir_distinct_weak_referents_get_distinct_drop_symbols() {
    let ir = ir_for(
        r#"
shared struct Wa { a: i64 }
shared struct Wb { b: i64 }
fn main() {
    let x: Wa = Wa { a: 1 };
    let y: Wb = Wb { b: 2 };
    let mut ia: Vec[weak Wa] = Vec.new();
    ia.push(x);
    let mut ib: Vec[weak Wb] = Vec.new();
    ib.push(y);
    let mut oa: Vec[Vec[weak Wa]] = Vec.new();
    oa.push(ia);
    let mut ob: Vec[Vec[weak Wb]] = Vec.new();
    ob.push(ib);
    println(f"{oa.len()}{ob.len()}")
}
"#,
    );
    assert!(
        !ir.contains("Vec_unknown"),
        "a weak referent still mangles to `unknown`:\n{ir}"
    );
    let syms: Vec<&str> = ir
        .lines()
        .filter(|l| l.contains("define") && (l.contains("drop") || l.contains("clone")))
        .collect();
    assert!(
        ir.contains("weak_Wa") && ir.contains("weak_Wb"),
        "the two weak referents did not both get named symbols. drop/clone defines:\n{}",
        syms.join("\n")
    );
}

#[test]
fn test_ir_map_i64_i64_get_uses_mono_symbol_with_inline_probe() {
    // Slice 1b.3 — Map[i64, i64].get routes through the mono
    // `karac_map_i64_i64_get` symbol with the same inline-probe shape as
    // insert_old: inline status `load i8`, inline `icmp eq i64`. Get has
    // no load-factor branch (never resizes) and no tombstone-tracking PHI
    // — simpler than insert_old but the same hot-path pattern.
    //
    // B-2026-08-22-27 — the hash is loaded from the map's control block
    // and called indirectly, matching insert_old. The two MUST agree: a
    // get that probed with a different hash than insert filed under would
    // miss every key, which is half of what that row measured.
    let ir = ir_for(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 100_i64);
    match m.get(1_i64) {
        Some(v) => println(v),
        None => println(0_i64),
    }
}
"#,
    );
    assert!(
        ir.contains("@karac_map_i64_i64_get"),
        "mono get symbol should be emitted; IR:\n{}",
        ir
    );
    let define_line = ir
        .lines()
        .find(|l| l.contains("@karac_map_i64_i64_get(") && l.starts_with("define"))
        .unwrap_or_else(|| panic!("could not find define for mono get; IR:\n{}", ir));
    assert!(
        define_line.contains("linkonce_odr"),
        "mono get should have linkonce_odr linkage; saw: {}",
        define_line
    );
    assert!(
        ir.contains("call i1 @karac_map_i64_i64_get"),
        "user-facing m.get(...) should dispatch through mono symbol; IR:\n{}",
        ir
    );
    // Extract the mono get body and pin the inline-probe shape.
    let mut in_body = false;
    let mut body_lines: Vec<&str> = Vec::new();
    for line in ir.lines() {
        if line.starts_with("define") && line.contains("@karac_map_i64_i64_get(") {
            in_body = true;
            continue;
        }
        if in_body {
            if line.starts_with('}') {
                break;
            }
            body_lines.push(line);
        }
    }
    let body = body_lines.join("\n");
    assert!(
        body.contains("i64 56") && body.contains("hash.fn"),
        "mono get should load the stored hash_fn from the control block; body:\n{}",
        body
    );
    assert!(
        !body.contains("call i64 @karac_hash_"),
        "mono get must NOT call a baked hash symbol (B-2026-08-22-27); body:\n{}",
        body
    );
    assert!(
        body.contains("load i8"),
        "mono get should load status byte inline; body:\n{}",
        body
    );
    assert!(
        body.contains("icmp eq i64"),
        "mono get should inline the i64 eq check; body:\n{}",
        body
    );
}

/// Three globals must always emit, regardless of whether the
/// program contains any `par {}` blocks. Slice 5's runtime API
/// reads through these symbols unconditionally and degrades
/// cleanly when the table is empty.
#[test]
fn test_spawn_site_metadata_emitted_for_par_blocks() {
    // Serialize against the env-var test below — see module
    // comment for rationale.
    // Two par blocks: first has 2 branches, second has 3. The
    // metadata table should pin both with their `worker_count`
    // values (2 and 3) and assign IDs 0 and 1 (matching the
    // `par_counter` start).
    let ir = ir_for_with_source(
        r#"
fn a() { println(1); }
fn b() { println(2); }
fn c() { println(3); }
fn main() {
    par {
        a();
        b();
    }
    par {
        a();
        b();
        c();
    }
}
"#,
    );

    // Length global: two entries.
    assert!(
        ir.contains("@KARAC_SPAWN_SITES_LEN"),
        "missing KARAC_SPAWN_SITES_LEN global; ir:\n{ir}"
    );
    assert!(
        ir.contains("@KARAC_SPAWN_SITES_LEN = constant i32 2")
            || ir.contains("@KARAC_SPAWN_SITES_LEN = constant i32 2,")
            || ir.contains("@KARAC_SPAWN_SITES_LEN = constant i32 2\n"),
        "expected KARAC_SPAWN_SITES_LEN = 2; ir:\n{ir}"
    );

    // Enabled global: true (i1 1).
    assert!(
        ir.contains("@KARAC_SPAWN_SITES_ENABLED = constant i1 true"),
        "expected KARAC_SPAWN_SITES_ENABLED = true; ir:\n{ir}"
    );

    // Array global: two entries.
    assert!(
        ir.contains("@KARAC_SPAWN_SITES = constant"),
        "missing KARAC_SPAWN_SITES global; ir:\n{ir}"
    );
    // The array type prefix should reflect the entry count.
    assert!(
        ir.contains("@KARAC_SPAWN_SITES = constant [2 x"),
        "expected `[2 x …]` array type for KARAC_SPAWN_SITES; ir:\n{ir}"
    );

    // Worker counts: 2 and 3 should both appear in the array
    // initializer. We can't easily isolate just the array text
    // from the IR string, but the combination of `[2 x` plus
    // both i32 values 2 and 3 is a strong signal.
    // Sanity-check: at least one occurrence of `i32 2,` and
    // `i32 3,` in the array initializer (the entry struct fields).
    assert!(
        ir.contains("i32 2"),
        "expected i32 2 worker_count; ir:\n{ir}"
    );
    assert!(
        ir.contains("i32 3"),
        "expected i32 3 worker_count; ir:\n{ir}"
    );
}

/// Empty array must still emit (length zero, enabled true) — the
/// runtime API reads through these symbols even on programs with
/// no `par {}` blocks.
#[test]
fn test_spawn_site_metadata_empty_when_no_par_blocks() {
    let ir = ir_for_with_source(
        r#"
fn main() {
    println(42);
}
"#,
    );

    assert!(
        ir.contains("@KARAC_SPAWN_SITES_LEN"),
        "missing KARAC_SPAWN_SITES_LEN global; ir:\n{ir}"
    );
    // Length zero.
    assert!(
        ir.contains("@KARAC_SPAWN_SITES_LEN = constant i32 0"),
        "expected KARAC_SPAWN_SITES_LEN = 0; ir:\n{ir}"
    );
    // Enabled true (dev default).
    assert!(
        ir.contains("@KARAC_SPAWN_SITES_ENABLED = constant i1 true"),
        "expected KARAC_SPAWN_SITES_ENABLED = true; ir:\n{ir}"
    );
    // Empty array.
    assert!(
        ir.contains("@KARAC_SPAWN_SITES = constant [0 x"),
        "expected empty `[0 x …]` KARAC_SPAWN_SITES; ir:\n{ir}"
    );
}

/// The runtime-debug-metadata gate off (`KARAC_RUNTIME_DEBUG_METADATA=0`,
/// or the per-thread pin used here) — all three globals still emit, but
/// `LEN = 0`, `ENABLED = false`, and the array is empty regardless of how
/// many `par {}` blocks the program contains.
///
/// Test isolation: the gate is read once at `Codegen::new` time, so it
/// is pinned for THIS THREAD only. The earlier spelling set and unset
/// `KARAC_RUNTIME_DEBUG_METADATA` on the process env, reasoning that "the
/// var name is unique to this test, so there is no collision risk with
/// peers" — but the var is read by every `Codegen::new` in the process,
/// so the risk was with every concurrently-compiling test rather than
/// with peers naming the same var (B-2026-08-20-26).
#[test]
fn test_spawn_site_metadata_disabled_when_gate_pinned_off() {
    // Acquire the shared lock so peer spawn-site tests don't
    // observe the var while the gate is flipped to "0".
    // Restore prior state on completion. Establishing the prior
    // value before the test is paranoid but cheap — most CI runs
    // start with the var unset.
    // B-2026-08-20-26: pinned per-thread rather than by setting the
    // process env, which every other concurrently-compiling test in this
    // binary would have seen.
    let _pin = karac::codegen::pin_runtime_debug_metadata(false);
    let ir = ir_for_with_source(
        r#"
fn a() { println(1); }
fn b() { println(2); }
fn main() {
    par {
        a();
        b();
    }
}
"#,
    );

    // Length zero, even though the program has one par block.
    assert!(
        ir.contains("@KARAC_SPAWN_SITES_LEN = constant i32 0"),
        "expected KARAC_SPAWN_SITES_LEN = 0 when gate off; ir:\n{ir}"
    );
    // Enabled false.
    assert!(
        ir.contains("@KARAC_SPAWN_SITES_ENABLED = constant i1 false"),
        "expected KARAC_SPAWN_SITES_ENABLED = false when gate off; ir:\n{ir}"
    );
    // Empty array when gate off.
    assert!(
        ir.contains("@KARAC_SPAWN_SITES = constant [0 x"),
        "expected empty `[0 x …]` KARAC_SPAWN_SITES when gate off; ir:\n{ir}"
    );
}

// ── Debugger Contract: std.runtime APIs (slice 5) ──
//
// Item (4) of the four-piece Debugger Contract. Three Kāra-callable
// functions exposed via the empty-marker `Runtime` struct in baked
// stdlib (`runtime/stdlib/runtime.kara`):
//
//   - `Runtime.has_debug_metadata() -> bool` — reads
//     `KARAC_SPAWN_SITES_ENABLED` (slice 3 global).
//   - `Runtime.list_par_blocks() -> Vec[ParBlockInfo]` — joins slice 4's
//     `ACTIVE_FRAMES` registry against slice 3's `KARAC_SPAWN_SITES`.
//   - `Runtime.list_tasks() -> Vec[TaskInfo]` — always empty in v1.
//
// These pin the runtime-debug-metadata gate per-thread, like slice 3's
// tests above; neither touches the process env (B-2026-08-20-26).

/// `has_debug_metadata()` returns `true` under the dev default
/// (env var unset). Validates that slice 3's `KARAC_SPAWN_SITES_ENABLED = 1`
/// flows through the runtime fn into the boolean returned to Kāra.
#[test]
fn test_has_debug_metadata_returns_true_when_gate_on() {
    // Make sure the var is unset so the dev default applies.
    // B-2026-08-20-26: pinned per-thread rather than by unsetting the
    // process env, which every other concurrently-compiling test in this
    // binary would have seen.
    let _pin = karac::codegen::pin_runtime_debug_metadata(true);
    let captured = run_program_capturing(
        r#"
fn main() {
    let dbg = Runtime.has_debug_metadata();
    if dbg {
        println(1);
    } else {
        println(0);
    }
}
"#,
    );
    if let Some(c) = captured {
        assert_eq!(c.stdout.trim(), "1", "expected gate-on (true → 1)");
    }
}

/// `has_debug_metadata()` returns `false` when codegen runs with
/// `KARAC_RUNTIME_DEBUG_METADATA=0`. Pinpoints the slice 3 gate-off
/// emission of `KARAC_SPAWN_SITES_ENABLED = 0` flowing through the
/// runtime fn.
#[test]
fn test_has_debug_metadata_returns_false_when_gate_off() {
    // B-2026-08-20-26: pinned per-thread rather than by setting the
    // process env, which every other concurrently-compiling test in this
    // binary would have seen.
    let _pin = karac::codegen::pin_runtime_debug_metadata(false);
    let captured = run_program_capturing(
        r#"
fn main() {
    let dbg = Runtime.has_debug_metadata();
    if dbg {
        println(1);
    } else {
        println(0);
    }
}
"#,
    );
    if let Some(c) = captured {
        assert_eq!(c.stdout.trim(), "0", "expected gate-off (false → 0)");
    }
}

// ── Phase 6 line 26 slice 8s: typed-aware arm-local let slot lowering ─
//
// `BodySplitStmt::Let` emission now derives the slot type from the
// materialised value (`value.get_type()`) rather than hardcoding i64.
// Fixes the latent miscompile where `let v = items` with `items: Vec[i64]`
// would alloca an 8-byte i64 slot and store 24 bytes of Vec data into
// it. Captured-local slots (slice 8a) already carry their state-struct
// field type and are unaffected — slice 8s only touches the arm-local
// let emission arm.

#[test]
fn test_body_splitting_8s_let_vec_captured_alloca_uses_inline_vec_type() {
    // `let v = items` where `items: Vec[i64]` is a captured local —
    // slice 8s makes the `%v.slot` alloca match the inline Vec layout
    // `{ ptr, i64, i64 }`, not i64. Same width as the loaded value.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(items: Vec[i64]) with sends(Network) receives(Network) {
                 fetch();
                 let v = items;
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // `v.slot` alloca must use the inline Vec shape, not i64.
    assert!(
        body.contains("%v.slot = alloca { ptr, i64, i64 }")
            || body.contains("%v.slot = alloca {ptr, i64, i64}"),
        "let v = items must alloca a Vec-shaped slot, not i64:\n{body}"
    );
    // The store and the .let_rhs load must also be Vec-typed.
    assert!(
        body.contains("%items.let_rhs = load { ptr, i64, i64 }, ptr %items.slot")
            || body.contains("%items.let_rhs = load {ptr, i64, i64}, ptr %items.slot"),
        "let RHS load must read the inline Vec layout from items.slot:\n{body}"
    );
    assert!(
        body.contains("store { ptr, i64, i64 } %items.let_rhs, ptr %v.slot")
            || body.contains("store {ptr, i64, i64} %items.let_rhs, ptr %v.slot"),
        "let RHS store must write the inline Vec layout into v.slot:\n{body}"
    );
    // Regression guard: no i64 alloca for `v.slot`.
    assert!(
        !body.contains("%v.slot = alloca i64"),
        "v.slot must NOT be an i64 alloca (slice 8s widening regression):\n{body}"
    );
}

#[test]
fn test_per_mono_poll_fn_constructor_destructor_emitted_at_mangled_key() {
    // All three callable helpers (poll-fn + constructor;
    // destructor only when heap-bearing — this mono has only
    // `i64`, no destructor) emit at the mangled key.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); }
             fn caller() { driver(42i64); }",
    );
    assert!(
        ir.contains("@\"__kara_poll_driver$i64\""),
        "per-mono poll-fn must emit at mangled key:\n{ir}"
    );
    assert!(
        ir.contains("@\"__kara_state_new_driver$i64\""),
        "per-mono constructor must emit at mangled key:\n{ir}"
    );
    // Destructor for i64-only captures: not emitted (skip-when-empty rule).
    assert!(
        !ir.contains("@\"__kara_state_drop_driver$i64\""),
        "per-mono destructor must skip for primitive-only captures:\n{ir}"
    );
}

#[test]
fn test_e2e_inline_map_get_unwrap_heap_value() {
    // B-2026-07-15-26: an inline `map.get(k).unwrap()` of a heap value
    // (String/Vec) used to double-free (SIGABRT) when consumed directly — the
    // borrowed bucket buffer was freed both as the inline temporary and by the
    // map's scope-exit drop. Now the unwrap zeroes the borrow view's `cap`, so
    // the inline consumer's free-guard skips it. This output test guards the
    // observable result across a println arg, a method receiver (`.len()`), a
    // fn-call arg, and a bound read; the sibling LSan test guards the memory
    // safety.
    let output = run_program(
        "fn takes(s: String) -> i64 { s.len() }\n\
             fn main() {\n\
                 let mut m: Map[i64, String] = Map.new();\n\
                 m.insert(1, \"hello\");\n\
                 m.insert(2, \"worldish\");\n\
                 println(m.get(1).unwrap());\n\
                 println(m.get(2).unwrap().len());\n\
                 println(takes(m.get(1).unwrap()));\n\
                 let v = m.get(2).unwrap();\n\
                 println(v);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "hello\n8\n5\nworldish\n");
}

#[test]
fn test_e2e_inline_index_map_get_unwrap_vec_value() {
    // B-2026-07-15-27: inline-indexing a `map.get(k).unwrap()` Vec value
    // (`m.get(k).unwrap()[i]`) used to loud-bail "Index operator applied to
    // non-array type" — codegen recognised only named-binding / free-fn-call
    // Vec receivers, not a `.get().unwrap()` method chain. Now the borrow
    // view is materialized into a synth Vec local and indexed; heap elements
    // are deep-cloned so the read stands alone, and the temp is NOT dropped
    // (the map owns the buffer, cf. B-2026-07-15-26). Covers a scalar-elem
    // value (`Vec[i64]`, the reported repro), a heap-elem value
    // (`Vec[String]`) across a println arg + a bound read, a `[i].method()`
    // indexed-receiver call, and a nested `Vec[Vec[i64]]`. Sibling LSan test
    // guards the memory safety.
    let output = run_program(
        "fn main() {\n\
                 let mut m: Map[i64, Vec[i64]] = Map.new();\n\
                 m.insert(1, [10, 20, 30]);\n\
                 println(m.get(1).unwrap()[1]);\n\
                 let mut s: Map[i64, Vec[String]] = Map.new();\n\
                 s.insert(1, [\"alpha\", \"beta\", \"gamma\"]);\n\
                 println(s.get(1).unwrap()[0]);\n\
                 println(s.get(1).unwrap()[2].len());\n\
                 let w = s.get(1).unwrap()[1].clone();\n\
                 println(w);\n\
                 let mut n: Map[i64, Vec[Vec[i64]]] = Map.new();\n\
                 n.insert(7, [[1, 2], [3, 4, 5]]);\n\
                 let inner = n.get(7).unwrap()[1].clone();\n\
                 println(inner[2]);\n\
                 println(m.get(1).unwrap()[0]);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "20\nalpha\n5\nbeta\n5\n10\n");
}

/// B-2026-07-30-2: an inline non-capturing comparator must monomorphize
/// at EVERY length. Before the fix a `len > 64` check sent larger sorts
/// to `karac_vec_sort_by`, whose comparator is a function pointer — an
/// indirect call per probe that cannot be inlined into the merge. That
/// is structurally what C's `qsort` does, and it measured that way:
/// kata #1665 at parity with C (17.9 vs 18.4 ms) and ~2x behind Rust
/// (9.1 ms), which monomorphizes the comparator into the sort.
///
/// Asserts on the IR rather than on timing: for an N well past the old
/// threshold, no call to the runtime helper survives.
#[test]
fn large_n_inline_comparator_emits_no_runtime_sort_call() {
    let src = r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    let mut i: i64 = 0;
    while i < 5000 {
        v.push((i * 37 + 11) % 5000);
        i = i + 1;
    }
    v.sort_by(|a, b| a.cmp(b));
    println(v[0]);
}
"#;
    let mut parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    let ir =
        karac::codegen::compile_to_ir(&parsed.program, Some(&ownership), None).expect("codegen");
    let runtime_sort_calls = ir
        .lines()
        .filter(|l| l.contains("@karac_vec_sort_by(") && l.contains(" call "))
        .count();
    assert_eq!(
        runtime_sort_calls, 0,
        "an inline non-capturing comparator must not reach the \
             function-pointer runtime sort at any N"
    );
}

#[test]
fn test_e2e_column_fold_rejects_noninline_and_heap_accumulator() {
    // First-cut boundaries the native backend rejects LOUDLY (each works
    // under `karac run`): a closure-valued local (the inline-body strategy
    // needs the literal at the call site) and a heap / aggregate
    // accumulator (`String`, whose per-iteration replacement would need
    // drop plumbing). Loud rejection, never a silent miscompile.
    let non_inline = r#"
fn main() {
    let c: Column[i64] = Column.from_vec([1, 2, 3]);
    let g = |a: i64, x: i64| a + x;
    println(f"{c.fold(0, g)}");
}
"#;
    let err = ir_result(non_inline).expect_err("a non-inline closure must be rejected");
    assert!(err.contains("inline closure literal"), "got: {err}");

    let heap_acc = r#"
fn main() {
    let c: Column[i64] = Column.from_vec([1, 2, 3]);
    let s = c.fold("", |acc, x| acc + "!");
    println(f"{s}");
}
"#;
    let err = ir_result(heap_acc).expect_err("a heap accumulator must be rejected");
    assert!(err.contains("heap / aggregate accumulator"), "got: {err}");
}

#[test]
fn test_e2e_column_map_rejects_noninline_and_string() {
    // Same first-cut boundaries as `Column.fold` (each works under `karac
    // run`): a closure-valued local and a heap-element column.
    let non_inline = r#"
fn main() {
    let c: Column[i64] = Column.from_vec([1, 2, 3]);
    let g = |x: i64| x * 2;
    let d = c.map(g);
    println(f"{d.sum()}");
}
"#;
    let err = ir_result(non_inline).expect_err("a non-inline closure must be rejected");
    assert!(err.contains("inline closure literal"), "got: {err}");

    let string_elem = r#"
fn main() {
    let v: Vec[String] = ["a", "b"];
    let c: Column[String] = Column.from_vec(v);
    let d = c.map(|x| x);
    println(f"{d.len()}");
}
"#;
    let err = ir_result(string_elem).expect_err("a String-element map must be rejected");
    assert!(err.contains("Column[String].map"), "got: {err}");
}

#[test]
fn test_e2e_tensor_fold_rejects_noninline_and_heap_accumulator() {
    // Same first-cut boundaries as `Column.fold`, rejected LOUDLY (each
    // works under `karac run`): a closure-valued local and a heap /
    // aggregate accumulator.
    let non_inline = r#"
fn main() {
    let t: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);
    let g = |a: i64, x: i64| a + x;
    println(f"{t.fold(0, g)}");
}
"#;
    let err = ir_result(non_inline).expect_err("a non-inline closure must be rejected");
    assert!(err.contains("inline closure literal"), "got: {err}");

    let heap_acc = r#"
fn main() {
    let t: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);
    let s = t.fold("", |acc, x| acc + "!");
    println(f"{s}");
}
"#;
    let err = ir_result(heap_acc).expect_err("a heap accumulator must be rejected");
    assert!(err.contains("heap / aggregate accumulator"), "got: {err}");
}

#[test]
fn host_fn_native_call_links_against_host_symbol_e2e() {
    // `labs` is libc's i64 absolute value — the linker's default
    // libc/libSystem provides the symbol, making this a true
    // end-to-end "host provides the body" round trip: declare via
    // `host fn`, call, link, run, observe the host's answer.
    let out = run_program(
        "host fn labs(x: i64) -> i64 with reads(Env);\n\
             fn main() {\n\
                 println(f\"{labs(0 - 42)}\");\n\
                 println(f\"{labs(7)}\");\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out, "42\n7\n", "host-provided labs must answer");
    }
}

// ── Inline index of a method/function-returned `Vec` ─────────────
// `expr[i]` where `expr` is a non-place expression producing a `Vec`
// (`a.shape()[k]`, `make()[i]`) now lowers: the arbitrary-Vec arm in
// `compile_index` materializes the value into a synth Vec local,
// recurses so the identifier Vec path reads the element, then drops
// the temp Vec (buffer + nested element heap) after the read —
// deep-cloning the element first when it isn't trivially Copy. Before
// this the Vec struct fell to the generic tail and died with "Index
// operator applied to non-array type". (phase-11-stdlib-longtail.md)

#[test]
fn test_e2e_inline_index_tensor_shape() {
    // The motivating case: read a tensor dim inline via `shape()[k]`.
    // `shape()` returns `Vec[i64]` (trivially Copy element → no clone,
    // just free the temp buffer after the read).
    let out = run_program(
        "fn main() {\n\
                 let a: Tensor[f64, [2, 3]] = Tensor.zeros([2, 3]);\n\
                 println(a.shape()[0]);\n\
                 println(a.shape()[1]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "2\n3\n",
            "inline shape()[k] must read dims (codegen == karac run)"
        );
    }
}

#[test]
fn test_e2e_inline_index_fn_returned_vec_scalar() {
    // General (non-tensor) case: a free fn returning `Vec[i64]`,
    // indexed inline. Hits non-generic code, scalar element.
    let out = run_program(
        "fn make() -> Vec[i64] {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(10); v.push(20); v.push(30);\n\
                 v\n\
             }\n\
             fn main() {\n\
                 println(make()[0]);\n\
                 println(make()[2]);\n\
                 println(make()[0] + make()[1]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "10\n30\n30\n",
            "inline make()[i] must read (codegen == karac run)"
        );
    }
}

#[test]
fn test_e2e_inline_index_fn_returned_vec_string() {
    // Non-Copy element: a fn returning `Vec[String]`, indexed inline.
    // The read shallow-aliases the buffer; the element must be
    // deep-cloned before the temp Vec's nested heap is freed, so the
    // printed value is intact (no use-after-free) and the buffer's
    // other elements free cleanly.
    let out = run_program(
        "fn names() -> Vec[String] {\n\
                 let mut v: Vec[String] = Vec.new();\n\
                 v.push(\"alice\"); v.push(\"bob\"); v.push(\"carol\");\n\
                 v\n\
             }\n\
             fn main() {\n\
                 println(names()[0]);\n\
                 println(names()[2]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "alice\ncarol\n",
            "inline names()[i] must deep-clone the String element (codegen == karac run)"
        );
    }
}

#[test]
fn test_inline_index_fn_returned_vec_ir_lowers() {
    // The inline index compiles (no "Index operator applied to
    // non-array type") — the synth Vec materialization + drop fn.
    let ir = ir_for(
        "fn make() -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(7); v }\n\
             fn main() { println(make()[0]); }\n",
    );
    assert!(
        ir.contains("define") && !ir.is_empty(),
        "the inline-Vec-index program must compile to IR"
    );
}

// ── Inline index of a METHOD-returned `Vec` (B-2026-08-14-38) ────
// The sibling of the block above, for the receiver shape it never
// covered: `inline_temp_vec_te` resolves a Vec temporary's element type
// from the CALLEE'S SIGNATURE, which exists for a free fn and for the one
// built-in `tensor.shape()` — and for nothing else. So every other
// Vec-returning method (`v.clone()`, `s.to_vec()`, a user accessor) fell
// to the generic tail and failed the build with "Index operator applied to
// non-array type" while `--interp` ran it. The typechecker now records the
// receiver's own `Vec[T]` under its span; the materialization is the same.

#[test]
fn test_e2e_inline_index_method_returned_vec() {
    // Both spellings the bug reported, plus the two the fix has to get
    // right for its own reasons: a USER method (nothing built-in resolves
    // it) and a GENERIC body (the recorded element is the body's `T`, so
    // the monomorph substitution has to run before it sizes the load —
    // one instantiation per element type here proves it does).
    let out = run_program(
        "struct Bag { items: Vec[i64] }\n\
             impl Bag {\n\
                 pub fn copy_items(ref self) -> Vec[i64] { return self.items.clone(); }\n\
             }\n\
             fn pick[T](v: Vec[T], i: i64) -> T { return v.clone()[i]; }\n\
             fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3];\n\
                 println(f\"{v.clone()[1]}\");\n\
                 let nums: Vec[i64] = [10, 20, 30, 40];\n\
                 println(f\"{nums[1..3].to_vec()[0]}\");\n\
                 let b = Bag { items: [7, 8] };\n\
                 println(f\"{b.copy_items()[1]}\");\n\
                 let names: Vec[String] = [\"ann\", \"bo\", \"cy\"];\n\
                 println(names.clone()[2]);\n\
                 println(f\"{pick(v, 0)}\");\n\
                 println(pick(names, 1));\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "2\n20\n8\ncy\n1\nbo\n",
            "inline <method>()[i] must read the element (codegen == karac run)"
        );
    }
}

#[test]
fn test_inline_index_method_returned_vec_takes_the_temp_path() {
    // The E2E above soft-skips without the runtime archives, and the
    // generic tail's failure is a compile ERROR — so pin structurally that
    // the read goes through the materialize-a-synth-Vec path (its alloca
    // is named `inline.vec.tmp`) rather than merely "compiles".
    for src in [
        "fn main() { let v: Vec[i64] = [1, 2, 3]; println(f\"{v.clone()[1]}\"); }\n",
        "fn main() { let n: Vec[i64] = [10, 20, 30]; println(f\"{n[0..2].to_vec()[1]}\"); }\n",
    ] {
        let ir = ir_for(src);
        assert!(
            ir.contains("inline.vec.tmp"),
            "the method-returned Vec must be materialized, not left to the generic tail: {src}"
        );
    }
}

#[test]
fn test_e2e_chained_access_through_inline_shared_field() {
    // B-2026-06-14-28 — a plain struct with an INLINE field whose type is
    // a `shared` struct/enum (an 8-byte RC pointer). Chained field access
    // through that shared field (`h.a.v`) must LOAD the RC pointer and GEP
    // into the heap payload. Pre-fix, `compile_field_access`'s shared
    // branch only fired for an Identifier/`self` object — a `FieldAccess`
    // object (the intermediate `h.a`) fell to the generic struct-value
    // path, where `compile_expr(h.a)` yields the extracted RC pointer, the
    // `StructValue` guard misses, and the access returned the const-0
    // placeholder: SILENT wrong value (interp gave 5, AOT gave 0). The fix
    // makes `shared_type_for_expr` resolve a `FieldAccess` object via
    // `type_name_of_expr` + `shared_types`. This is the exact shape the
    // AST-port operand wrappers use (`struct BinOp { left: Expr }` read as
    // `b.left.something`). Verifies the chained read, sibling i64 fields
    // around it, and the binding-out form (`let x = h.a; x.v`) all agree.
    if let Some(out) = run_program(
        "shared struct Leaf { v: i64 }\n\
             struct Holder { x: i64, a: Leaf, y: i64 }\n\
             fn main() {\n\
                 let h = Holder { x: 7, a: Leaf { v: 5 }, y: 9 };\n\
                 // `h.a.v` read as a VALUE (not a method receiver — chained\n\
                 // field-receiver method calls are a separate deferred item).\n\
                 let chained: i64 = h.a.v;       // chained through shared field\n\
                 let bound = h.a;\n\
                 let via_bind: i64 = bound.v;    // binding-out form\n\
                 println(h.x);                   // 7  (sibling i64 before)\n\
                 println(h.y);                   // 9  (sibling i64 after)\n\
                 println(chained);               // 5  (the load-through-RC fix)\n\
                 println(via_bind);              // 5  (already worked)\n\
                 println(chained + via_bind);    // 10 (both reads sound)\n\
             }",
    ) {
        assert_eq!(out.trim(), "7\n9\n5\n5\n10");
    }
}

// ── Codegen hint attributes (#[inline] / #[cold]) — IR ─────────
//
// design.md § Codegen Hint Attributes. The inline axis lowers to the
// LLVM `inlinehint` / `alwaysinline` / `noinline` function attribute,
// and `#[cold]` to `cold`. These are advisory; the tests pin only
// that the attribute reaches the IR, and that the blocked-inlining
// shapes (recursive / fn-pointer) still compile cleanly.

#[test]
fn test_ir_inline_always_emits_alwaysinline() {
    let ir = ir_for("#[inline(always)]\nfn helper(a: i64) -> i64 { a + 1 }");
    assert!(
        ir.contains("alwaysinline"),
        "expected `alwaysinline` attribute in IR:\n{ir}"
    );
}

#[test]
fn test_ir_inline_never_emits_noinline() {
    let ir = ir_for("#[inline(never)]\nfn helper(a: i64) -> i64 { a + 1 }");
    assert!(
        ir.contains("noinline"),
        "expected `noinline` attribute in IR:\n{ir}"
    );
}

#[test]
fn test_ir_inline_emits_inlinehint() {
    let ir = ir_for("#[inline]\nfn helper(a: i64) -> i64 { a + 1 }");
    assert!(
        ir.contains("inlinehint"),
        "expected `inlinehint` attribute in IR:\n{ir}"
    );
}

#[test]
fn test_ir_recursive_inline_always_compiles() {
    // `#[inline(always)]` on a recursive function must still compile —
    // the recursive call site is simply left out of line.
    let ir = ir_for(
        "#[inline(always)]\n\
             fn fact(n: i64) -> i64 { if n <= 1 { 1 } else { n * fact(n - 1) } }",
    );
    assert!(ir.contains("@fact") && ir.contains("alwaysinline"));
}

// The checklist's "`#[inline(always)]` used through a function pointer
// still compiles" case — unblocked once the bare-named-fn → `Fn`-value
// codegen gap (B-2026-06-20-1) landed; the indirect-call variant is the
// test below. Kāra's function type is the uppercase `Fn(...)` (design.md §
// First-Class Functions, syntax.md § 6.3); a bare named fn passed to a
// `Fn(...)` parameter now reifies into the closure fat-pointer ABI, so the
// higher-order call compiles and the attribute still rides on the function
// definition regardless of call shape.
#[test]
fn test_ir_inline_always_through_fn_value_compiles() {
    let ir = ir_for(
        "#[inline(always)]\n\
             fn doubler(n: i64) -> i64 { n * 2i64 }\n\
             fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
             fn main() { let r = apply(doubler, 21i64); println(f\"{r}\"); }",
    );
    // The attribute is emitted on the definition regardless of call shape,
    // and the indirect higher-order call now compiles (no verifier error).
    assert!(
        ir.contains("@doubler") && ir.contains("alwaysinline"),
        "expected alwaysinline on the fn-value target:\n{ir}"
    );
    assert!(
        ir.contains("@__karac_fnval_doubler"),
        "expected the reified fn-value trampoline:\n{ir}"
    );
}

#[test]
fn test_ir_cold_and_inline_never_coexist() {
    let ir = ir_for("#[cold]\n#[inline(never)]\nfn rare(a: i64) -> i64 { a - 1 }");
    assert!(ir.contains("noinline"), "expected noinline:\n{ir}");
    assert!(ir.contains("cold"), "expected cold:\n{ir}");
}

#[test]
fn test_ir_trait_inline_hint_propagates_to_impl() {
    // A `#[inline(always)]` on a trait method declaration reaches the
    // non-overriding impl method's IR (propagation runs in desugar).
    let ir = ir_for_desugared(
        "trait Doubler { #[inline(always)] fn go(ref self, x: i64) -> i64; }\n\
             struct P { k: i64 }\n\
             impl Doubler for P { fn go(ref self, x: i64) -> i64 { x * self.k } }",
    );
    assert!(
        ir.contains("alwaysinline"),
        "trait inline hint should reach impl method IR:\n{ir}"
    );
}

#[test]
fn test_e2e_inline_option_payload_field_body_fires_once() {
    // B-2026-08-03-10 — an `Option` FIELD whose payload is a Drop-bearing
    // struct with NO heap fired the body TWICE at binding death under AOT
    // (three times when the owner was also passed by value), against one
    // fire under `karac run`. vg=0 throughout — the payload owns no heap —
    // so it is a pure body-COUNT bug no sanitizer can see.
    //
    // `emit_drop_fn_for_type_expr` has a guard that routes a Drop-bearing
    // user struct to the MEMORY-ONLY synthesis, precisely so the memory
    // channel never runs a user body. Its fallback for a struct with
    // nothing to free called `emit_primitive_drop_fn(type_name)`, which
    // opens with the same `karac_drop_<T>` module lookup the guard exists to
    // bypass — and that name IS the user-drop wrapper. The fallback now uses
    // a `$mem` suffix that cannot collide.
    //
    // The controls pin why only this shape broke: a BOXED payload (`Big`, 4
    // words) reaches a different branch, the `Result` sibling registers no
    // memory drop for a struct payload at all, and a direct binding never
    // goes through the field channel.
    let out = run_program(
        r#"
struct Small { id: i64 }
impl Drop for Small { fn drop(mut ref self) { println(f"drop {self.id}") } }
struct Big { id: i64, name: String }
impl Drop for Big { fn drop(mut ref self) { println(f"drop {self.id} {self.name}") } }
struct Hsm { o: Option[Small], t: i64 }
struct Hb { o: Option[Big], t: i64 }
struct Hr { r: Result[Small, i64], t: i64 }
fn take(h: Hsm) -> i64 { h.t }
fn main() {
    println("inline-field:");
    { let h = Hsm { o: Option.Some(Small { id: 1 }), t: 10 }; println(h.t); }
    println("inline-field-byvalue:");
    { let h = Hsm { o: Option.Some(Small { id: 2 }), t: 20 }; println(take(h)); }
    println("boxed-field:");
    { let h = Hb { o: Option.Some(Big { id: 3, name: f"c{3}" }), t: 30 }; println(h.t); }
    println("result-sibling:");
    { let h = Hr { r: Result.Ok(Small { id: 4 }), t: 40 }; println(h.t); }
    println("direct-binding:");
    { let o = Option.Some(Small { id: 5 }); println(50); }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "inline-field:\n10\ndrop 1\n\
                 inline-field-byvalue:\n20\ndrop 2\n\
                 boxed-field:\n30\ndrop 3 c3\n\
                 result-sibling:\n40\ndrop 4\n\
                 direct-binding:\ndrop 5\n50\nend"
        );
    }
}

/// B-2026-08-15-6 — every consumer of an inline-temp-`Vec` index reads the
/// same element, whichever spelling it is written in.
///
/// The bug was a leak, so this is a regression guard rather than the
/// detector (it passes on the parent; `asan_fstring_inline_temp_vec_index_-
/// element_no_leak` is what fails there). What it pins is the direction the
/// fix could go wrong: both halves now hand a clone to a scope-exit cleanup,
/// and freeing a buffer the reader still needs — or one a CONTAINER owns —
/// shows up here as a wrong element rather than as a leak. The `b.xs` /
/// `grid[1]` / `held[1]` lines are place reads that must keep printing their
/// container's live contents after the same container has been read through
/// a temporary.
#[test]
fn test_e2e_inline_temp_vec_index_spellings_agree() {
    assert_eq!(
            run_program(
                "struct B { xs: Vec[i64] }\n\
                 fn names() -> Vec[String] { let v: Vec[String] = [\"alpha\", \"beta\", \"gamma\"]; return v; }\n\
                 fn mkrows() -> Vec[Vec[i64]] { let v: Vec[Vec[i64]] = [[1, 2, 3], [4, 5, 6]]; return v; }\n\
                 fn main() {\n\
                     let b = B { xs: [7, 8, 9] };\n\
                     let grid: Vec[Vec[i64]] = [[1, 2], [3, 4]];\n\
                     let held: Vec[String] = [\"one\", \"two\"];\n\
                     println(f\"{names()[1]}\");\n\
                     println(names()[1]);\n\
                     println(f\"{names()[0]}-{names()[2]}\");\n\
                     println(f\"{mkrows()[1]}\");\n\
                     println(mkrows()[1]);\n\
                     println(f\"{b.xs}\");\n\
                     println(b.xs);\n\
                     println(f\"{grid[1]}\");\n\
                     println(grid[1]);\n\
                     println(f\"{held[1]}\");\n\
                     println(held[1]);\n\
                     let r = names();\n\
                     println(f\"{r[2]}\");\n\
                     println(f\"{b.xs}\");\n\
                     println(f\"{held[0]}\");\n\
                 }\n"
            ),
            Some(
                "beta\nbeta\nalpha-gamma\n[4, 5, 6]\n[4, 5, 6]\n[7, 8, 9]\n[7, 8, 9]\n\
                 [3, 4]\n[3, 4]\ntwo\ntwo\ngamma\n[7, 8, 9]\none\n"
                    .to_string()
            )
        );
}

/// A `T: Ord` bound on a GENERIC IMPL admits a tuple, as the identical
/// bound on a free fn always has (B-2026-08-27-33).
///
/// `impl[T: Ord] W[T]` refused `W[(i64, i64)]` outright — "`(i64, i64)`
/// does not implement `Ord`" — while `fn pick[T: Ord](a: T)` took the same
/// tuple, so the answer depended on which side of the call the bound was
/// written on. `W.add` never orders anything: the rejection was of a
/// correct program on both backends, which is why this is an E2E fixture
/// and not only a typecheck one.
///
/// The bound and the OPERATOR are separate gates, and this fixture is the
/// bound one. It deliberately does NOT order anything through `T`: a
/// comparison written against the type PARAMETER (`fn beats(x: T, y: T)
/// { x < y }`) lowers to `T.cmp`. When this row landed, tuples had no
/// `cmp` on ANY surface — B-2026-08-27-41, since fixed
/// (`test_e2e_tuple_cmp_to_ordering` below). What still stands between
/// this row and `PriorityQueue[(i64, i64)]` is the remaining half: the
/// mono substitution channel drops a tuple type argument, so a receiver
/// typed as a bare `T` never learns it is one (B-2026-08-27-40).
/// Ordering two tuples DIRECTLY is covered by
/// `test_e2e_tuple_comparison_and_equality` above.
///
/// TWO tuple instantiations, which this fixture could not carry when it
/// was written: the mono mangler mapped every tuple to the same opaque
/// `$struct` token, so a second one collided on a single symbol and failed
/// module verification. That was a separate defect from this row's bound
/// gate — it reproduced with the bound dropped entirely (`impl[T] W[T]`)
/// — and it is fixed under B-2026-08-27-40, whose
/// `test_e2e_two_tuple_instantiations_of_one_generic_get_distinct_symbols`
/// is the dedicated fixture. Widening here keeps the two rows' surfaces
/// crossed: the bound must admit a tuple AND the tuple must reach its own
/// symbol.
/// B-2026-08-27-40, forwarding leg — a tuple type argument FORWARDED from
/// one generic function into another.
///
/// Neither of the two sources that carry a tuple into the substitution can
/// reach this shape: `outer[U](a: U, b: U)` calling `pick(a, b)` passes an
/// argument whose static type is the caller's own type PARAM, so there is
/// no receiver instantiation to read and no tuple in `expr_types` to
/// recover. `infer_type_args` still binds `T` correctly from the LLVM
/// value, so the BODY was always right — only the SYMBOL collided, and the
/// second instantiation failed module verification against the first's
/// signature. The mangle's LLVM-shape fallback is what separates them.
///
/// The `String` row is the control: the name channel carries `U -> "String"`
/// straight through the forward, so that spelling has always worked, and it
/// must keep working — the fallback fires only where the recorded name does
/// not resolve to a type.
///
/// Three tuple shapes with three distinct LLVM shapes (`{i64,i64}`,
/// `{double,double}`, `{i64,i64,i64}`), which is what the fallback keys on.
/// Two tuples that differ only in SIGNEDNESS (`(i64, i64)` vs `(u64, u64)`)
/// lower to one LLVM shape and would still share a symbol here; that is the
/// documented limit of the fallback, and it is unreachable on the direct
/// call path, which takes the exact `TypeExpr` instead.
#[test]
fn test_e2e_tuple_type_arg_forwarded_between_generic_fns_gets_distinct_symbols() {
    assert_eq!(
        run_program(
            r#"
fn pick[T](a: T, b: T) -> T { return a; }
fn outer[U](a: U, b: U) -> U { return pick(a, b); }

fn main() {
    let x = outer((1, 2), (3, 4));
    println(f"{x.0}:{x.1}");
    let y = outer((1.5, 2.5), (3.5, 4.5));
    println(f"{y.0}:{y.1}");
    let t = outer((1, 2, 3), (4, 5, 6));
    println(f"{t.0},{t.1},{t.2}");
    let s = outer("aa", "bb");
    println(f"{s}");
}
"#,
        ),
        Some("1:2\n1.5:2.5\n1,2,3\naa\n".to_string())
    );
}

/// B-2026-08-27-40, repro B — the MANGLING half of the same cause, loud
/// where the one above is silent. Every tuple lowers to the opaque
/// `$struct` token and has no `subst_names` entry to disambiguate it, so
/// two tuple instantiations of one generic shared a single symbol and the
/// second failed module verification:
///
/// ```text
/// Call parameter type does not match function signature!
///   ... call void @"W.add$struct"(ptr %s, { { ptr, i64, i64 }, i64 } ...)
/// ```
///
/// Kept separate from the fixture above even though that one now also
/// carries three tuple instantiations: this is the row's own minimal
/// repro, it fails at a different phase (module verification, not the
/// run), and a future change that fixes the substitution channel without
/// the mangle axis would still pass the other test's shapes one at a time.
///
/// BOTH CALL SHAPES, because they bind the type argument through different
/// channels and only one of them was covered by the row's own repro. An
/// impl method takes its type args from the receiver's recorded
/// instantiation; a FREE generic fn has no receiver and binds from
/// `infer_type_args`, which sees only the LLVM shape — where every tuple is
/// the same opaque `struct`. So `pick((1, 2), …)` beside
/// `pick((1.5, 2.5), …)` collided on `pick$struct` for the same reason
/// `W.add$struct` did, and fixing the method half alone left it standing.
/// `(1.5, 2.5)` is deliberately a same-ARITY, same-size tuple: it differs
/// from `(1, 2)` only in element type, so nothing but the mangle axis can
/// separate the two.
#[test]
fn test_e2e_two_tuple_instantiations_of_one_generic_get_distinct_symbols() {
    assert_eq!(
        run_program(
            r#"
struct W[=T] { v: Vec[T] }

impl[T] W[T] {
    fn add(mut ref self, x: T) { self.v.push(x); }
}

fn pick[T](a: T, b: T) -> T { return a; }

fn main() {
    let mut w: W[(i64, i64)] = W { v: Vec.new() };
    w.add((1, 2));
    let mut s: W[(String, i64)] = W { v: Vec.new() };
    s.add(("a", 1));
    println(f"{w.v.len()} {s.v.len()}");

    let x = pick((1, 2), (3, 4));
    println(f"{x.0}:{x.1}");
    let y = pick((1.5, 2.5), (3.5, 4.5));
    println(f"{y.0}:{y.1}");
    let z = pick(("a", 1), ("b", 2));
    println(f"{z.0}:{z.1}");
}
"#,
        ),
        Some("1 1\n1:2\n1.5:2.5\na:1\n".to_string())
    );
}

/// B-2026-08-29-42 — a REDUCED-PRECISION receiver picked the DOUBLE
/// libm symbol, and got silent garbage.
///
/// The `tan` / `atan2` / inverse-trig / hyperbolic set has no LLVM-18
/// intrinsic, so codegen calls libm directly and chose the symbol with
/// `let is_f32 = fty == f32_type()` — a BINARY test. `f16` and `bf16` are
/// neither, so they took the f64 arm: they called `tan` (not `tanf`) and
/// declared it `bfloat tan(bfloat)`, a signature libm does not have. libm
/// read a double out of 16 bits.
///
/// This one is worth a fixture of its own because it is NOT the
/// `Cannot select` class the neighbouring tests are about: it produced
/// WRONG ANSWERS on every target, x86 included, with no diagnostic —
/// `x.tan()` returned `x` unchanged and `x.hypot(y)` returned 2.4e16.
/// A test that only asserts "it compiles" passes against the bug.
#[test]
fn e2e_half_precision_libm_calls_use_the_f32_symbol() {
    // f16 and bf16 both round 2.0 exactly, so the two agree here; the
    // point is the VALUES, not the width.
    for ty in ["f16", "bf16"] {
        let src = format!(
            r#"
fn main() {{
    let x: {ty} = 2.0 as f32 as {ty};
    let y: {ty} = 3.0 as f32 as {ty};
    println(f"tan={{x.tan()}} atan={{x.atan()}} sinh={{x.sinh()}}");
    println(f"hypot={{x.hypot(y)}} atan2={{x.atan2(y)}}");
}}
"#
        );
        let out = run_program(&src);
        assert!(out.is_some(), "{ty} program failed to build");
        let out = out.unwrap();
        // The bug's signature: every unary result came back as the
        // receiver (2), and hypot returned a huge integer.
        assert!(
            !out.contains("tan=2 "),
            "{ty}: `tan` returned its receiver — the f64-symbol bug:\n{out}"
        );
        assert!(
            out.starts_with("tan=-2.1"),
            "{ty}: expected tan(2) ≈ -2.18, got:\n{out}"
        );
        assert!(
            out.contains("atan=1.1"),
            "{ty}: expected atan(2) ≈ 1.107, got:\n{out}"
        );
        assert!(
            out.contains("sinh=3.6"),
            "{ty}: expected sinh(2) ≈ 3.627, got:\n{out}"
        );
        assert!(
            out.contains("hypot=3.6"),
            "{ty}: expected hypot(2,3) ≈ 3.606, got:\n{out}"
        );
    }
}

/// B-2026-08-18-3 — indexing by a `let`-bound range. The interpreter ICE'd
/// and both compiled backends refused the program with "Undefined variable
/// 'r'", because codegen has no runtime `Range` value to hand the index.
/// It does have the BOUNDS: B-2026-08-17-29 spills them at the `let`, and
/// this reads the same captured pair the for-loop position reads.
///
/// Pinned against the inline spelling, which is the same slice written the
/// way that always worked.
#[test]
fn indexing_by_a_let_bound_range_slices_like_the_inline_form() {
    for (bound, inline) in [
        (
            "let r = 1..3; let s = v[r]; println(s.len());",
            "let s = v[1..3]; println(s.len());",
        ),
        (
            "let r = 1..3; let s = v[r]; println(s[0]); println(s[1]);",
            "let s = v[1..3]; println(s[0]); println(s[1]);",
        ),
        (
            "let r = 1..=2; let s = v[r]; println(s.len()); println(s[1]);",
            "let s = v[1..=2]; println(s.len()); println(s[1]);",
        ),
        (
            "let r = 2..2; let s = v[r]; println(s.len());",
            "let s = v[2..2]; println(s.len());",
        ),
    ] {
        let src = |body: &str| format!("fn main() {{ let v = [10, 20, 30, 40]; {body} }}\n");
        let Some(got) = run_program(&src(bound)) else {
            return;
        };
        let want = run_program(&src(inline)).expect("the inline control must build");
        assert_eq!(got, want, "`{bound}` must slice as `{inline}` does");
    }

    // A range is a value: its bounds are fixed at the `let`, so mutating
    // the source binding afterwards must not move the window
    // (B-2026-08-17-29's rule, in the index position).
    let Some(out) = run_program(
        "fn main() { let v = [10, 20, 30, 40]; let mut a = 1; let r = a..3; a = 0;\n\
             let s = v[r]; println(s.len()); println(s[0]); }\n",
    ) else {
        return;
    };
    assert_eq!(out, "2\n20\n", "the bounds are captured at the binding");
}

/// B-2026-08-22-4 — an inline associated-type binding on an
/// ARGUMENT-position `impl Trait`.
///
/// Argument position is the half that reaches codegen: the desugar turns
/// `impl Src[Item = i64]` into a named synthetic parameter plus the
/// ordinary `where T.Item = i64` constraint, so the backend sees the
/// monomorphized generic it has always seen and the binding costs it
/// nothing. This pins that the binding does not perturb that lowering.
///
/// The RETURN-position half is deliberately absent, and not because it
/// works: a method call through a return-position existential has no
/// codegen dispatcher AT ALL — with or without a binding — so such a
/// program is check-green, `run --interp`-green and `build`-red. That
/// gap predates this change (the slice-3 entry in phase-5-diagnostics.md
/// records "No codegen support") and is filed as B-2026-08-22-12; adding
/// a test for it here would pin a failure, not a behaviour.
#[test]
fn test_e2e_impl_trait_argument_position_inline_assoc_binding() {
    let src = r#"
trait Src {
    type Item;
    fn get(ref self) -> Self.Item;
}

struct S { v: i64 }

impl Src for S {
    type Item = i64;
    fn get(ref self) -> i64 { self.v }
}

fn take(s: impl Src[Item = i64]) -> i64 { s.get() }

fn main() {
    println(take(S { v: 5 }));
    println(take(S { v: 37 }));
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("5\n37\n"));
}

/// B-2026-09-09-22 — an `Option`/`Result` payload of `Vec[<element that
/// owns heap of its own>]` had its elements released TWICE, so
/// `match o { Some(t) => … }` aborted with `free(): double free detected in
/// tcache 2` on every compiled backend, at both opt levels and either way
/// on auto-par, while `--interp` printed the right answer.
///
/// ONE owner, two loops. `emit_free_inline_payload_overlay` ran an
/// aggregate DRAIN over the payload's elements (B-2026-08-14-15 leg B) and
/// then, as a separate `if`, its older one-level `{ptr,len,cap}` recursion
/// over the same elements. Leg B's own doc argued the two were "disjoint by
/// construction", and when it landed that was true —
/// `vec_elem_agg_drop_for_type_expr` answered `None` for every element the
/// recursion handles. It stopped being true once a `Vec[Vec[T]]` element
/// resolved a drain: such an element is a vec-struct AND has an agg drop,
/// so both loops ran over it.
///
/// `FreeVecBuffer` orders the identical pair correctly and says why —
/// `if agg_drop { … } else if …`, "running both would double-free the
/// direct heap fields" (B-2026-06-12-6). The overlay now matches it.
///
/// Cells 6-8 are the shapes that were already correct and say why the bug
/// hid: an element with NO drain (`Vec[String]`, `Vec[Vec[i64]]`) only ever
/// ran the recursion, and a USER enum reaches its payload through
/// `EnumDrop` rather than this overlay at all — so the whole `Vec`/`Option`
/// surface looked fine unless the element itself owned heap.
#[test]
fn e2e_inline_optres_vec_payload_drains_its_elements_once() {
    for (label, src, want) in [
            // 1 — the row's own shape, read two levels deep.
            (
                "option-arm-indexed",
                "fn plainV(x: Option[Vec[Vec[String]]]) {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t[0][0]}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let v: Vec[Vec[String]] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
                 \x20   plainV(Some(v));\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 2 — `Result` reaches the same overlay through its `Ok` arm, so a
            //     fix keyed on `Option` alone would leave this half aborting.
            (
                "result-ok-arm",
                "fn plainV(x: Result[Vec[Vec[String]], i64]) {\n\
                 \x20   match x { Ok(t) => { println(f\"s:{t[0][0]}\") } Err(e) => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let v: Vec[Vec[String]] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
                 \x20   plainV(Ok(v));\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 3 — NO index anywhere. The binding alone was enough; the index
            //     never had anything to do with it.
            (
                "option-arm-no-index",
                "fn plainV(x: Option[Vec[Vec[String]]]) {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t.len()}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let v: Vec[Vec[String]] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
                 \x20   plainV(Some(v));\n\
                 }\n",
                "s:2\n",
            ),
            // 4 — no named local at all, so the caller's own binding cannot be
            //     one of the two frees.
            (
                "inline-literal-argument",
                "fn plainV(x: Option[Vec[Vec[String]]]) {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t.len()}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() { plainV(Some([[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]])); }\n",
                "s:2\n",
            ),
            // 5 — one frame, no call: the minimal reproducer, whose single
            //     cleanup action was doing both frees by itself.
            (
                "local-option-one-frame",
                "fn main() {\n\
                 \x20   let o: Option[Vec[Vec[String]]] = Some([[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]]);\n\
                 \x20   match o { Some(t) => { println(f\"s:{t.len()}\") } None => { println(\"n\") } }\n\
                 }\n",
                "s:2\n",
            ),
            // 6 — CONTROL: a `String` element has no drain, so only the
            //     recursion ever ran and this shape was always correct. It is
            //     also the shape leg B's disjointness claim was written for.
            (
                "string-element-control",
                "fn plainV(x: Option[Vec[String]]) {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t[0]}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let v: Vec[String] = [f\"aaaaaaaa0\", f\"bbbbbbbb1\"];\n\
                 \x20   plainV(Some(v));\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 7 — CONTROL: a SCALAR inner element resolves no drain either, so
            //     the nesting on its own was never the trigger.
            (
                "scalar-inner-control",
                "fn plainV(x: Option[Vec[Vec[i64]]]) {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t[0][1]}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let v: Vec[Vec[i64]] = [[10, 11], [20]];\n\
                 \x20   plainV(Some(v));\n\
                 }\n",
                "s:11\n",
            ),
            // 8 — CONTROL: the same payload in a USER enum never reaches this
            //     overlay (it drops through `EnumDrop`), which is what made the
            //     envelope look like the discriminator.
            (
                "user-enum-control",
                "enum E { A(Vec[Vec[String]]), B }\n\
                 fn plainE(x: E) {\n\
                 \x20   match x { E.A(t) => { println(f\"s:{t[0][0]}\") } E.B => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let v: Vec[Vec[String]] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
                 \x20   plainE(E.A(v));\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
        ] {
            let Some(out) = run_program(src) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-10-1 — passing `Some(<a Vec of heap-bearing structs>)` to a
/// function double freed on every compiled backend, at both opt levels and
/// either way on auto-par, while `--interp` was correct. No `match`, no
/// index, no second binding: `fn f(x: Option[Vec[S]]) { println("in") }`
/// with `f(Some([S { s: f".." }, ..]))` is the whole reproducer.
///
/// The callee entry-copies an owned `Option[Vec[T]]` payload so the two
/// frames own separate buffers. That copy resolved its element `TypeExpr`
/// through `.filter(elem_te_needs_direct_recursive_drain)` — a NAME LIST
/// (`String`/`Vec`/`Map`/`Set`), and only the FALLBACK half of
/// `vec_element_drain_fn`, whose primary half
/// (`vec_elem_agg_drop_for_type_expr`) is what answers for a user struct.
/// So the DROP side drained a `Vec[S]` payload's elements while the COPY
/// side was handed no element type at all and skipped its entire element
/// chain — including the aggregate arm written for exactly this shape. The
/// "copy" was a flat memcpy of the element array, aliasing every element's
/// `String` with the caller's, and both frames then freed it.
///
/// The user-ENUM sibling one function up already passes element depth
/// unconditionally, with the reason in its comment ("unconditional since
/// the drop side drains too"), which is why an enum payload of the same
/// type was always correct. The two envelope paths now match it.
///
/// Cells 6-8 are the shapes that were already right, and each names a
/// different reason: a no-heap element has nothing to alias, a bare
/// `Vec[S]` argument never builds an envelope payload, and a local option
/// in one frame is never entry-copied because there is no callee.
#[test]
fn e2e_inline_optres_vec_payload_entry_copy_is_element_deep() {
    const S: &str = "struct S { s: String }\n";
    for (label, src, want) in [
            // 1 — the minimal reproducer: the param is never even looked at.
            (
                "option-param-untouched",
                "fn plainV(x: Option[Vec[S]]) { println(\"in\"); }\n\
                 fn main() { plainV(Some([S { s: f\"aaaaaaaa0\" }, S { s: f\"bbbbbbbb1\" }])); }\n",
                "in\n",
            ),
            // 2 — the `Result` half, which resolves its element type through
            //     the same filter and said so in its own comment.
            (
                "result-ok-half",
                "fn plainV(x: Result[Vec[S], i64]) {\n\
                 \x20   match x { Ok(t) => { println(f\"s:{t.len()}\") } Err(e) => { println(\"n\") } }\n\
                 }\n\
                 fn main() { plainV(Ok([S { s: f\"aaaaaaaa0\" }, S { s: f\"bbbbbbbb1\" }])); }\n",
                "s:2\n",
            ),
            // 3 — a NAMED local as the payload source rather than a literal.
            (
                "named-local-source",
                "fn plainV(x: Option[Vec[S]]) {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t.len()}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let v: Vec[S] = [S { s: f\"aaaaaaaa0\" }, S { s: f\"bbbbbbbb1\" }];\n\
                 \x20   plainV(Some(v));\n\
                 }\n",
                "s:2\n",
            ),
            // 4 — TWO fields, one of them scalar. Rules out the first guess,
            //     that a single-field struct was being mistaken for a
            //     `{ptr,len,cap}` vec-struct.
            (
                "two-field-struct",
                "struct S2 { a: String, b: i64 }\n\
                 fn plainV(x: Option[Vec[S2]]) {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t.len()}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() { plainV(Some([S2 { a: f\"aaaaaaaa0\", b: 1 }, S2 { a: f\"bbbbbbbb1\", b: 2 }])); }\n",
                "s:2\n",
            ),
            // 5 — the arm actually reads an element through, so the copy has
            //     to be correct rather than merely balanced.
            (
                "arm-reads-element",
                "fn plainV(x: Option[Vec[S]]) {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t[0].s}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let v: Vec[S] = [S { s: f\"aaaaaaaa0\" }, S { s: f\"bbbbbbbb1\" }];\n\
                 \x20   plainV(Some(v));\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 6 — CONTROL: a no-heap element has nothing to alias, so the flat
            //     memcpy was always a complete copy for it.
            (
                "no-heap-element-control",
                "struct S3 { n: i64 }\n\
                 fn plainV(x: Option[Vec[S3]]) {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t.len()}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() { plainV(Some([S3 { n: 1 }, S3 { n: 2 }])); }\n",
                "s:2\n",
            ),
            // 7 — CONTROL: the same `Vec[S]` as a bare argument. No envelope,
            //     so none of this machinery runs and it was always correct.
            (
                "bare-vec-argument-control",
                "fn plainV(x: Vec[S]) { println(f\"s:{x.len()}\"); }\n\
                 fn main() { plainV([S { s: f\"aaaaaaaa0\" }, S { s: f\"bbbbbbbb1\" }]); }\n",
                "s:2\n",
            ),
            // 8 — CONTROL: one frame, no call. There is no entry copy to get
            //     wrong, which is what made the call boundary the tell.
            (
                "local-option-one-frame-control",
                "fn main() {\n\
                 \x20   let v: Vec[S] = [S { s: f\"aaaaaaaa0\" }, S { s: f\"bbbbbbbb1\" }];\n\
                 \x20   let o: Option[Vec[S]] = Some(v);\n\
                 \x20   match o { Some(t) => { println(f\"s:{t.len()}\") } None => { println(\"n\") } }\n\
                 }\n",
                "s:2\n",
            ),
        ] {
            let Some(out) = run_program(&format!("{S}{src}")) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-20-1 — an `Array[T, N]` enum payload narrow enough to ride
/// INLINE keeps its TYPE when bound in a match arm.
///
/// `reconstruct_payload_value`'s single-word tail returned the raw payload
/// word as the binding, so the array-ness was gone: `x[0].v` failed codegen
/// with "Index operator applied to non-array type", and `eat(x)` failed
/// module verification with a bare `i64` where `[1 x { i64 }]` was wanted —
/// both on programs `--interp` ran correctly. `pattern_payload_llvm_type`
/// has carried the right `Array` / `Vector` arm since B-2026-08-31-18, but
/// only the DEBOX load ever consulted it; the inline tail never did. A
/// two-element array is two words, so the guard was already false for it
/// and only the one-word case was ever exposed.
///
/// The `Plain` element is what establishes this is the binding's SHAPE and
/// not its ownership: it has no `Drop` impl anywhere and failed the same
/// way.
///
/// Sibling of `e2e_inline_array_enum_payload_keeps_its_value`, and the two
/// faults are independent — with only this half repaired the programs
/// COMPILED and printed `r:0`, the value having been destroyed at the pack.
#[test]
fn e2e_inline_array_enum_payload_keeps_its_type_in_a_match_arm() {
    let src = r#"
struct Cell { v: i64 }
impl Drop for Cell { fn drop(mut ref self) { println(f"dc{self.v}") } }
struct Plain { v: i64 }

enum One { P(Array[Cell, 1]), Q }
enum Flat { P(Array[Plain, 1]), Q }

fn eat_cell(a: Array[Cell, 1]) -> i64 { return a[0].v; }
fn eat_plain(a: Array[Plain, 1]) -> i64 { return a[0].v; }

fn hand(g: One) -> i64 { match g { One.P(x) => { return eat_cell(x); } One.Q => { return 0; } } }
fn read(g: One) -> i64 { match g { One.P(x) => { return x[0].v; } One.Q => { return 0; } } }
fn hand_flat(g: Flat) -> i64 { match g { Flat.P(x) => { return eat_plain(x); } Flat.Q => { return 0; } } }
fn read_opt(o: Option[Array[Cell, 1]]) -> i64 { match o { Some(x) => { return x[0].v; } None => { return 0; } } }

fn main() {
    let mut n = 0;
    while n < 3 {
        let a: Array[Cell, 1] = [Cell { v: 10 + n }];
        let g: One = One.P(a);
        println(f"h:{hand(g)}");
        let b: Array[Cell, 1] = [Cell { v: 20 + n }];
        let r: One = One.P(b);
        println(f"r:{read(r)}");
        let c: Array[Plain, 1] = [Plain { v: 30 + n }];
        let fl: Flat = Flat.P(c);
        println(f"f:{hand_flat(fl)}");
        let d: Array[Cell, 1] = [Cell { v: 40 + n }];
        let o: Option[Array[Cell, 1]] = Some(d);
        println(f"o:{read_opt(o)}");
        let e: Array[Cell, 1] = [Cell { v: 50 + n }];
        One.P(e);
        println("kept");
        n = n + 1;
    }
    println("end");
}
"#;
    let mut want = String::new();
    for n in 0..3 {
        want.push_str(&format!("h:{}\ndc{}\n", 10 + n, 10 + n));
        want.push_str(&format!("r:{}\ndc{}\n", 20 + n, 20 + n));
        want.push_str(&format!("f:{}\n", 30 + n));
        want.push_str(&format!("o:{}\ndc{}\n", 40 + n, 40 + n));
        want.push_str(&format!("dc{}\nkept\n", 50 + n));
    }
    want.push_str("end\n");
    assert_eq!(run_program(src).as_deref(), Some(want.as_str()));
}

/// B-2026-09-19-49 — an `Array[T, N]` enum payload narrow enough to ride
/// INLINE keeps its VALUE.
///
/// `coerce_to_payload_words` opens with a fast path taken when the variant
/// slot is one word and the value is one word wide, and it handed the value
/// to `coerce_to_i64`, whose tail returns a literal ZERO for any aggregate
/// it does not recognise — and it recognises a one-FIELD struct, not an
/// array. So the payload was destroyed AT THE PACK and every reader
/// downstream was faithfully correct about a zero. That is why the row
/// measured valgrind CLEAN: nothing is corrupt, the zero is simply what was
/// stored.
///
/// The width is what hid it. `payload_word_count_for_type_expr` sizes a
/// source-written `Array[T, N]` at its conservative one-word tail, so only
/// an array whose elements total one word satisfies BOTH halves of that
/// guard — `Array[Sd, 2]` and every wider element already took the
/// per-element arms and were correct. An array payload wrong at exactly one
/// width is the signature.
///
/// The `Option` line is a built-in control rather than extra coverage: the
/// seeded envelope's payload area is three words, so `num_words <= 1` is
/// false there and it never took the fast path. It printed the right value
/// before this fix and after it, which is what pins the defect to the SLOT
/// WIDTH rather than to arrays.
#[test]
fn e2e_inline_array_enum_payload_keeps_its_value() {
    let src = r#"
struct Cell { v: i64 }
impl Drop for Cell { fn drop(mut ref self) { println(f"dc{self.v}") } }

enum One { P(Array[Cell, 1]), Q }

fn main() {
    let mut n = 0;
    while n < 3 {
        let a: Array[Cell, 1] = [Cell { v: 10 + n }];
        let g: One = One.P(a);
        println("bound");
        let b: Array[Cell, 1] = [Cell { v: 20 + n }];
        One.P(b);
        println("dropped");
        let c: Array[Cell, 1] = [Cell { v: 30 + n }];
        let o: Option[Array[Cell, 1]] = Some(c);
        println("opt");
        n = n + 1;
    }
    println("end");
}
"#;
    let mut want = String::new();
    for n in 0..3 {
        want.push_str(&format!("dc{}\nbound\n", 10 + n));
        want.push_str(&format!("dc{}\ndropped\n", 20 + n));
        want.push_str(&format!("dc{}\nopt\n", 30 + n));
    }
    want.push_str("end\n");
    assert_eq!(run_program(src).as_deref(), Some(want.as_str()));
}
