//! Option, Result, `?`, try -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer option_result::
//!
//! New fixtures about Option, Result, `?`, try belong in this file.

use super::*;

/// B-2026-09-04-1 — a STRUCT-FIELD destructure leaf typed `Result[<struct with
/// a Drop body>, _]` runs the payload's body exactly once, on every surface.
///
/// The row's cell (`loc`): the arm only borrows `r`, so nothing at the arm owned
/// the body, and the leaf's memory action was a body-less struct drop — `dR101`
/// ran under `--interp` and on none of jit/aot/AUTO_PAR=0. `iflet`, `errs`,
/// `noread`, `solo` and `rename` are the same defect in five spellings the row
/// did not record; `unused` is the UNCONSUMED leaf, which lost the body on every
/// backend (the B-2026-09-03-33 deferral, closed here). `fcall` / `flit` /
/// `fcallu` are FRESH sources, whose `Result` field had no owner at all (a leak
/// the optimizer's dead-chain mask hid until a body walk made the words live;
/// `fstru` is the unmasked direct-`String` twin, valgrind: 4 bytes). The `w*`
/// cells are the heap-BOXED payload (seven words), which the inline tracker
/// declined and nothing else owned. `param` is the by-value-param source, whose
/// own field walk already ran the body — unchanged.
///
/// The interpreter twin runs the same string; the ASAN twin is
/// `asan_struct_field_result_leaf_is_balanced`.
///
/// ASAN twin of `e2e_struct_field_result_leaf_owns_its_payload_body` (tests/codegen.rs): the cells that aborted did so
/// on a double free, and the fresh-source cells leaked, so the pin here is
/// the balance itself; the stdout expectation is the same as the E2E's.
#[test]
fn asan_struct_field_result_leaf_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct W { id: i64, x: String, y: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.x}{self.y}") } }
fn mkw(n: i64) -> W { return W { id: n, x: f"x{n}", y: f"y{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct HoErr { a: R, b: Result[String, R] }
struct SoloRes { b: Result[R, String] }
struct HoStr { a: R, b: Result[String, String] }
struct HoW { a: R, b: Result[W, String] }
fn mkho(n: i64) -> HoRes { return HoRes { a: mk(n), b: Result.Ok(mk(n + 100)) }; }
fn mkhs(n: i64) -> HoStr { return HoStr { a: mk(n), b: Result.Ok(f"s{n + 100}") }; }
fn mkhw(n: i64) -> HoW { return HoW { a: mk(n), b: Result.Ok(mkw(n + 100)) }; }

fn loc()    { let h = HoRes { a: mk(1), b: Result.Ok(mk(101)) }; let HoRes { a, b } = h; println(f"  rd{a.id}")
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn iflet()  { let h = HoRes { a: mk(2), b: Result.Ok(mk(102)) }; let HoRes { a, b } = h; println(f"  rd{a.id}")
              if let Result.Ok(r) = b { println(f"  ok{r.id}") } }
fn errs()   { let h = HoErr { a: mk(3), b: Result.Err(mk(103)) }; let HoErr { a, b } = h; println(f"  rd{a.id}")
              match b { Result.Ok(s) => println(f"  ok{s}"), Result.Err(r) => println(f"  er{r.id}") } }
fn noread() { let h = HoRes { a: mk(4), b: Result.Ok(mk(104)) }; let HoRes { a, b } = h; println(f"  rd{a.id}")
              match b { Result.Ok(r) => println("  ok"), Result.Err(e) => println(f"  er{e}") } }
fn solo()   { let h = SoloRes { b: Result.Ok(mk(105)) }; let SoloRes { b } = h;
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn rename() { let h = HoRes { a: mk(6), b: Result.Ok(mk(106)) }; let HoRes { a: aa, b: bb } = h; println(f"  rd{aa.id}")
              match bb { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn unused() { let h = HoRes { a: mk(7), b: Result.Ok(mk(107)) }; let HoRes { a, b } = h; println(f"  rd{a.id}") }
fn fcall()  { let HoRes { a, b } = mkho(8); println(f"  rd{a.id}")
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn flit()   { let HoRes { a, b } = HoRes { a: mk(9), b: Result.Ok(mk(109)) }; println(f"  rd{a.id}")
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn fcallu() { let HoRes { a, b } = mkho(10); println(f"  rd{a.id}") }
fn fstru()  { let HoStr { a, b } = mkhs(11); println(f"  rd{a.id}") }
fn fstr()   { let HoStr { a, b } = mkhs(12); println(f"  rd{a.id}")
              match b { Result.Ok(s) => println(f"  ok{s}"), Result.Err(e) => println(f"  er{e}") } }
fn wloc()   { let h = HoW { a: mk(13), b: Result.Ok(mkw(113)) }; let HoW { a, b } = h; println(f"  rd{a.id}")
              match b { Result.Ok(w) => println(f"  ok{w.id}"), Result.Err(e) => println(f"  er{e}") } }
fn wunused(){ let h = HoW { a: mk(14), b: Result.Ok(mkw(114)) }; let HoW { a, b } = h; println(f"  rd{a.id}") }
fn wfcall() { let HoW { a, b } = mkhw(15); println(f"  rd{a.id}")
              match b { Result.Ok(w) => println(f"  ok{w.id}"), Result.Err(e) => println(f"  er{e}") } }
fn wfcallu(){ let HoW { a, b } = mkhw(16); println(f"  rd{a.id}") }
fn param()  { take(HoRes { a: mk(17), b: Result.Ok(mk(117)) }) }
fn take(h: HoRes) { let HoRes { a, b } = h; println(f"  rd{a.id}")
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }

fn main() {
  println("loc");     loc()
  println("iflet");   iflet()
  println("errs");    errs()
  println("noread");  noread()
  println("solo");    solo()
  println("rename");  rename()
  println("unused");  unused()
  println("fcall");   fcall()
  println("flit");    flit()
  println("fcallu");  fcallu()
  println("fstru");   fstru()
  println("fstr");    fstr()
  println("wloc");    wloc()
  println("wunused"); wunused()
  println("wfcall");  wfcall()
  println("wfcallu"); wfcallu()
  println("param");   param()
  println("done")
}
"#,
        &[
            "loc",
            "  rd1",
            "dR1/t1",
            "  ok101",
            "dR101/t101",
            "iflet",
            "  rd2",
            "dR2/t2",
            "  ok102",
            "dR102/t102",
            "errs",
            "  rd3",
            "dR3/t3",
            "  er103",
            "dR103/t103",
            "noread",
            "  rd4",
            "dR4/t4",
            "  ok",
            "dR104/t104",
            "solo",
            "  ok105",
            "dR105/t105",
            "rename",
            "  rd6",
            "dR6/t6",
            "  ok106",
            "dR106/t106",
            "unused",
            "dR107/t107",
            "  rd7",
            "dR7/t7",
            "fcall",
            "  rd8",
            "dR8/t8",
            "  ok108",
            "dR108/t108",
            "flit",
            "  rd9",
            "dR9/t9",
            "  ok109",
            "dR109/t109",
            "fcallu",
            "dR110/t110",
            "  rd10",
            "dR10/t10",
            "fstru",
            "  rd11",
            "dR11/t11",
            "fstr",
            "  rd12",
            "dR12/t12",
            "  oks112",
            "wloc",
            "  rd13",
            "dR13/t13",
            "  ok113",
            "dW113/x113y113",
            "wunused",
            "dW114/x114y114",
            "  rd14",
            "dR14/t14",
            "wfcall",
            "  rd15",
            "dR15/t15",
            "  ok115",
            "dW115/x115y115",
            "wfcallu",
            "dW116/x116y116",
            "  rd16",
            "dR16/t16",
            "param",
            "  rd17",
            "  ok117",
            "dR117/t117",
            "dR17/t17",
            "done",
        ],
        "asan_struct_field_result_leaf_is_balanced",
    );
}

/// B-2026-09-04-21 — a struct destructured out of a PROJECTION of an owned local
/// (`let HoRes { a, b } = w.inner;`) hands its `Option` / `Result` leaf the
/// field, on every surface.
///
/// On the parent the leaf was a bit-copy VIEW registered in no set while the
/// root kept the memory, so `rmatch` (a consuming arm on a `Result[R, String]`
/// leaf) freed the payload's String from the arm binding and again from the
/// root's drop — glibc's `free(): double free detected in tcache 2` on an
/// ordinary build — and `rcall` / `resc` / `rrebind` / `two` / `awild` aborted
/// the same way; `unused` lost the body on every backend; `omatch` / `ocall` /
/// `oesc` ran the `Option` payload's body at the ROOT's last use (before the
/// leaf was read, or a second time) and `orebind` crashed silently. The leaf
/// now TRANSFERS the field out of the root (cap-zero in place, the same move the
/// by-value-param destructure makes) when the read is a move, and owns its own
/// defensive copy when the root is read again (`rlive`, `olive`, `livecall`);
/// the root's walk is masked for the field either way, so the body fires at
/// the leaf's last use. `wild` / `errs` are the no-payload controls; `nest`
/// moves the destructure into a block. A by-value-PARAM root is deliberately
/// not on this path (its own walk runs the bodies) and keeps its own row.
///
/// ASAN twin of `e2e_projection_source_optres_leaf_owns_its_field` (tests/codegen.rs): six of these cells aborted on
/// a double free and one crashed silently, so the balance is the pin.
#[test]
fn asan_projection_source_optres_leaf_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct HoOpt { a: R, b: Option[R] }
struct WrapR { inner: HoRes }
struct WrapO { inner: HoOpt }
struct Outer { h: WrapR }
fn eat(x: R) { println(f"  eat{x.id}") }

fn unused()  { let w = WrapR { inner: HoRes { a: mk(1), b: Result.Ok(mk(101)) } }; let HoRes { a, b } = w.inner; println(f"  rd{a.id}") }
fn rmatch()  { let w = WrapR { inner: HoRes { a: mk(2), b: Result.Ok(mk(102)) } }; let HoRes { a, b } = w.inner; println(f"  rd{a.id}")
               match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn rlive()   { let w = WrapR { inner: HoRes { a: mk(3), b: Result.Ok(mk(103)) } }; let HoRes { a, b } = w.inner; println(f"  rd{a.id}")
               match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") }
               println(f"  w{w.inner.a.id}") }
fn ounused() { let w = WrapO { inner: HoOpt { a: mk(4), b: Option.Some(mk(104)) } }; let HoOpt { a, b } = w.inner; println(f"  rd{a.id}") }
fn omatch()  { let w = WrapO { inner: HoOpt { a: mk(5), b: Option.Some(mk(105)) } }; let HoOpt { a, b } = w.inner; println(f"  rd{a.id}")
               match b { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") } }
fn olive()   { let w = WrapO { inner: HoOpt { a: mk(6), b: Option.Some(mk(106)) } }; let HoOpt { a, b } = w.inner; println(f"  rd{a.id}")
               match b { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") }
               println(f"  w{w.inner.a.id}") }
fn two()     { let g = Outer { h: WrapR { inner: HoRes { a: mk(7), b: Result.Ok(mk(107)) } } }; let HoRes { a, b } = g.h.inner; println(f"  rd{a.id}")
               match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn rcall()   { let w = WrapR { inner: HoRes { a: mk(8), b: Result.Ok(mk(108)) } }; let HoRes { a, b } = w.inner;
               match b { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") } }
fn resc()    { let w = WrapR { inner: HoRes { a: mk(9), b: Result.Ok(mk(109)) } }; let HoRes { a, b } = w.inner;
               let g = match b { Result.Ok(r) => r, Result.Err(e) => mk(0) }; println(f"  got{g.id}") }
fn rrebind() { let w = WrapR { inner: HoRes { a: mk(10), b: Result.Ok(mk(110)) } }; let HoRes { a, b } = w.inner; let c = b;
               match c { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn ocall()   { let w = WrapO { inner: HoOpt { a: mk(11), b: Option.Some(mk(111)) } }; let HoOpt { a, b } = w.inner;
               match b { Option.Some(r) => eat(r), Option.None => println("  none") } }
fn oesc()    { let w = WrapO { inner: HoOpt { a: mk(12), b: Option.Some(mk(112)) } }; let HoOpt { a, b } = w.inner;
               let g = match b { Option.Some(r) => r, Option.None => mk(0) }; println(f"  got{g.id}") }
fn orebind() { let w = WrapO { inner: HoOpt { a: mk(13), b: Option.Some(mk(113)) } }; let HoOpt { a, b } = w.inner; let c = b;
               match c { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") } }
fn wild()    { let w = WrapR { inner: HoRes { a: mk(14), b: Result.Ok(mk(114)) } }; let HoRes { a, b: _ } = w.inner; println(f"  rd{a.id}") }
fn awild()   { let w = WrapR { inner: HoRes { a: mk(15), b: Result.Ok(mk(115)) } }; let HoRes { a: _, b } = w.inner;
               match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn nest()    { let w = WrapR { inner: HoRes { a: mk(16), b: Result.Ok(mk(116)) } };
               { let HoRes { a, b } = w.inner; println(f"  rd{a.id}") }
               println("  outer") }
fn errs()    { let w = WrapR { inner: HoRes { a: mk(17), b: Result.Err("e17") } }; let HoRes { a, b } = w.inner;
               match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn livecall(){ let w = WrapR { inner: HoRes { a: mk(18), b: Result.Ok(mk(118)) } }; let HoRes { a, b } = w.inner;
               match b { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") }
               println(f"  w{w.inner.a.id}") }

fn main() {
  println("unused");   unused()
  println("rmatch");   rmatch()
  println("rlive");    rlive()
  println("ounused");  ounused()
  println("omatch");   omatch()
  println("olive");    olive()
  println("two");      two()
  println("rcall");    rcall()
  println("resc");     resc()
  println("rrebind");  rrebind()
  println("ocall");    ocall()
  println("oesc");     oesc()
  println("orebind");  orebind()
  println("wild");     wild()
  println("awild");    awild()
  println("nest");     nest()
  println("errs");     errs()
  println("livecall"); livecall()
  println("done")
}
"#,
        &[
            "unused",
            "dR101/t101",
            "  rd1",
            "dR1/t1",
            "rmatch",
            "  rd2",
            "dR2/t2",
            "  okt102",
            "dR102/t102",
            "rlive",
            "  rd3",
            "dR3/t3",
            "  okt103",
            "dR103/t103",
            "  w3",
            "ounused",
            "dR104/t104",
            "  rd4",
            "dR4/t4",
            "omatch",
            "  rd5",
            "dR5/t5",
            "  okt105",
            "dR105/t105",
            "olive",
            "  rd6",
            "dR6/t6",
            "  okt106",
            "dR106/t106",
            "  w6",
            "two",
            "  rd7",
            "dR7/t7",
            "  okt107",
            "dR107/t107",
            "rcall",
            "dR8/t8",
            "  eat108",
            "dR108/t108",
            "resc",
            "dR9/t9",
            "  got109",
            "dR109/t109",
            "rrebind",
            "dR10/t10",
            "  okt110",
            "dR110/t110",
            "ocall",
            "dR11/t11",
            "  eat111",
            "dR111/t111",
            "oesc",
            "dR12/t12",
            "  got112",
            "dR112/t112",
            "orebind",
            "dR13/t13",
            "  okt113",
            "dR113/t113",
            "wild",
            "dR114/t114",
            "  rd14",
            "dR14/t14",
            "awild",
            "dR15/t15",
            "  okt115",
            "dR115/t115",
            "nest",
            "dR116/t116",
            "  rd16",
            "dR16/t16",
            "  outer",
            "errs",
            "dR17/t17",
            "  ere17",
            "livecall",
            "dR18/t18",
            "  eat118",
            "dR118/t118",
            "  w18",
            "done",
        ],
        "asan_projection_source_optres_leaf_is_balanced",
    );
}

/// B-2026-09-04-25 — a struct destructured out of a PROJECTION of a by-value
/// PARAM (`fn f(w: WrapR) { let HoRes { a, b } = w.inner; .. }`) hands each
/// `Option` / `Result` leaf the field, on every surface.
///
/// On the parent the leaf was a view of the param's storage registered in no
/// set, while the param's own `StructDrop` still owned the field: `rmatch` ran
/// the payload's body twice on aot and aborted with glibc's `free(): double free
/// detected in tcache 2` on jit; `rlive` ran it twice around the later read;
/// `rcall` ran it from the arm binding and again from `eat`'s callee copy; and
/// under `--interp`, `wild` / `awild` ran a discarded field's body at the
/// destructure and again at the param's exit. The projection now takes the
/// identifier source's transfer (`sfld.move` into the param in place, the leaf
/// owning the field) and its leaves are param views (`mark_views`), so a
/// consuming arm's binding takes the memory-only struct drop its identifier
/// twin takes; the interpreter's discard walk defers a projected wildcard to
/// the param's drop as it already did for `let HoRes { a, b: _ } = h`. `two`
/// is the two-hop root, `errs` the `Err` side, `runused` / `ounused` the unread
/// leaf, `livecall` a moving arm with the param read again. A rebind of the
/// leaf and a `self` receiver split identically for the identifier source and
/// keep their own row.
///
/// ASAN twin of `e2e_param_projection_optres_leaf_owns_its_field` (tests/codegen.rs): `rmatch` aborted on a double
/// free under the JIT, so the balance is the pin.
#[test]
fn asan_param_projection_optres_leaf_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct HoErr { a: R, b: Result[String, R] }
struct HoOpt { a: R, b: Option[R] }
struct WrapR { inner: HoRes }
struct WrapE { inner: HoErr }
struct WrapO { inner: HoOpt }
struct Outer { h: WrapR }
fn eat(x: R) { println(f"  eat{x.id}") }

fn rmatch(w: WrapR)  { let HoRes { a, b } = w.inner; println(f"  rd{a.id}")
                       match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn rlive(w: WrapR)   { let HoRes { a, b } = w.inner; println(f"  rd{a.id}")
                       match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") }
                       println(f"  w{w.inner.a.id}") }
fn two(o: Outer)     { let HoRes { a, b } = o.h.inner; println(f"  rd{a.id}")
                       match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn rcall(w: WrapR)   { let HoRes { a, b } = w.inner;
                       match b { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") } }
fn runused(w: WrapR) { let HoRes { a, b } = w.inner; println(f"  rd{a.id}") }
fn errs(w: WrapE)    { let HoErr { a, b } = w.inner;
                       match b { Result.Ok(s) => println(f"  ok{s}"), Result.Err(r) => println(f"  er{r.tag}") } }
fn wild(w: WrapR)    { let HoRes { a, b: _ } = w.inner; println(f"  rd{a.id}") }
fn awild(w: WrapR)   { let HoRes { a: _, b } = w.inner;
                       match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn livecall(w: WrapR){ let HoRes { a, b } = w.inner;
                       match b { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") }
                       println(f"  w{w.inner.a.id}") }
fn omatch(w: WrapO)  { let HoOpt { a, b } = w.inner; println(f"  rd{a.id}")
                       match b { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") } }
fn olive(w: WrapO)   { let HoOpt { a, b } = w.inner; println(f"  rd{a.id}")
                       match b { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") }
                       println(f"  w{w.inner.a.id}") }
fn ocall(w: WrapO)   { let HoOpt { a, b } = w.inner;
                       match b { Option.Some(r) => eat(r), Option.None => println("  none") } }
fn ounused(w: WrapO) { let HoOpt { a, b } = w.inner; println(f"  rd{a.id}") }

fn main() {
  println("rmatch");   rmatch(WrapR { inner: HoRes { a: mk(1), b: Result.Ok(mk(101)) } })
  println("rlive");    rlive(WrapR { inner: HoRes { a: mk(2), b: Result.Ok(mk(102)) } })
  println("two");      two(Outer { h: WrapR { inner: HoRes { a: mk(3), b: Result.Ok(mk(103)) } } })
  println("rcall");    rcall(WrapR { inner: HoRes { a: mk(4), b: Result.Ok(mk(104)) } })
  println("runused");  runused(WrapR { inner: HoRes { a: mk(5), b: Result.Ok(mk(105)) } })
  println("errs");     errs(WrapE { inner: HoErr { a: mk(6), b: Result.Err(mk(106)) } })
  println("wild");     wild(WrapR { inner: HoRes { a: mk(7), b: Result.Ok(mk(107)) } })
  println("awild");    awild(WrapR { inner: HoRes { a: mk(8), b: Result.Ok(mk(108)) } })
  println("livecall"); livecall(WrapR { inner: HoRes { a: mk(9), b: Result.Ok(mk(109)) } })
  println("omatch");   omatch(WrapO { inner: HoOpt { a: mk(10), b: Option.Some(mk(110)) } })
  println("olive");    olive(WrapO { inner: HoOpt { a: mk(11), b: Option.Some(mk(111)) } })
  println("ocall");    ocall(WrapO { inner: HoOpt { a: mk(12), b: Option.Some(mk(112)) } })
  println("ounused");  ounused(WrapO { inner: HoOpt { a: mk(13), b: Option.Some(mk(113)) } })
  println("done")
}
"#,
        &[
            "rmatch",
            "  rd1",
            "  okt101",
            "dR101/t101",
            "dR1/t1",
            "rlive",
            "  rd2",
            "  okt102",
            "  w2",
            "dR102/t102",
            "dR2/t2",
            "two",
            "  rd3",
            "  okt103",
            "dR103/t103",
            "dR3/t3",
            "rcall",
            "  eat104",
            "dR104/t104",
            "dR4/t4",
            "runused",
            "  rd5",
            "dR105/t105",
            "dR5/t5",
            "errs",
            "  ert106",
            "dR106/t106",
            "dR6/t6",
            "wild",
            "  rd7",
            "dR107/t107",
            "dR7/t7",
            "awild",
            "  okt108",
            "dR108/t108",
            "dR8/t8",
            "livecall",
            "  eat109",
            "  w9",
            "dR109/t109",
            "dR9/t9",
            "omatch",
            "  rd10",
            "  okt110",
            "dR110/t110",
            "dR10/t10",
            "olive",
            "  rd11",
            "  okt111",
            "  w11",
            "dR111/t111",
            "dR11/t11",
            "ocall",
            "  eat112",
            "dR112/t112",
            "dR12/t12",
            "ounused",
            "  rd13",
            "dR113/t113",
            "dR13/t13",
            "done",
        ],
        "asan_param_projection_optres_leaf_is_balanced",
    );
}

/// B-2026-09-04-9 — the BALANCE half of
/// `e2e_fresh_tuple_option_binding_leaf_owns_its_payload_body`, and the
/// reason that fix waited on B-2026-09-04-10.
///
/// The binding leaf takes BOTH halves — the payload walker and the
/// `track_inline_option_agg_payload_var` owner — and each has its own way
/// to be wrong that a transcript cannot see. Bodies alone leak 76 bytes per
/// occurrence (arming the walker stops whoever was freeing the boxed
/// payload); the owner alone double-frees any leaf that moves. Both were
/// measured on this program's cells before the fix took its final shape.
///
/// `field` IS THE CELL THAT FAILED LAST. A leaf moved into a struct-literal
/// field aborted with `free(): double free detected in tcache 2` until the
/// transfer disarm was widened to the field-init sites; `arg` is its
/// counter-case, where the same widening would have lost a body instead.
/// The two are one line apart in the fixture and pull in opposite
/// directions, which is the whole shape of this fix.
///
/// `loopb` runs two iterations so a per-iteration imbalance shows as a
/// multiple.
#[test]
fn asan_fresh_tuple_option_binding_leaf_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}:{self.s}:{self.xs.len()}") } }
struct W { p: Option[R] }
fn mk(n: i64) -> R { return R { id: n, s: f"s{n}", xs: [n, n] }; }
fn eat(xa: Option[R]) -> i64 { match xa { Option.Some(ra) => { ra.id }, Option.None => { 0 } } }
fn give() -> Option[R] { let (_, ob) = (mk(56), Option.Some(mk(156))); return ob }

fn bind()   { let (_, oc) = (mk(32), Option.Some(mk(132))); println("  b") }
fn bindsib(){ let (ad, bd) = (mk(48), Option.Some(mk(148))); println(f"  rd{ad.id}") }
fn slot0()  { let (oe, ke) = (Option.Some(mk(72)), 5); println(f"  k{ke}") }
fn nested() { let ((_, of), nf) = ((mk(2), Option.Some(mk(102))), 3); println(f"  n{nf}") }
fn nestpl() { let tg = ((mk(6), Option.Some(mk(106))), 7); let ((_, og), ng) = tg; println(f"  p{ng}") }
fn loopb()  { let mut i = 0; while i < 2 { let (_, oh) = (mk(64), Option.Some(mk(164))); i = i + 1; } println("  l") }
fn consume(){ let (_, oi) = (mk(52), Option.Some(mk(152))); match oi { Option.Some(ri) => { println(f"  c{ri.id}") }, Option.None => { println("  z") } } }
fn moved()  { let (_, oj) = (mk(54), Option.Some(mk(154))); let qj = oj; println(f"  m{qj.is_some()}") }
fn ret()    { let zk = give(); println(f"  r{zk.is_some()}") }
fn arg()    { let (_, ol) = (mk(58), Option.Some(mk(158))); println(f"  a{eat(ol)}") }
fn field()  { let (_, om) = (mk(76), Option.Some(mk(176))); let wm = W { p: om }; println(f"  f{wm.p.is_some()}") }
fn instr()  { let (_, on) = (mk(60), Option.Some(f"q60")); println("  s") }
fn none()   { let np: Option[R] = Option.None; let (_, op) = (mk(62), np); println("  o") }
fn plainel(){ let (_, oq) = (mk(9), mk(109)); println("  q") }
fn wildcd() { let (_, _) = (mk(31), Option.Some(mk(131))); println("  w") }

fn main() {
  println("bind");    bind()
  println("bindsib"); bindsib()
  println("slot0");   slot0()
  println("nested");  nested()
  println("nestpl");  nestpl()
  println("loopb");   loopb()
  println("consume"); consume()
  println("moved");   moved()
  println("ret");     ret()
  println("arg");     arg()
  println("field");   field()
  println("instr");   instr()
  println("none");    none()
  println("plainel"); plainel()
  println("wildcd");  wildcd()
  println("done")
}
"#,
        &[
            "bind",
            "dR32:s32:2",
            "dR132:s132:2",
            "  b",
            "bindsib",
            "dR148:s148:2",
            "  rd48",
            "dR48:s48:2",
            "slot0",
            "dR72:s72:2",
            "  k5",
            "nested",
            "dR2:s2:2",
            "dR102:s102:2",
            "  n3",
            "nestpl",
            "dR6:s6:2",
            "dR106:s106:2",
            "  p7",
            "loopb",
            "dR64:s64:2",
            "dR164:s164:2",
            "dR64:s64:2",
            "dR164:s164:2",
            "  l",
            "consume",
            "dR52:s52:2",
            "  c152",
            "dR152:s152:2",
            "moved",
            "dR54:s54:2",
            "  mtrue",
            "dR154:s154:2",
            "ret",
            "dR56:s56:2",
            "  rtrue",
            "dR156:s156:2",
            "arg",
            "dR58:s58:2",
            "  a158",
            "dR158:s158:2",
            "field",
            "dR76:s76:2",
            "  ftrue",
            "dR176:s176:2",
            "instr",
            "dR60:s60:2",
            "  s",
            "none",
            "dR62:s62:2",
            "  o",
            "plainel",
            "dR9:s9:2",
            "dR109:s109:2",
            "  q",
            "wildcd",
            "dR31:s31:2",
            "dR131:s131:2",
            "  w",
            "done",
        ],
        "b9-fresh-option-binding-leaf",
    );
}

/// B-2026-09-06-51 — a fresh tuple literal's `Option` / `Result` element
/// whose payload runs NO user `Drop`.
///
/// Every owner the sibling fixtures exercise is reached through a payload
/// WALKER: the arm is entered on `emit_optres_payload_user_drop_bodies_fn`
/// returning something, and both of its memory registrations then ask
/// `option_payload_struct_or_enum_drop_ok`. So a payload that merely CARRIES
/// heap — a bare `String`, a `Vec`, or a struct with a heap field and no
/// `impl Drop` — got no owner at all and leaked outright. The BODIES
/// question standing in for the MEMORY one, which is the same predicate
/// mistake `tests/asan-o0-known-failures.txt` records for B-2026-09-01-25.
///
/// Localized by two spellings of the SAME value that were always clean and
/// stay so: the struct-FIELD destructure (`let W { p } = w`) and the NAMED
/// tuple (`let t = (..); let (a, o) = t`). Only the literal destructured in
/// place is affected — bound or wildcarded, either slot, both heads.
///
/// `ovec` / `vwild` CARRY A SECOND ROOT CAUSE and are not redundant with
/// `ostr` / `owild`: a bare `[4, 4, 4]` reaches
/// `refined_tuple_literal_elem_te` as a `PrefixCollectionLiteral`, which had
/// no arm, so the element came back as a bare `Option` and the ownership
/// predicate declined it on a missing generic argument rather than on its
/// payload. The two spellings that already carried the type —
/// `let xs = [..]; Option.Some(xs)` and `Option[Vec[i64]].Some([..])` — were
/// clean throughout, which is what separated the erasure from the predicate.
///
/// `omove` / `oarm` / `rarm` PULL THE OTHER WAY and are why the owner is the
/// one `track_owned_destructure_field_cleanup` already installs for the
/// struct-field spelling rather than a fresh registration: its actions join
/// `inline_option_payload_vars` / `inline_result_payload_vars`, the sets a
/// whole-value move and a consuming arm retract. An owner outside those sets
/// balances every leaf that stays put and double-frees the one that moves.
///
/// `odrop` is the user-`Drop` control (unchanged, one body), and `loops`
/// runs two iterations so a per-iteration imbalance shows as a multiple.
///
/// MEASURED at the parent: 71 allocs / 62 frees, 56 bytes definitely lost
/// in 9 blocks from 8 contexts, against 71/71 and zero errors after — with
/// **byte-identical stdout both ways**, so no output oracle on any of the
/// four surfaces could have caught it and the balance is the whole pin.
#[test]
fn asan_fresh_tuple_optres_leaf_with_a_bodyless_heap_payload_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(n: i64) -> R { return R { id: n, s: f"s{n}" }; }
struct Carrier { s: String }

fn ostr()   { let (r, o) = (mk(1), Option.Some(f"p1")); println(f"  a{r.id}{o.is_some()}") }
fn owild()  { let (r, _) = (mk(2), Option.Some(f"p2")); println(f"  b{r.id}") }
fn rstr()   { let (r, o) = (mk(3), Result[String, i64].Ok(f"p3")); println(f"  c{r.id}{o.is_ok()}") }
fn ovec()   { let (r, o) = (mk(4), Option.Some([4, 4, 4])); println(f"  d{r.id}{o.is_some()}") }
fn vwild()  { let (r, _) = (mk(5), Option.Some([5, 5])); println(f"  e{r.id}") }
fn ocarry() { let (r, o) = (mk(6), Option.Some(Carrier { s: f"p6" })); println(f"  f{r.id}{o.is_some()}") }
fn omove()  { let (r, o) = (mk(7), Option.Some(f"p7")); let q = o; println(f"  g{r.id}{q.is_some()}") }
fn oarm()   { let (r, o) = (mk(8), Option.Some(f"p8")); match o { Option.Some(s) => println(f"  h{s}"), Option.None => println("  hz") } println(f"  i{r.id}") }
fn rarm()   { let (r, o) = (mk(9), Result[String, i64].Ok(f"p9")); match o { Result.Ok(s) => println(f"  j{s}"), Result.Err(e) => println(f"  jz{e}") } println(f"  k{r.id}") }
fn odrop()  { let (r, o) = (mk(10), Option.Some(mk(110))); println(f"  l{r.id}{o.is_some()}") }
fn loops()  { let mut i = 0; while i < 2 { let (r, o) = (mk(11), Option.Some(f"p11")); println(f"  m{r.id}{o.is_some()}"); i = i + 1; } println("  n") }

fn main() {
  println("ostr");   ostr()
  println("owild");  owild()
  println("rstr");   rstr()
  println("ovec");   ovec()
  println("vwild");  vwild()
  println("ocarry"); ocarry()
  println("omove");  omove()
  println("oarm");   oarm()
  println("rarm");   rarm()
  println("odrop");  odrop()
  println("loops");  loops()
  println("done")
}
"#,
        &[
            "ostr",
            "  a1true",
            "dR1",
            "owild",
            "  b2",
            "dR2",
            "rstr",
            "  c3true",
            "dR3",
            "ovec",
            "  d4true",
            "dR4",
            "vwild",
            "  e5",
            "dR5",
            "ocarry",
            "  f6true",
            "dR6",
            "omove",
            "  g7true",
            "dR7",
            "oarm",
            "  hp8",
            "  i8",
            "dR8",
            "rarm",
            "  jp9",
            "  k9",
            "dR9",
            "odrop",
            "  l10true",
            "dR110",
            "dR10",
            "loops",
            "  m11true",
            "dR11",
            "  m11true",
            "dR11",
            "  n",
            "done",
        ],
        "b51-bodyless-heap-payload-leaf",
    );
}

/// B-2026-09-03-39 — THE FRESH-SOURCE TWIN OF THE ARM ABOVE MUST STAY
/// BALANCED WHILE IT GAINS OWNERS.
///
/// A tuple LITERAL destructured in place (`let (r, _) = (mk(31),
/// Option.Some(mk(131)));`) lost the payload's `Drop` body on all three
/// compiled surfaces, on BOTH leaf kinds and for `Option` and `Result`
/// alike, because `infer_arg_elem_te` rebuilds an enum-constructor
/// element's type from its NAME and drops the generic argument — so the
/// leaf walkers, which key the payload off exactly that argument, were
/// handed a bare `Option`.
///
/// WHY IT IS HERE AND NOT ONLY IN THE TRANSCRIPT PINS, and why it is a
/// SEPARATE case from `b25-wildcard-optres-leaf` rather than more cells in
/// it: the two halves of the fix take DIFFERENT ownership positions, and
/// each has its own way to be wrong.
/// - The WILDCARD leaf reaches `run_discarded_leaf_user_drop_bodies` with
///   `free_memory: true`, so once the element is typed it takes the MEMORY
///   as well — the opposite of the place-source arm above, which takes the
///   body only. A fresh literal has no source aggregate to fall back on, so
///   this is the leaf becoming the sole owner where previously there was
///   none. LSan is what says whether "none" meant a leak or an owner
///   elsewhere that now double-frees.
/// - The BINDING leaf is NOT fixed and NOT pinned here, and this suite is
///   why. Registering the payload walk beside the memory owner
///   `track_destructure_leaf_cleanup` installed leaks 76 bytes per
///   occurrence — arming a `ContainerElemBodies` action stops whoever was
///   freeing the boxed payload — and adding
///   `track_inline_option_agg_payload_var` to compensate balances every
///   leaf that stays put while DOUBLE-FREEING the one that moves. Both
///   measurements are on the follow-up row; the transcript pins alone said
///   the fix worked.
///
/// `frres` IS THE CELL B-2026-09-03-15 WOULD PREDICT TO LEAK. It declined
/// the `Result` leaf on its own arm precisely because
/// `track_inline_option_agg_payload_var` has no `Result` peer, and taking
/// it there leaked 272 bytes. It is safe here for the same reason `resw` is
/// safe above — the memory question is answered by a different owner — but
/// it is the first cell to check if this ever regresses.
///
/// `frloop` RUNS THREE ITERATIONS, so a per-iteration leak shows as a
/// multiple rather than as one block that could be mistaken for a fixture
/// artifact.
///
/// `frstr` and `frplain` ARE THE CONTROLS: an inline `Option[String]`
/// payload with no user `Drop` (nothing owed, and it was clean before the
/// fix — which is what isolates the boxed payload rather than the typing
/// change itself), and a plain struct element, which proves the leaf arm
/// was always reached and that the refinement did not disturb it.
///
/// Every body renders `s` and `xs.len()`, so a body run against a
/// cap-zeroed husk prints `dR131::0` and fails the transcript instead of
/// passing as a bare count.
#[test]
fn asan_fresh_tuple_source_optres_leaf_owns_its_payload_body() {
    assert_clean_asan_run(
            "struct R { id: i64, s: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}:{self.s}:{self.xs.len()}\") } }\n\
             fn mk(n: i64) -> R { return R { id: n, s: f\"s{n}\", xs: [n, n] }; }\n\
             fn frw() { let (r, _) = (mk(31), Option.Some(mk(131))); println(f\"rd{r.id}\") }\n\
             fn frres() { let (r, _) = (mk(35), Result[R, String].Ok(mk(135))); println(f\"rr{r.id}\") }\n\
             fn frw0() { let (_, r) = (Option.Some(mk(41)), mk(141)); println(f\"w0{r.id}\") }\n\
             fn frnest() { let ((_, _), n) = ((mk(43), Option.Some(mk(143))), 4); println(f\"nn{n}\") }\n\
             fn frloop() { let mut i = 0; while i < 3 { let (_, _) = (mk(44), Option.Some(mk(144))); i = i + 1; } println(\"fl\") }\n\
             fn frstr() { let (r, _) = (mk(45), Option.Some(f\"x45\")); println(f\"fs{r.id}\") }\n\
             fn frplain() { let (r, _) = (mk(46), mk(146)); println(f\"fp{r.id}\") }\n\
             fn main() { frw(); frres(); frw0(); frnest(); frloop(); frstr(); frplain(); }\n",
            &[
                "dR131:s131:2",
                "rd31",
                "dR31:s31:2",
                "dR135:s135:2",
                "rr35",
                "dR35:s35:2",
                "dR41:s41:2",
                "w0141",
                "dR141:s141:2",
                "dR43:s43:2",
                "dR143:s143:2",
                "nn4",
                "dR44:s44:2",
                "dR144:s144:2",
                "dR44:s44:2",
                "dR144:s144:2",
                "dR44:s44:2",
                "dR144:s144:2",
                "fl",
                "fs45",
                "dR45:s45:2",
                "dR146:s146:2",
                "fp46",
                "dR46:s46:2",
            ],
            "b39-fresh-optres-leaf",
        );
}

/// B-2026-09-06-53 — the MEMORY half of `tests/codegen.rs`'s
/// `e2e_scalar_argument_does_not_make_the_result_a_view` under ASAN + LSan.
/// The row was a lost BODY with balanced memory, so this pins that giving
/// the result binding its own ownership back does not disturb that: one
/// owner and one free per object on every cell.
#[test]
fn asan_scalar_argument_result_keeps_one_owner() {
    assert_clean_asan_run(
            "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             struct H { r: R, n: i64 }\n\
             fn mkUses(i: i64) -> R { return R { id: i, name: f\"h{i}\" }; }\n\
             fn mkIgnores(i: i64) -> R { return R { id: 0, name: \"z\" }; }\n\
             fn mkName(i: i64) -> R { return R { id: 7, name: f\"n{i}\" }; }\n\
             fn mkOpt(i: i64) -> Option[R] { return Option.Some(R { id: i, name: f\"o{i}\" }); }\n\
             fn mkFlt(x: f64) -> R { return R { id: 20, name: f\"f{x}\" }; }\n\
             fn mkChr(c: char) -> R { return R { id: 21, name: f\"c{c}\" }; }\n\
             fn keep(r: R) -> R { return r; }\n\
             fn wrap(r: R) -> H { return H { r: r, n: 1 }; }\n\
             fn both(i: i64, r: R) -> R { return r; }\n\
             impl H { fn make(i: i64) -> R { return R { id: i, name: f\"a{i}\" }; } }\n\
             \n\
             fn scalar_arg(i: i64) { let x = mkUses(i); println(f\"  v={x.id}\"); }\n\
             fn scalar_rebound(i: i64) { let j = i; let x = mkUses(j); println(f\"  v={x.id}\"); }\n\
             fn scalar_chained(i: i64) { let x = mkUses(i); let y = keep(x); println(f\"  v={y.id}\"); }\n\
             fn scalar_unused(i: i64) { let x = mkIgnores(i); println(f\"  v={x.id}\"); }\n\
             fn scalar_interpolated(i: i64) { let x = mkName(i); println(f\"  v={x.id}\"); }\n\
             fn scalar_constant(i: i64) { let x = mkUses(9); println(f\"  v={x.id}\"); }\n\
             fn scalar_arith(i: i64) { let x = mkUses(i + 0); println(f\"  v={x.id}\"); }\n\
             fn scalar_option(i: i64) { let o = mkOpt(i); match o { Option.Some(r) => { println(f\"  v={r.id}\"); } Option.None => { println(\"  v=none\"); } } }\n\
             fn scalar_float(x: f64) { let r = mkFlt(x); println(f\"  v={r.id}\"); }\n\
             fn scalar_char(c: char) { let r = mkChr(c); println(f\"  v={r.id}\"); }\n\
             fn scalar_assoc(i: i64) { let x = H.make(i); println(f\"  v={x.id}\"); }\n\
             fn owned_handback(r: R) { let x = keep(r); println(f\"  v={x.id}\"); }\n\
             fn owned_wrapped(r: R) { let x = wrap(r); println(f\"  v={x.r.id}\"); }\n\
             fn mixed_args(i: i64, r: R) { let x = both(i, r); println(f\"  v={x.id}\"); }\n\
             fn local_scalar() { let n = 31; let x = mkUses(n); println(f\"  v={x.id}\"); }\n\
             \n\
             fn main() {\n\
             \x20   println(\"scalar_arg\"); scalar_arg(1);\n\
             \x20   println(\"scalar_rebound\"); scalar_rebound(2);\n\
             \x20   println(\"scalar_chained\"); scalar_chained(3);\n\
             \x20   println(\"scalar_unused\"); scalar_unused(4);\n\
             \x20   println(\"scalar_interpolated\"); scalar_interpolated(5);\n\
             \x20   println(\"scalar_constant\"); scalar_constant(6);\n\
             \x20   println(\"scalar_arith\"); scalar_arith(8);\n\
             \x20   println(\"scalar_option\"); scalar_option(10);\n\
             \x20   println(\"scalar_float\"); scalar_float(1.5);\n\
             \x20   println(\"scalar_char\"); scalar_char('q');\n\
             \x20   println(\"scalar_assoc\"); scalar_assoc(11);\n\
             \x20   println(\"owned_handback\"); owned_handback(mkUses(12));\n\
             \x20   println(\"owned_wrapped\"); owned_wrapped(mkUses(13));\n\
             \x20   println(\"mixed_args\"); mixed_args(14, mkUses(15));\n\
             \x20   println(\"local_scalar\"); local_scalar();\n\
             \x20   println(\"end\");\n\
             }\n",
            &[
                "scalar_arg",
                "  v=1",
                "  dR1",
                "scalar_rebound",
                "  v=2",
                "  dR2",
                "scalar_chained",
                "  v=3",
                "  dR3",
                "scalar_unused",
                "  v=0",
                "  dR0",
                "scalar_interpolated",
                "  v=7",
                "  dR7",
                "scalar_constant",
                "  v=9",
                "  dR9",
                "scalar_arith",
                "  v=8",
                "  dR8",
                "scalar_option",
                "  v=10",
                "  dR10",
                "scalar_float",
                "  v=20",
                "  dR20",
                "scalar_char",
                "  v=21",
                "  dR21",
                "scalar_assoc",
                "  v=11",
                "  dR11",
                "owned_handback",
                "  v=12",
                "  dR12",
                "owned_wrapped",
                "  v=13",
                "  dR13",
                "mixed_args",
                "  v=15",
                "  dR15",
                "local_scalar",
                "  v=31",
                "  dR31",
                "end"
            ],
            "scalar_argument_result_one_owner",
        );
}

/// B-2026-09-06-58 — the MEMORY half of `tests/codegen.rs`'s
/// `e2e_drop_free_argument_does_not_make_the_result_a_view` under ASAN +
/// LSan. Declining the view gives the result binding its own ownership, so
/// this pins that the argument's buffer still has exactly one owner: the
/// `String`, `Vec[String]` and `Drop`-free-struct arguments, the `Option`
/// return and the two-parameter call all keep one free per object.
#[test]
fn asan_drop_free_argument_result_keeps_one_owner() {
    assert_clean_asan_run(
            "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             struct H { r: R, n: i64 }\n\
             impl Drop for H { fn drop(mut ref self) { println(f\"  dH{self.n}\") } }\n\
             struct Q { s: String, n: i64 }\n\
             struct W { r: R, n: i64 }\n\
             struct N { r: R, n: i64 }\n\
             \n\
             fn from_string(i: i64, s: String) -> R { return R { id: i, name: s }; }\n\
             fn from_vec(i: i64, v: Vec[String]) -> R { return R { id: i, name: v[0] }; }\n\
             fn from_struct(q: Q) -> R { return R { id: q.n, name: q.s }; }\n\
             fn from_two(a: String, b: String) -> R { return R { id: 30, name: a }; }\n\
             fn into_option(s: String) -> Option[R] { return Option.Some(R { id: 40, name: s }); }\n\
             fn into_bodyless(s: String) -> N { return N { r: R { id: 50, name: s }, n: 1 }; }\n\
             fn hand_back(r: R) -> R { return r; }\n\
             fn wrap_bodyless(r: R) -> W { return W { r: r, n: 2 }; }\n\
             fn wrap_bodied(r: R) -> H { return H { r: r, n: 3 }; }\n\
             fn from_scalar(i: i64) -> R { return R { id: i, name: f\"s{i}\" }; }\n\
             fn no_store(i: i64, v: Vec[i64]) -> R { return R { id: i, name: f\"n{v.len()}\" }; }\n\
             \n\
             fn string_arg(i: i64, nm: String) { let x = from_string(i, nm); println(f\"  v={x.id}\"); }\n\
             fn vec_arg(i: i64, v: Vec[String]) { let x = from_vec(i, v); println(f\"  v={x.id}\"); }\n\
             fn struct_arg(q: Q) { let x = from_struct(q); println(f\"  v={x.id}\"); }\n\
             fn two_string_args(a: String, b: String) { let x = from_two(a, b); println(f\"  v={x.id}\"); }\n\
             fn option_return(s: String) { let o = into_option(s); match o { Option.Some(r) => { println(f\"  v={r.id}\"); } Option.None => { println(\"  v=none\"); } } }\n\
             fn bodyless_return(s: String) { let n = into_bodyless(s); println(f\"  v={n.r.id}\"); }\n\
             fn chained(s: String) { let x = from_string(60, s); let y = hand_back(x); println(f\"  v={y.id}\"); }\n\
             fn same_type(r: R) { let y = hand_back(r); println(f\"  v={y.id}\"); }\n\
             fn wrap_control(r: R) { let w = wrap_bodyless(r); println(f\"  v={w.r.id}\"); }\n\
             fn wrap_bodied_control(r: R) { let h = wrap_bodied(r); println(f\"  v={h.r.id}\"); }\n\
             fn scalar_control(i: i64) { let x = from_scalar(i); println(f\"  v={x.id}\"); }\n\
             fn read_only_arg(i: i64, v: Vec[i64]) { let x = no_store(i, v); println(f\"  v={x.id}\"); }\n\
             \n\
             fn main() {\n\
             \x20   println(\"string_arg\"); string_arg(1, \"a\");\n\
             \x20   println(\"vec_arg\"); vec_arg(2, [\"b\"]);\n\
             \x20   println(\"struct_arg\"); struct_arg(Q { s: \"c\", n: 3 });\n\
             \x20   println(\"two_string_args\"); two_string_args(\"d\", \"e\");\n\
             \x20   println(\"option_return\"); option_return(\"f\");\n\
             \x20   println(\"bodyless_return\"); bodyless_return(\"g\");\n\
             \x20   println(\"chained\"); chained(\"h\");\n\
             \x20   println(\"same_type\"); same_type(R { id: 7, name: \"i\" });\n\
             \x20   println(\"wrap_control\"); wrap_control(R { id: 8, name: \"j\" });\n\
             \x20   println(\"wrap_bodied_control\"); wrap_bodied_control(R { id: 9, name: \"k\" });\n\
             \x20   println(\"scalar_control\"); scalar_control(10);\n\
             \x20   println(\"read_only_arg\"); read_only_arg(11, [1, 2]);\n\
             \x20   println(\"end\");\n\
             }\n",
            &[
                "string_arg",
                "  v=1",
                "  dR1",
                "vec_arg",
                "  v=2",
                "  dR2",
                "struct_arg",
                "  v=3",
                "  dR3",
                "two_string_args",
                "  v=30",
                "  dR30",
                "option_return",
                "  v=40",
                "  dR40",
                "bodyless_return",
                "  v=50",
                "  dR50",
                "chained",
                "  v=60",
                "  dR60",
                "same_type",
                "  v=7",
                "  dR7",
                "wrap_control",
                "  v=8",
                "  dR8",
                "wrap_bodied_control",
                "  v=9",
                // B-2026-09-06-63 — the wrapper's OWN body, which this cell
                // pinned as MISSING while that row was open.
                "  dH3",
                "  dR9",
                "scalar_control",
                "  v=10",
                "  dR10",
                "read_only_arg",
                "  v=11",
                "  dR11",
                "end"
            ],
            "drop_free_argument_result_one_owner",
        );
}

#[test]
/// B-2026-09-06-70 — the FREEING half of
/// `test_e2e_method_and_assoc_arg_registrars_admit_only_when_the_result_owns_it`.
///
/// That test asserts the output, which on the parent was already correct at
/// `-O2` for most of these cells: LLVM inlines the callee and the double
/// free becomes a surviving use-after-free. Only a sanitizer separates
/// "prints the right lines" from "owns its memory once". Measured here 0
/// valgrind errors after the fix, against `free(): double free detected in
/// tcache 2` and 3 errors from 3 contexts per red cell on the parent — plus
/// `malloc(): unaligned tcache chunk detected` for the struct-literal cell,
/// which is heap corruption rather than a detected double free.
///
/// DELIBERATELY OMITS the E2E fixture's `k`/`l` and loop cells, which are
/// controls for the output count rather than for ownership, and its tuple
/// sibling `h.thrut2((mk(35), 9))`: that one goes from a double free to
/// correct output with a 16-byte residual — the `shared` handle's refcount
/// block, B-2026-09-06-72's class, whose no-method twin leaks the same 16
/// bytes on the parent. Including it would make this fixture red for
/// someone else's bug.
fn asan_method_and_assoc_arg_registrars_admit_only_when_the_result_owns_it() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
struct P { id: i64, name: String }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.id}") } }
fn mkP(i: i64) -> P { return P { id: i, name: f"p{i}" }; }
struct Hold { n: i64 }
impl R {
  fn passa(r: R) -> R { return r; }
  fn mka() -> R { return mk(31); }
}
impl P { fn passp(p: P) -> P { return p; } }
impl Hold {
  fn thru(ref self, r: R) -> R { return r; }
  fn reb(ref self, r: R) -> R { let m = r; return m; }
  fn thrup(ref self, p: P) -> P { return p; }
}
fn main() {
  let h = Hold { n: 0 };
  let a = R.passa(mk(16)); println(f"a={a.inner.v}");
  let b = h.thru(mk(17)); println(f"b={b.inner.v}");
  let c = h.reb(mk(18)); println(f"c={c.inner.v}");
  let d = h.thru(R { id: 19, name: "h19", inner: Inner { v: 19 } }); println(f"d={d.inner.v}");
  h.thru(mk(20));
  R.passa(mk(21));
  R.mka();
  let e = h.thrup(mkP(23)); println(f"e={e.name}");
  let g = P.passp(mkP(24)); println(f"g={g.name}");
  println("end");
}
"#,
        &[
            "a=16", "dR16", "b=17", "dR17", "c=18", "dR18", "d=19", "dR19", "dR20", "dR21", "dR31",
            "e=p23", "dP23", "g=p24", "dP24", "end",
        ],
        "b0906-70-method-assoc-admission",
        30,
    );
}

/// B-2026-09-01-29 (the `ref` half) — a fresh `Option`/`Result` temp handed
/// to a `ref` parameter had NO owner at all: the callee borrows, the temp
/// has no binding, and `Option`'s type-erased layout carries no droppable
/// payload for the enum arm of `queue_ref_rvalue_arg_cleanup` to find. The
/// row measured 333 B in 6 allocations over a three-iteration loop — one
/// `Vec` buffer plus its `String` per iteration.
///
/// The `ref` case is the one place ownership needs no escape analysis: a
/// borrow cannot take the value, so a temp nothing else names has exactly
/// one possible owner. That is why the fix is a registration rather than
/// a predicate change, and why it cannot double-free the way the by-value
/// half's widening could (B-2026-09-01-35).
///
/// FIVE LEGS PER ITERATION, four that leaked and one control:
///   - `temp` / `moved` — `peek(Some(mkv(i)))` against a free fn.
///   - `res` — the `Result` sibling, `peekr(Ok(...))`.
///   - `meth` — the METHOD spelling, whose call site is a second
///     `queue_ref_rvalue_arg_cleanup` caller and threads the parameter
///     index through the receiver offset.
///   - `named` — THE CONTROL. `let o = Some(mkv(i)); peek(o)` was already
///     clean, because the binding owns it; if the new registration ever
///     stops asking whether the argument is an unowned temp, this leg is
///     the double free that says so.
///
/// The callee DESTRUCTURES in every leg (`match x { Some(v) => v.len() }`),
/// which is deliberate: reading through the borrow is the shape where the
/// caller most plausibly believes the callee took over.
#[test]
fn asan_ref_optres_temp_arg_has_exactly_one_owner() {
    let Some((out, status)) = run_under_asan(
        r#"struct H { n: i64 }
impl H {
    fn peek(ref self, x: ref Option[Vec[String]]) -> i64 {
        match x { Some(v) => { return self.n + v.len() } None => { return self.n } }
    }
}

fn mkv(n: i64) -> Vec[String] {
    let mut vs: Vec[String] = Vec.new();
    vs.push("abcdefghijklmnopqrstuvwxyz0123456789");
    vs.push(f"tag{n}");
    return vs
}

fn peek(x: ref Option[Vec[String]]) -> i64 {
    match x { Some(v) => { return v.len() } None => { return 0 } }
}

fn peekr(x: ref Result[Vec[String], i64]) -> i64 {
    match x { Ok(v) => { return v.len() } Err(e) => { return e } }
}

fn main() {
    let h = H { n: 100 };
    let mut i = 0;
    while i < 3 {
        println(f"temp {peek(Some(mkv(i)))}");
        println(f"moved {peek(Some(mkv(i)))}");
        println(f"res {peekr(Ok(mkv(i)))}");
        println(f"meth {h.peek(Some(mkv(i)))}");
        let o: Option[Vec[String]] = Some(mkv(i));
        println(f"named {peek(o)}");
        i = i + 1;
    }
    println("end");
}
"#,
        "asan_ref_optres_temp_arg_has_exactly_one_owner",
    ) else {
        return;
    };
    assert!(status.success(), "ASAN/LSan reported a problem:\n{out}");
    assert_eq!(
        out,
        "temp 2\nmoved 2\nres 2\nmeth 102\nnamed 2\n\
             temp 2\nmoved 2\nres 2\nmeth 102\nnamed 2\n\
             temp 2\nmoved 2\nres 2\nmeth 102\nnamed 2\nend\n",
        "unexpected transcript:\n{out}"
    );
}

/// B-2026-08-28-58 — the memory half of running an own-`Drop` enum's body
/// when it is an `Option`/`Result` PAYLOAD.
///
/// This fixture carries more weight than its siblings, because the fix
/// touched the MEMORY channel and not just a bodies walker.
/// `emit_drop_fn_for_type_expr` used to hand a Drop-bearing enum's
/// user-drop WRAPPER to every memory-side caller; it now routes to
/// `emit_enum_drop_switch` (memory-only) instead, with an
/// `emit_primitive_drop_fn("<E>$mem")` fallback when there is nothing to
/// free. If that reroute lost the free, the `String` rows below leak under
/// LSan; if the bodies walker also freed, they double-free. Passing both
/// ways is the balance claim, and the enum arm of `karac_drop_<E>` is
/// where the heap actually lives.
///
/// `mem-fallback-no-heap` is the second half of that reroute — a
/// Drop-bearing enum with NO heap at all, where `emit_enum_drop_switch`
/// returns `None`. The fallback must not resolve back to the wrapper (the
/// B-2026-08-03-10 trap, whose struct twin needed the same suffix), which
/// would run the body a second time at scope exit and show up here as a
/// doubled line.
///
/// DELIBERATELY ABSENT: `Option[K]` where `K`'s variant payload is a
/// heap-carrying nested STRUCT. That shape leaks the struct's buffer, for
/// a reason that predates this fix and is independent of it — the plain
/// `let` site registers `track_inline_result_payload_var` but has no
/// `Option` sibling for a struct/enum payload, so `Result[K, i64]` frees
/// it and `Option[K]` does not. It went unseen because nothing observed
/// the buffer, and an unread `malloc` is removed outright; making the
/// body run is what turns it into a reportable leak. Filed on its own row
/// with the scoping table (only that one cell of seven leaks). Every row
/// below is a shape measured balanced.
#[test]
fn asan_own_drop_enum_as_optres_payload_is_memory_balanced() {
    const H: &str = "enum E { A(String), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n";
    // OPTION, payload variant — the enum owns a heap `String` its own body
    // reads. The agree-on-zero shape before the fix.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let o: Option[E] = Some(E.A(f\"n{{7}}\"));\n\
             \x20            println(\"mid\"); }}\n"
        ),
        &["drop E", "mid"],
        "option-payload-heap",
    );
    // RESULT — the leg that ran the body from the MEMORY channel at scope
    // exit before the fix. If the reroute is wrong, this is where a double
    // free or a leak lands.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let r: Result[E, i64] = Ok(E.A(f\"n{{7}}\"));\n\
             \x20            println(\"mid\"); }}\n"
        ),
        &["drop E", "mid"],
        "result-payload-heap",
    );
    // ERR position — the second arm of the same switch.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let r: Result[i64, E] = Err(E.A(f\"n{{7}}\"));\n\
             \x20            println(\"mid\"); }}\n"
        ),
        &["drop E", "mid"],
        "result-err-heap",
    );
    // $mem FALLBACK — a Drop-bearing enum with no heap. `emit_enum_drop_
    // switch` gives `None` here, so the fallback name is what stops the
    // module lookup from handing back the wrapper.
    assert_clean_asan_run(
        "enum E3 { A(i64), B }\n\
             impl Drop for E3 { fn drop(mut ref self) { println(\"drop E3\") } }\n\
             fn main() { let r: Result[E3, i64] = Ok(E3.A(3)); println(\"mid\"); }\n",
        &["drop E3", "mid"],
        "mem-fallback-no-heap",
    );
    // NESTED-STRUCT payload with NO heap — the -54 predicate reaching this
    // position through a struct rather than a direct `String`. Balanced
    // because there is nothing to free; the heap-carrying sibling is the
    // shape excluded above.
    assert_clean_asan_run(
        "enum H2 { A(R2), B }\n\
             struct R2 { id: i64 }\n\
             impl Drop for R2 { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n\
             fn main() { let o: Option[H2] = Some(H2.A(R2 { id: 5 })); println(\"mid\"); }\n",
        &["drop R5", "mid"],
        "payload-only-enum-no-heap",
    );
    // RESULT with the heap-carrying nested struct — the `Option` sibling
    // of this row is the excluded leak, so pinning the `Result` half is
    // what proves the fix itself is memory-neutral: same body, same heap,
    // same walker, and the registrar that exists here frees it.
    assert_clean_asan_run(
        "enum K2 { A(R3), B }\n\
             struct R3 { id: i64, name: String }\n\
             impl Drop for R3 { fn drop(mut ref self) { println(f\"drop R{self.name}\") } }\n\
             fn main() { let r: Result[K2, i64] = Ok(K2.A(R3 { id: 5, name: f\"n{5}\" }));\n\
             \x20            println(\"mid\"); }\n",
        &["drop Rn5", "mid"],
        "result-nested-struct-heap",
    );
    // MOVED OUT by a consuming arm — the destination owns the payload, so
    // the source's walk must stay retracted and the BINDING's registration
    // is the only one that fires. A per-owner rather than per-value
    // widening double-frees here.
    //
    // The expectation was `got` alone when written, because the arm
    // binding ran no body at all; B-2026-08-28-63 gave it one. The memory
    // claim is untouched — one body, one free.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let o: Option[E] = Some(E.A(f\"n{{7}}\"));\n\
             \x20            match o {{ Some(e) => {{ println(\"got\") }}\n\
             \x20                       None => {{ println(\"none\") }} }} }}\n"
        ),
        &["got", "drop E"],
        "consuming-match-moved-out",
    );
}

#[test]
fn asan_weak_read_into_option_binding_then_weak_store_no_leak() {
    // B-2026-07-21-21: `let after: Option[N] = nodes[i].link;` (a WEAK-field
    // read) binds an `Option[shared]` that owns NO +1 — the weak read is a
    // borrow. But the binding queued a scope-exit `RcDecOption`; without a
    // matching balancing inner rc-inc (the "case (d)" aliasing acquire), that
    // dec was UNBALANCED, so storing `after` back into another `weak` field
    // (`nodes[j].link = after`, the insertion-sort splice from kata #147)
    // over-released the popped node — a leak for one splice, use-after-free /
    // double-free across several. The fix acquires the inner ref when a
    // weak-field read initializes a tracked `Option[shared]` binding. This is
    // the exact leetcode-#147 shape (build-a-chain, save-successor, re-link).
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, id: i64, mut next: weak Node }
fn main() {
    let mut nodes: Vec[Node] = Vec.new();
    nodes.push(Node { val: 1i64, id: 0i64, next: None });
    nodes.push(Node { val: 2i64, id: 1i64, next: None });
    nodes.push(Node { val: 3i64, id: 2i64, next: None });
    nodes[0].next = nodes[1];
    nodes[1].next = nodes[2];
    let a: Option[Node] = nodes[0].next;
    nodes[2].next = a;
    let b: Option[Node] = nodes[1].next;
    nodes[2].next = b;
    println("ok");
}
"#,
        &["ok"],
        "asan_weak_read_into_option_binding_then_weak_store_no_leak",
    );
}

#[test]
fn asan_freshtemp_result_err_struct_field_freefn_no_double_free() {
    // B-2026-07-23-4 Err-side + USER free-fn sibling: `Err(e) => use_s(e.msg)`
    // where `fn use_s(x: String)` consumes (frees) its owned param. Same
    // heap-field-to-free-fn move-out on the `Err` variant, verifying the fix
    // is not `println`- or Ok-specific.
    assert_clean_asan_run(
        r#"
struct E { msg: String }
fn use_s(x: String) -> i64 { x.len() }
fn f() -> Result[i64, E] { Err(E { msg: "rejected".to_string() }) }
fn main() {
    let mut i: i64 = 0i64;
    let mut n: i64 = 0i64;
    while i < 40i64 {
        match f() {
            Ok(v) => { n = n + v; }
            Err(e) => { n = n + use_s(e.msg); }
        }
        i = i + 1;
    }
    println(n.to_string());
}
"#,
        // len("rejected") == 8; 40 * 8 = 320
        &["320"],
        "freshtemp_result_err_struct_field_freefn_no_double_free",
    );
}

#[test]
fn asan_freshtemp_result_struct_field_borrow_read_no_leak() {
    // B-2026-07-23-4 negative control: a BORROW-only read of the struct-
    // wrapper heap field (`Ok(w) => w.s.len()`, no by-value move to a free
    // fn) must KEEP the source payload drop armed — the buffer is freed by
    // nobody else, so suppressing it would LEAK. Confirms the fix's gate is
    // move-specific and did not over-suppress the borrow-only path.
    assert_clean_asan_run(
        r#"
struct W { s: String }
fn f() -> Result[W, i64] { Ok(W { s: "boom".to_string() }) }
fn main() {
    let mut i: i64 = 0i64;
    let mut n: i64 = 0i64;
    while i < 40i64 {
        match f() {
            Ok(w) => { n = n + w.s.len(); }
            Err(e) => { n = n + e; }
        }
        i = i + 1;
    }
    println(n.to_string());
}
"#,
        // len("boom") == 4; 40 * 4 = 160
        &["160"],
        "freshtemp_result_struct_field_borrow_read_no_leak",
    );
}

#[test]
fn asan_result_discard_struct_with_heap_no_leak() {
    // B-2026-07-12-2 gap 3 (general, NOT once-specific) — a discarded
    // fresh-temp `Result[i64, Rec]` whose `Err` payload is a multi-field
    // struct-with-heap. The seeded `Result` layout carries no drop kind and
    // the `{ptr,len,cap}` overlay only frees a single buffer at offset 0, so
    // the struct's inner `String` leaked; the `FreeInlineResultPayload`
    // struct-drop arm now frees it. `boom` is side-effecting (mutates a
    // module-less counter via a Vec push) so the discarded `Err` survives DCE.
    assert_clean_asan_run(
        r#"
struct Rec { id: i64, name: String }
fn boom(v: mut ref Vec[i64]) -> Result[i64, Rec] {
    v.push(1i64);
    Err(Rec { id: 2i64, name: "leakme".to_string() })
}
fn main() {
    let mut i: i64 = 0i64;
    let mut sink: Vec[i64] = Vec.new();
    while i < 40i64 {
        match boom(mut sink) { Ok(_) => {}, Err(_) => {}, }
        i = i + 1;
    }
    println(sink.len().to_string());
}
"#,
        &["40"],
        "result_discard_struct_with_heap_no_leak",
    );
}

#[test]
fn asan_consuming_call_result_shared_no_leak() {
    // B-2026-07-12-24 (residual, consuming-call leg): an owned `Result[shared]`
    // passed BY VALUE to a consuming fn (`eat(d)`) used to leak — neither the
    // caller (d escapes as an arg → unregistered) nor the callee (params were
    // not RC-tracked) released it. Result parameter RC-tracking, gated by the
    // same escape analysis, now makes the callee's in-place-consumed param own
    // the caller's transferred +1 and release it. A forwarded param would stay
    // unregistered (terminal consumer decs); here `eat` matches in place.
    // Looped 200x; prints 1400.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[Node, i64] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => Err(1),
        Some(n) => Ok(n),
    }
}
fn eat(r: Result[Node, i64]) -> i64 {
    match r {
        Err(e) => e,
        Ok(n) => n.val,
    }
}
fn caller() -> i64 {
    let d = take();
    eat(d)
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + caller();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
        &["1400"],
        "consuming_call_result_shared_no_leak",
    );
}

#[test]
fn asan_forwarded_result_shared_param_no_double_free() {
    // B-2026-07-12-24 (residual, consuming-call leg) chain guard: a param
    // FORWARDED to another consuming call (`eat(r) { eat2(r) }`) must NOT be
    // released by the intermediate — it escapes → unregistered → only the
    // TERMINAL consumer (`eat2`, which matches in place) decs. Exactly one
    // release across the chain: no leak, no double-free. Prints 1400.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[Node, i64] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => Err(1),
        Some(n) => Ok(n),
    }
}
fn eat2(r: Result[Node, i64]) -> i64 {
    match r {
        Err(e) => e,
        Ok(n) => n.val,
    }
}
fn eat(r: Result[Node, i64]) -> i64 {
    eat2(r)
}
fn caller() -> i64 {
    let d = take();
    eat(d)
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + caller();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
        &["1400"],
        "forwarded_result_shared_param_no_double_free",
    );
}

#[test]
fn asan_escaping_result_shared_binding_no_double_free() {
    // B-2026-07-12-24 (residual) safety guard: a USER `Result[shared]` binding
    // that ESCAPES must NOT be given a producer-side dec (that would
    // double-free). Here `d` is returned whole (`relay` → `d`) and reaches a
    // consumer (`match relay()`) that releases it — the escape analysis leaves
    // `d` unregistered, so exactly one release happens: no leak, no
    // double-free / use-after-free. A mis-classification would crash under
    // ASAN or corrupt the value; this pins clean + correct (1400).
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[Node, i64] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => Err(1),
        Some(n) => Ok(n),
    }
}
fn relay() -> Result[Node, i64] {
    let d = take();
    d
}
fn caller() -> i64 {
    match relay() {
        Err(e) => e,
        Ok(n) => n.val,
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + caller();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
        &["1400"],
        "escaping_result_shared_binding_no_double_free",
    );
}

#[test]
fn asan_question_on_result_heap_enum_no_leak_or_double_free() {
    // B-2026-07-11-7 — `?` on `Result[<heap-bearing enum>, E]` reconstructs
    // the Ok payload from its words. The old 3-word
    // `rebuild_value_from_payload_words` truncated a 4-word enum
    // (`J { S(String) }` flattens to {tag, ptr, len, cap}), losing `cap`, so
    // the enum's drop freed the `String` with a garbage cap ("free(): invalid
    // pointer"). Flat-copying the enum's full word span across from the
    // Result's payload fixed it. Exercises BORROW-free MOVE-OUT of the String
    // payload (`S(s) => Ok(s)`), a tuple-variant with a heap tail
    // (`Pair(i64, String)`), and loop iteration — the double-free / invalid
    // free surfaced immediately without the fix.
    assert_clean_asan_run(
        r#"
enum J { N, S(String), Pair(i64, String) }
fn get(k: i64) -> Result[J, String] {
    if k == 0i64 { Result.Ok(J.S(f"str-{k}-padding-padding")) }
    else { Result.Ok(J.Pair(k, f"pair-{k}-padding-padding")) }
}
fn take(k: i64) -> Result[String, String] {
    let v = get(k)?;
    match v {
        N => { Result.Ok(f"n") }
        S(s) => { Result.Ok(s) }
        Pair(a, s) => { Result.Ok(f"{a}:{s}") }
    }
}
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        match take(0i64) { Ok(r) => println(r), Err(_) => println("e") }
        match take(1i64) { Ok(r) => println(r), Err(_) => println("e") }
        i = i + 1;
    }
}
"#,
        &[
            "str-0-padding-padding",
            "1:pair-1-padding-padding",
            "str-0-padding-padding",
            "1:pair-1-padding-padding",
        ],
        "asan_question_on_result_heap_enum_no_leak_or_double_free",
    );
}

/// B-2026-09-06-3 — A DISCARDED BOXED `Option` TUPLE-PAYLOAD TEMP NEVER
/// FREED ITS BOX OR THE TUPLE'S INTERIOR.
///
/// `let _ = f();` where `f -> Option[(R, i64)]` boxes the payload (a
/// `(R, i64)` is 5 words, past the 3-word `Option` area). The body walker
/// (`track_discarded_optres_payload_bodies`, B-2026-09-05-14) ran the tuple
/// element's user `Drop` body — so the `dR` lines print on every surface and
/// a transcript-only pin sees nothing wrong — but the MEMORY battery's
/// `try_track_discarded_boxed_option` admitted only a struct payload and
/// declined a tuple one, which then fell through to `materialize_owned_temp`
/// (no Option arm) and freed nothing: the 40-byte box plus the tuple's
/// `String`/`Vec` interior leaked, once per call. The fix extends that
/// tracker with a tuple arm that frees the box and walks the tuple interior
/// through `synthesize_tuple_drop_fn_te` (memory only — its per-element walk
/// routes a struct leaf to `emit_struct_drop_synthesis`, not the user
/// wrapper, so the body does not double).
///
/// A leak-only class the DEFAULT `-O2` optimizer folds away for the cells
/// measured; `KARAC_OPT_LEVEL=0` (the `asan-o0-leg.sh` leg) is what caught
/// it, and this fixture allocates for real there. Looped so any per-call
/// imbalance accumulates for LSan; ASan would flag a double-free if this
/// side and the body walker both freed the tuple interior.
///
/// THE CELLS, in leak-shape then control order:
/// - `topagg(mk(n))` — the fn-return producer, the row's canonical shape.
/// - `Option.Some((mk(..), 9))` — the ctor producer of the same box.
/// - `fss(n)` — an `Option[(String, String)]` tuple with NO user `Drop`;
///   the box + two `String` buffers still leaked pre-fix, and this cell
///   isolates the interior walk from the body channel (no body to run).
/// - `Option.Some(mk(n + 200))` — the boxed STRUCT payload the tracker
///   ALREADY freed; a control that must stay clean, proving the added tuple
///   arm did not perturb the struct arm and that the source's own walk does
///   not double-fire. Each body renders `s` and `xs.len()`, so a body run
///   against a cap-zeroed husk would print `dR..::0` and fail the transcript
///   rather than pass as a bare count.
#[test]
fn asan_discarded_boxed_option_tuple_payload_frees_box_and_interior() {
    assert_clean_asan_run(
            "struct R { id: i64, s: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}:{self.s}:{self.xs.len()}\") } }\n\
             fn mk(n: i64) -> R { return R { id: n, s: f\"s{n}\", xs: [n, n] }; }\n\
             fn topagg(r: R) -> Option[(R, i64)] { return Option.Some((r, 9)); }\n\
             fn fss(n: i64) -> Option[(String, String)] { return Option.Some((f\"aa{n}\", f\"bb{n}\")); }\n\
             fn main() {\n\
             \x20   let mut n = 0;\n\
             \x20   while n < 3 {\n\
             \x20       let _ = topagg(mk(n));\n\
             \x20       let _ = Option.Some((mk(n + 100), 9));\n\
             \x20       let _ = fss(n);\n\
             \x20       let _ = Option.Some(mk(n + 200));\n\
             \x20       n = n + 1;\n\
             \x20   }\n\
             }\n",
            &[
                "dR0:s0:2",
                "dR100:s100:2",
                "dR200:s200:2",
                "dR1:s1:2",
                "dR101:s101:2",
                "dR201:s201:2",
                "dR2:s2:2",
                "dR102:s102:2",
                "dR202:s202:2",
            ],
            "asan_discarded_boxed_option_tuple_payload_frees_box_and_interior",
        );
}

/// B-2026-09-06-43 — A DISCARDED BOXED `Result` PAYLOAD TEMP NEVER FREED
/// ITS BOX OR INTERIOR.
///
/// The discard-position memory battery had `try_track_discarded_inline_result`
/// (inline payload) and `try_track_discarded_boxed_option` (boxed Option),
/// but NO boxed-`Result` member. A `Result` payload wider than the 5-word
/// inline area boxes; the inline tracker zeroes a boxed side's drop per half
/// and so returns false for a purely-boxed payload, and the boxed-Option
/// tracker is Option-only — so a discarded boxed `Result` temp fell through
/// to `materialize_owned_temp` (no Option/Result arm) and freed nothing. The
/// box + interior leaked, once per call, for a STRUCT payload (82 B,
/// `Result[W, i64]`) as much as a tuple one (74 B, `Result[(R, String),
/// i64]`), on the `Err` side as much as `Ok`. The fix adds
/// `try_track_discarded_boxed_result`, registering the complete
/// tag-dispatching `emit_result_drop_fn` (memory only) beside the body
/// walker that already runs the payload's user `Drop`.
///
/// A leak-only class the default `-O2` folds away, so the
/// `KARAC_OPT_LEVEL=0` (`asan-o0-leg.sh`) leg is what caught it; this
/// fixture allocates for real there. Looped so any per-call imbalance
/// accumulates for LSan; ASan would flag a double-free if this side and the
/// body walker both freed the interior.
///
/// THE CELLS:
/// - `rstruct` — a boxed STRUCT `Ok` payload, `Result[W, i64]`.
/// - `rtuple`  — a boxed TUPLE `Ok` payload, `Result[(R, String), i64]`.
/// - `rerr`    — a boxed STRUCT `Err` payload, `Result[i64, W]`; proves the
///   drop dispatches on the live tag, not only `Ok`.
/// - `rinline` — `Result[(R, i64)]` at exactly 5 words, which stays INLINE
///   and is freed by the inline tracker; a control that must stay clean,
///   proving the boxed arm did not perturb the inline path. Each body
///   renders a heap field, so a body run against a cap-zeroed husk would
///   print an empty tail and fail the transcript rather than pass as a bare
///   count.
#[test]
fn asan_discarded_boxed_result_payload_frees_box_and_interior() {
    assert_clean_asan_run(
            "struct R { id: i64, s: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}:{self.s}:{self.xs.len()}\") } }\n\
             struct W { a: String, b: String, c: i64, d: i64, e: i64 }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.c}:{self.a}\") } }\n\
             fn mk(n: i64) -> R { return R { id: n, s: f\"s{n}\", xs: [n, n] }; }\n\
             fn mkw(n: i64) -> W { return W { a: f\"a{n}\", b: f\"b{n}\", c: n, d: 0, e: 0 }; }\n\
             fn rstruct(n: i64) -> Result[W, i64] { return Result.Ok(mkw(n)); }\n\
             fn rtuple(n: i64) -> Result[(R, String), i64] { return Result.Ok((mk(n), f\"x{n}\")); }\n\
             fn rerr(n: i64) -> Result[i64, W] { return Result.Err(mkw(n)); }\n\
             fn rinline(n: i64) -> Result[(R, i64), i64] { return Result.Ok((mk(n), 9)); }\n\
             fn main() {\n\
             \x20   let mut n = 0;\n\
             \x20   while n < 3 {\n\
             \x20       let _ = rstruct(n + 10);\n\
             \x20       let _ = rtuple(n + 20);\n\
             \x20       let _ = rerr(n + 30);\n\
             \x20       let _ = rinline(n + 40);\n\
             \x20       n = n + 1;\n\
             \x20   }\n\
             }\n",
            &[
                "dW10:a10", "dR20:s20:2", "dW30:a30", "dR40:s40:2",
                "dW11:a11", "dR21:s21:2", "dW31:a31", "dR41:s41:2",
                "dW12:a12", "dR22:s22:2", "dW32:a32", "dR42:s42:2",
            ],
            "asan_discarded_boxed_result_payload_frees_box_and_interior",
        );
}

/// B-2026-09-02-22 — AN `Option` PAYLOAD WIDER THAN THE 3-WORD AREA LOST
/// ITS BOX, ITS INTERIOR, OR BOTH, DEPENDING ONLY ON THE CALLEE'S BODY.
///
/// `fn show(x: Option[Option[String]])` boxes its payload (4 words, past
/// the 3-word `Option` area) and measured 141 B in 3 blocks over three
/// calls at `-O0` — 96 direct (the three boxes) plus 45 indirect (the
/// `String` inside each). Two independent defects, and the body decided
/// which fired:
///
///   - a body that reads the param in an INTERPOLATION HOLE
///     (`println(f"{x}")`) reached the callee's owned-param registration
///     under the STRICT non-escaping set, which counts every non-scrutinee
///     use as an escape — so nothing was registered and the BOX leaked too.
///     The registration now asks `by_value_nonescaping_param_names`, which
///     is the same question one position looser: a read cannot move the box.
///   - once registered, the arm freed the BOX ONLY, because it derived its
///     interior drop from a TUPLE payload and an `Option` is not one. The
///     45 B interior survived the box's own reclamation.
///
/// The interior drop goes to `nested_box_leaf_contents` — the bottom of the
/// envelope chain — not to the immediate payload, so `triple` frees the
/// `String` under two envelopes rather than freeing an envelope as though
/// it were a value (B-2026-08-29-2 made the chain and the drop compose).
///
/// Every cell is a direction the fix could break, and three are the
/// double-free directions:
///   - `interp` is the reported shape (gate widening);
///   - `matched` binds the whole inner out, so the interior drop must be
///     RETRACTED to box-only or the arm's binding and the box free twice;
///   - `unused` never mentions the param, so the box is the only owner;
///   - `other` is the OTHER nesting order (`Option[Result[..]]`), which the
///     row measured at the identical 45 B for the same reason;
///   - `triple` exercises the chain leaf (was 96 direct + 141 indirect);
///   - `admitted` is the CONTROL on the far side of the same word-count
///     gate: a `Result`'s area is 5 words and `Option[String]` is 4, so it
///     IS entry-copied and was already clean — B-2026-09-03-6's double free
///     lives here and must not come back;
///   - `ident` RETURNS the param, so it is in neither escape set and must
///     stay unregistered, leaving the caller's binding the only owner.
#[test]
fn asan_wide_option_payload_param_frees_box_and_interior() {
    assert_clean_asan_run(
        r#"
fn interp(x: Option[Option[String]]) { println(f"i:{x}"); }

fn matched(x: Option[Option[String]]) {
    match x { Some(inner) => { println(f"m:{inner}"); } None => { println("mn"); } }
}

fn unused(x: Option[Option[String]]) { println("u"); }

fn other(x: Option[Result[String, String]]) { println(f"o:{x}"); }

fn triple(x: Option[Option[Option[String]]]) { println(f"t:{x}"); }

fn admitted(x: Result[Option[String], String]) { println(f"a:{x}"); }

fn ident(x: Option[Option[String]]) -> Option[Option[String]] { return x; }

fn main() {
    let mut n = 0;
    while n < 3 {
        interp(Some(Some(f"in-{n}-padpadpad")));
        matched(Some(Some(f"ma-{n}-padpadpad")));
        unused(Some(Some(f"un-{n}-padpadpad")));
        other(Some(Ok(f"ot-{n}-padpadpad")));
        triple(Some(Some(Some(f"tr-{n}-padpadpad"))));
        admitted(Ok(Some(f"ad-{n}-padpadpad")));
        let r = ident(Some(Some(f"id-{n}-padpadpad")));
        println(f"r:{r}");
        n = n + 1;
    }
}
"#,
        &[
            "i:Some(Some(in-0-padpadpad))",
            "m:Some(ma-0-padpadpad)",
            "u",
            "o:Some(Ok(ot-0-padpadpad))",
            "t:Some(Some(Some(tr-0-padpadpad)))",
            "a:Ok(Some(ad-0-padpadpad))",
            "r:Some(Some(id-0-padpadpad))",
            "i:Some(Some(in-1-padpadpad))",
            "m:Some(ma-1-padpadpad)",
            "u",
            "o:Some(Ok(ot-1-padpadpad))",
            "t:Some(Some(Some(tr-1-padpadpad)))",
            "a:Ok(Some(ad-1-padpadpad))",
            "r:Some(Some(id-1-padpadpad))",
            "i:Some(Some(in-2-padpadpad))",
            "m:Some(ma-2-padpadpad)",
            "u",
            "o:Some(Ok(ot-2-padpadpad))",
            "t:Some(Some(Some(tr-2-padpadpad)))",
            "a:Ok(Some(ad-2-padpadpad))",
            "r:Some(Some(id-2-padpadpad))",
        ],
        "asan_wide_option_payload_param_frees_box_and_interior",
    );
}

#[test]
fn asan_option_heap_moved_from_recursive_shared_enum_into_mut_ref_self_method_no_double_free() {
    // B-2026-07-11-37: an `Option[String]` moved out of a RECURSIVE shared-enum
    // variant payload (`ContNode.label`, reached via `Expr.Cont` / `Expr.Wrap`)
    // and passed BY VALUE to a `mut ref self` method (`label_ref`) double-freed
    // the `Some` payload — once via the callee's arm drop, once via the caller's
    // scope-exit `FreeInlineOptionPayload` (the by-value arg transfer never nulled
    // the caller slot). The free-fn call path already zeroed the moved slot
    // (`suppress_inline_option_result_binding_move`); the method-call path did
    // not. AOT/JIT aborted (`free(): double free detected`); the interpreter was
    // correct — a run/build divergence with no diagnostic. Discriminators are all
    // in-shape: the RECURSIVE `Wrap` sibling (its `e3` path) is required to change
    // the enum's drop glue; a `None`-carrying `Cont` (`e2`) exercises the no-move
    // arm. Loops so the double-free (a `Some`-payload UAF/double-free) is
    // observable on every iteration.
    //
    // Full leak-checking applies: this shape once ALSO carried a separate leak
    // (dropping a recursive `shared enum` whose payload struct holds an
    // `Option[String]` field never freed that payload — reproduced with a bare
    // `let e = Expr.Cont(...)` and no method call at all), which forced this
    // test to run with LeakSanitizer off. That leak is now fixed
    // (B-2026-07-11-39), so this shape is fully leak-clean and the strong
    // `assert_clean_asan_run` gate covers both the double-free and no-leak.
    assert_clean_asan_run(
        r#"
shared enum Expr { Cont(ContNode), Wrap(WrapNode) }
struct ContNode { label: Option[String] }
struct WrapNode { inner: Expr }
struct R { labels: Vec[String], hits: i64 }
impl R {
  fn in_scope(ref self, name: ref String) -> bool {
    let mut i = 0;
    loop {
      if i >= self.labels.len() { return false; }
      if self.labels[i] == name { return true; }
      i = i + 1;
    }
  }
  fn label_ref(mut ref self, label: Option[String]) {
    match label {
      Some(l) => { if not self.in_scope(l) { self.hits = self.hits + 1; } }
      None => {}
    }
  }
  fn walk(mut ref self, e: Expr) {
    match e {
      Cont(n) => { let ContNode { label } = n; self.label_ref(label); }
      Wrap(n) => { let WrapNode { inner } = n; self.walk(inner); }
    }
  }
}
fn main() {
  let mut round: i64 = 0;
  while round < 50 {
    let mut r = R { labels: Vec.new(), hits: 0 };
    let e1 = Expr.Cont(ContNode { label: Some("continue-label-payload-xxxxxxxxxxxxxxxxxxxx".to_string()) });
    r.walk(e1);
    let e2 = Expr.Cont(ContNode { label: None });
    r.walk(e2);
    let e3 = Expr.Wrap(WrapNode { inner: Expr.Cont(ContNode { label: Some("nested-label-payload-yyyyyyyyyyyyyyyyyyyy".to_string()) }) });
    r.walk(e3);
    println(r.hits.to_string());
    round = round + 1;
  }
}
"#,
        &["2"; 50],
        "asan_option_heap_moved_from_recursive_shared_enum_into_mut_ref_self_method_no_double_free",
    );
}

#[test]
fn asan_recursive_shared_enum_option_heap_payload_field_no_leak() {
    // B-2026-07-11-39: dropping a RECURSIVE `shared enum` whose variant payload
    // is a struct with an `Option[<inline-heap>]` field (`Option[String]` /
    // `Option[Vec[T]]`) leaked the whole boxed payload + its `Some` payload. The
    // recursive `Wrap` variant makes the enum need a generated rc-drop
    // destructor; inside it the `Cont(ContNode)` variant was judged non-walkable
    // because `type_expr_has_drop_heap` has a deliberate `Option => false` blind
    // spot, so the variant got no drop block and its box + String leaked. Native
    // run was clean (leaks do not crash); the Linux LSan gate is authoritative.
    // Fixed by teaching the walkability gates (`field_is_walkable` /
    // `emit_shared_enum_field_drop`) the Option-aware `te_owns_option_heap_payload`
    // predicate and adding an `Option[<inline-heap>]` arm to the payload-struct
    // walker (`emit_nested_struct_shared_rc_decs_ex`) that frees the Some payload
    // via `emit_option_drop_fn`. Exercises the bare create-and-drop trigger
    // (`a`), the `None`-carrying no-heap arm (`b`, must stay clean), nested
    // recursion through `Wrap` (`c`), and an `Option[Vec[String]]` inner (`d`).
    assert_clean_asan_run(
        r#"
shared enum Expr { Cont(ContNode), Wrap(WrapNode) }
struct ContNode { label: Option[String], tags: Option[Vec[String]] }
struct WrapNode { inner: Expr }
fn main() {
  let mut round = 0;
  while round < 50 {
    let a = Expr.Cont(ContNode { label: Some("label-payload-xxxxxxxxxxxxxxxxxxxx".to_string()), tags: None });
    let b = Expr.Cont(ContNode { label: None, tags: None });
    let c = Expr.Wrap(WrapNode { inner: Expr.Cont(ContNode { label: Some("nested-payload-yyyyyyyyyyyyyyyy".to_string()), tags: None }) });
    let d = Expr.Cont(ContNode { label: None, tags: Some(Vec["tag-aaaaaaaaaaaaaaaaaa".to_string(), "tag-bbbbbbbbbbbbbbbbbb".to_string()]) });
    println(round.to_string());
    round = round + 1;
  }
}
"#,
        &(0..50)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>(),
        "asan_recursive_shared_enum_option_heap_payload_field_no_leak",
    );
}

/// B-2026-07-03-28 shared-leg focused coverage — a NON-shared struct whose
/// only heap is a `shared` / `Option[shared]` field must rc-dec that field on
/// EVERY owned-drop path, symmetric with the caller-retains entry-copy's
/// rc-inc. Before the fix, a plain struct LOCAL (`let h = Holder{..}`) and a
/// callee-owned by-value PARAM both dropped via `__karac_drop_struct_<S>`
/// alone, which SKIPS shared fields — so the box leaked (the direct-`shared`
/// case never rc-dec'd at all; the `Option[shared]` case leaked once
/// `field_copy_supported` admitted it and the entry-copy rc-INC'd with no
/// matching dec). Fixed by `track_struct_var` registering the COMBINED drop
/// (value-drop + `emit_nested_struct_shared_rc_decs`) for any shared-owning
/// struct, plus the fresh-temp-arg gate (`call_dispatch`) recognizing a
/// shared-owning struct (invisible to `type_expr_has_drop_heap`). Exercises
/// the owned-drop shapes for both a direct `shared` field and an
/// `Option[shared]` field: (1) plain local scope-drop; (2) by-value param
/// BORROWED then dropped (a fresh-temp arg for the copy-supported
/// `Option[shared]` struct, a local for the caller-retains direct-`shared`
/// one); (3) by-value param DESTRUCTURED (the destructure leaf rc-decs,
/// source neutralized — no double-free); (4) `Vec[Holder]` element
/// scope-drop. Payloads ≥36 bytes so LSan sees a leak; a double rc-dec would
/// abort under ASAN. (The direct-`shared` FRESH-TEMP arg and the
/// entry-copy-then-whole-drop of a `Vec[struct]` FIELD are separate
/// PRE-EXISTING residuals — B-2026-07-04-9 — deliberately not exercised.)
#[test]
fn asan_b28_option_shared_and_direct_shared_struct_drop_no_leak() {
    assert_clean_asan_run(
        r#"
shared enum Val { Nothing, Ident(String), Num(i64) }
struct OptH { value: Option[Val] }
struct DirH { value: Val }

fn borrow_opt(h: OptH) -> i64 {
    let mut r = 0;
    match h.value { Some(_) => { r = 1; } None => {} }
    r
}
fn destr_opt(h: OptH) -> i64 {
    let OptH { value } = h;
    let mut r = 0;
    match value { Some(_) => { r = 1; } None => {} }
    r
}
fn borrow_dir(h: DirH) -> i64 {
    let mut r = 0;
    match h.value { Val.Ident(_) => { r = 1; } _ => {} }
    r
}

fn main() {
    let mut total = 0;
    let mut i = 0;
    while i < 4 {
        // (1) plain local scope-drop, both field shapes.
        let a = OptH { value: Some(Val.Ident("option_shared_local_payload_alpha_aaaaaaaa".to_string())) };
        match a.value { Some(_) => { total = total + 1; } None => {} }
        let b = DirH { value: Val.Ident("direct_shared_local_payload_beta_bbbbbbbbbb".to_string()) };
        match b.value { Val.Ident(_) => { total = total + 1; } _ => {} }
        // (2) by-value param borrowed — fresh-temp for the copy-supported
        // Option[shared] struct; a LOCAL for the caller-retains direct-shared one
        // (a direct-shared fresh-temp arg is a separate pre-existing residual).
        total = total + borrow_opt(OptH { value: Some(Val.Ident("byvalue_borrow_opt_payload_gamma_cccccccc".to_string())) });
        let d = DirH { value: Val.Ident("byvalue_borrow_dir_payload_delta_dddddddd".to_string()) };
        total = total + borrow_dir(d);
        // (3) by-value param destructured.
        total = total + destr_opt(OptH { value: Some(Val.Ident("byvalue_destr_opt_payload_epsilon_eeeeeee".to_string())) });
        i = i + 1;
    }
    // (4) Vec[OptH] element scope-drop (built, len-read, dropped unconsumed).
    let mut v: Vec[OptH] = Vec.new();
    let mut j = 0;
    while j < 4 {
        v.push(OptH { value: Some(Val.Ident("vec_element_option_shared_payload_zeta_fff".to_string())) });
        j = j + 1;
    }
    total = total + v.len();
    println(total);
}
"#,
        &["24"],
        "b28_option_shared_and_direct_shared_struct_drop",
    );
}

/// B-2026-08-05-22 — a fresh-temp aggregate ARGUMENT whose heap lives only
/// behind `Option` fields registered no caller-side cleanup, leaking one
/// payload per call. `use_a(mk())` where `A { value: Option[Inner] }`.
///
/// Both ownership signals were blind to it. `aggregate_has_heap_field`
/// walks the LLVM type, where an `Option` is erased `i64` payload words and
/// never a `{ptr,len,cap}` — the same blindness the enum-leaf case already
/// documents. The source-level fallback then asked `type_expr_has_drop_heap`,
/// which returns false for `Option`/`Result` by design, because "their inline
/// payloads are freed by the let-binding machinery" — and a temp ARGUMENT has
/// no binding. `option_field_te_has_drop_heap` closes exactly that gap.
///
/// UNLIKE its b27 neighbours this is a DEFAULT-BUILD leak, not an -O0 one:
/// measured 7492 B in 200 allocations at `-O2` against the unfixed compiler,
/// identical at -O0. The b27 pair masked at -O2 only because `Some(_)` never
/// reads the payload (B-2026-08-04-17's discard family); this fixture reads
/// it, so the allocation is live and the leak is visible on the surface a
/// user actually builds.
///
/// Written to B-2026-08-04-17's three rules so it cannot go vacuous: the
/// seed is `env.args().len()` (opaque — 1 under both the harness and a bare
/// run), the payload CONTENT is runtime-derived rather than const-foldable,
/// and `starts_with` reads its BYTES without consuming it. Floored so a
/// future regression to zero allocations fails loudly instead of passing.
#[test]
fn asan_fresh_temp_arg_option_only_heap_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
struct Inner { s: String }
struct A { value: Option[Inner] }
fn mk(k: i64) -> A {
    let mut b: String = String.new();
    b.push_str("payload-");
    b.push_str(k.to_string());
    b.push_str("-tail-padding-to-force-heap");
    A { value: Some(Inner { s: b }) }
}
fn use_a(a: A) -> i64 {
    match a.value { Some(w) => { if w.s.starts_with("payload") { 1 } else { 2 } } None => 0 }
}
fn main() {
    let n = env.args().len() as i64;
    let mut t = 0;
    let mut i = 0;
    while i < 200 { t = t + use_a(mk(i + n)); i = i + 1; }
    println(t);
}
"#,
        &["200"],
        "fresh_temp_arg_option_only_heap",
        100,
    );
}

/// B-2026-07-03-31 (FIXED, Phase 1 of the caller-retains model): binding the
/// `Some` payload out of an `Option[<agg>]` destructure leaf and using it
/// ONLY as a borrow — `Some(v) => ident_len(v)`, where `ident_len`
/// entry-copies its owned param — must NOT disarm the source payload drop,
/// or the payload's inner heap leaks. The consumption classifier
/// (`arm_only_borrows_option_agg_payload` /
/// `block_only_borrows_option_agg_payload`) now keeps the drop armed for
/// borrow-only arms across `match`, `if let`, and `while let`. Covers a
/// boxed payload (`Val::Ident(String)`, tag + String > 3 words) and an
/// inline payload (`Inner { s: String }`, 3 words). Consuming arms still
/// suppress (verified double-free-clean under ASAN by the many existing
/// Option/enum match tests + the consuming arm below). ≥36-byte payloads for
/// LSan reachability under the Linux gate.
#[test]
fn asan_b31_option_agg_payload_borrow_only_no_leak() {
    assert_clean_asan_run(
        r#"
enum Val { Nothing, Ident(String) }
struct A { value: Option[Val] }
struct B { value: Option[Val] }
fn ident_len(v: Val) -> i64 { match v { Val.Ident(s) => s.len(), Val.Nothing => 0 } }
fn via_match(a: A) -> i64 { let A { value } = a; match value { Some(v) => ident_len(v), None => 0 } }
fn via_iflet(a: A) -> i64 { let A { value } = a; if let Some(v) = value { ident_len(v) } else { 0 } }
fn via_whilelet(a: A) -> i64 {
    let A { value } = a;
    let mut vv = value;
    let mut acc = 0;
    while let Some(v) = vv { acc = acc + ident_len(v); vv = val_none(); }
    acc
}
fn val_none() -> Option[Val] { Option.None }
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 { v.push(A { value: Some(Val.Ident("payload-borrow-only-aaaaaaaaaaaaaaaaaaaaaaaa".to_string())) }); i = i + 1; }
    v
}
fn main() {
    let mut t = 0;
    for a in build() { t = t + via_match(a); }
    for a in build() { t = t + via_iflet(a); }
    for a in build() { t = t + via_whilelet(a); }
    println(t);
}
"#,
        &["792"],
        "b31_option_agg_payload_borrow_only",
    );
}

/// B-2026-08-29-2, outer-`Option` half (FIXED): a doubly-nested `Option`
/// over a heap payload leaked that payload on plain scope exit —
/// `let vv: Option[Option[String]] = Some(Some(s));` and nothing else in
/// the program leaked the whole 38-byte buffer. Reassignment, which the row
/// was filed for, is not involved anywhere.
///
/// The envelope boxes were always freed; only the innermost payload's own
/// heap was stranded. `track_boxed_enum_var` resolved a box's interior drop
/// from a user struct/enum NAME, and a boxed `Option[..]`/`Result[..]`
/// contents is neither — so the box got no interior drop at all. Where the
/// contents' own payload boxes again, `emit_nested_box_chain_free` took
/// over and walks ENVELOPES by construction. The fix reads `inner_drop_fn`
/// as the drop for the value at the BOTTOM of that chain (with no chain the
/// bottom is the box itself, so single-box registrations are unchanged) and
/// resolves it from the contents' TypeExpr.
///
/// THE RETRACTION IS THE OTHER HALF, AND WITHOUT IT THIS IS A DOUBLE FREE
/// RATHER THAN A FIX. An arm written `Some(Some(s))` binds the leaf out and
/// already owns and frees it. Registering the drop alone — measured — makes
/// `asan_direct_boxed_enum_chain_frees_every_envelope` and
/// `asan_struct_field_boxed_heapless_option_envelope_owned` abort with an
/// AddressSanitizer double-free. The `leaf-bound-*` rows below are those two
/// fixtures' shape in miniature and are the ones that would go red first.
///
/// DEPTH IS WHAT MAKES THE RETRACTION PRECISE, and the two rows that pin it
/// are `envelope-bind` and `leaf-wildcard`. A coarse "the pattern binds any
/// name" rule is wrong in the expensive direction: `Some(o)` binds an
/// intermediate ENVELOPE whose binding does NOT take the leaf over, and
/// retracting there puts every one of those shapes back to leaking.
/// `Some(Some(_))` names no owner at all, so the box must stay armed.
///
/// `leaf-bound-wide` is the row that forced the rule's second half. A
/// STRUCT leaf takes NO retraction: there the drop is the struct's field
/// synthesis and an arm binding `w` gets the header, not the fields' heap —
/// verified, since that shape leaks on an unmodified tree, so nobody owns
/// it and retracting would only preserve the leak. Retracting solely where
/// the leaf is an `Option`/`Result`, whose drop frees the payload the
/// pattern literally names, is what lets both rows be clean at once.
///
/// WHICH ROWS CARRY WHICH SIGNAL, measured per row against the unfixed tree
/// rather than assumed, because two of them are easy to mislabel:
/// `envelope-bind` and `leaf-wildcard` are RED unfixed (38 B each) and are
/// therefore FIXED rows as well as depth-rule pins, while the
/// `leaf-bound-*` rows are clean both before and after and are pure
/// hazards — they go red only if the retraction stops firing.
///
/// `-O0` ONLY, and the fixture says so rather than implying otherwise. Every
/// leaking row here builds a nested envelope and drops it; at `-O2` the
/// whole local, non-escaping structure is scalarized and the box `malloc`
/// removed, so there is no allocation left to leak — verified, the fixture
/// is GREEN at `-O2` against the unfixed compiler. Unlike B-2026-08-28-75's
/// and B-2026-08-29-1's fixtures, no read placement recovers that: the
/// reading rows are exactly the hazard rows, which are clean either way.
/// `asan_direct_boxed_enum_chain_frees_every_envelope` states the same
/// limitation for the same family ("the memory half rides entirely on the
/// -O0 leg"), and `scripts/asan-o0-leg.sh` is the gate for both.
///
/// STILL OPEN, and deliberately absent here: the outer-`Result` half
/// (`Result[Option[Wide], i64]` and friends), whose `NestedBoxedEnumDrop`
/// DIRECT registration passes `box_contents: None` at the let site, so
/// there is nothing for this resolver to see. Measured unchanged by this
/// fix and tracked on the row.
///
/// Payloads are runtime-derived through `env.args().len()` and read with
/// `contains` rather than `len` — see the B-2026-08-28-75 fixture for what
/// each guards against.
#[test]
fn asan_nested_option_envelope_frees_its_leaf_payload() {
    const H: &str = "struct Wide { a: String, b: i64, c: i64, d: i64, e: i64 }\n\
             struct Narrow { a: String }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn slen(s: String) -> i64 { if s.contains(\"payload\") { s.len() } else { 0 } }\n\
             fn wl(w: Wide) -> i64 { slen(w.a) }\n\
             fn mkw() -> Wide { Wide { a: payload(), b: 1, c: 2, d: 3, e: 4 } }\n";
    for (label, body, want) in [
            // ── the leaking family: build one, drop it, nothing else ──
            (
                "opt-opt-string",
                "fn f() -> i64 { let vv: Option[Option[String]] = Some(Some(payload()));\n\
                 \x20  if vv.is_some() { 1 } else { 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
            (
                "opt-opt-vec",
                "fn f() -> i64 { let vv: Option[Option[Vec[i64]]] = Some(Some([seed(), 2, 3, 4, 5, 6, 7, 8, 9, 10]));\n\
                 \x20  if vv.is_some() { 1 } else { 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
            // A boxed STRUCT leaf, reached through a chain of two envelopes.
            (
                "opt-opt-wide",
                "fn f() -> i64 { let vv: Option[Option[Wide]] = Some(Some(mkw()));\n\
                 \x20  if vv.is_some() { 1 } else { 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
            // A `Result` one level down rather than an `Option` — 94 B in 2
            // objects unfixed, the `Wide` box AND its `String`.
            (
                "opt-result-wide",
                "fn f() -> i64 { let vv: Option[Result[Wide, i64]] = Some(Ok(mkw()));\n\
                 \x20  if vv.is_some() { 1 } else { 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
            // A single-field struct leaf, which fits its own payload area and
            // so takes a different path to the same place.
            (
                "opt-opt-narrow",
                "fn f() -> i64 { let vv: Option[Option[Narrow]] = Some(Some(Narrow { a: payload() }));\n\
                 \x20  if vv.is_some() { 1 } else { 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
            // ── the depth rule: an ENVELOPE binding must NOT retract ──
            (
                "envelope-bind",
                "fn f() -> i64 { let vv: Option[Option[Wide]] = Some(Some(mkw()));\n\
                 \x20  match vv { Some(o) => (match o { Some(w) => wl(w), None => 0 }), None => 0 } }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            // ── the hazard rows: an arm binds the LEAF, so it owns it ──
            (
                "leaf-bound-string",
                "fn f() -> i64 { let vv: Option[Option[String]] = Some(Some(payload()));\n\
                 \x20  match vv { Some(Some(s)) => slen(s), _ => 0 } }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            (
                "leaf-bound-triple",
                "fn f() -> i64 { let vv: Option[Option[Option[String]]] = Some(Some(Some(payload())));\n\
                 \x20  match vv { Some(Some(Some(s))) => slen(s), _ => 0 } }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            (
                "leaf-bound-iflet",
                "fn f() -> i64 { let vv: Option[Option[String]] = Some(Some(payload()));\n\
                 \x20  if let Some(Some(s)) = vv { slen(s) } else { 0 } }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            // A STRUCT leaf bound out takes NO retraction — see the doc above.
            (
                "leaf-bound-wide",
                "fn f() -> i64 { let vv: Option[Option[Wide]] = Some(Some(mkw()));\n\
                 \x20  match vv { Some(Some(w)) => wl(w), _ => 0 } }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            // ── boundaries ──
            // A wildcard at leaf depth names no owner, so the box must stay
            // armed; retracting here would put the leak straight back.
            (
                "leaf-wildcard",
                "fn f() -> i64 { let vv: Option[Option[String]] = Some(Some(payload()));\n\
                 \x20  match vv { Some(Some(_)) => 1, _ => 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
            // The axis: one level, always correct, before and after.
            (
                "flat-option-wide",
                "fn f() -> i64 { let vv: Option[Wide] = Some(mkw());\n\
                 \x20  if vv.is_some() { 1 } else { 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
            // A POD leaf owes nothing, so the resolver answers `None` and the
            // registration is byte-identical to before.
            (
                "opt-opt-pod",
                "fn f() -> i64 { let vv: Option[Option[i64]] = Some(Some(seed()));\n\
                 \x20  match vv { Some(Some(x)) => x, _ => 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
        ] {
            assert_clean_asan_run(&format!("{H}{body}"), &[want], label);
        }
}

/// B-2026-08-29-18 (FIXED): the outer-`Result` spelling of the
/// nested-envelope leak. `let vv: Result[Option[String], i64] = Ok(Some(s));`
/// with nothing else in the program stranded the whole 38-byte buffer, and
/// so did `Result[Option[Wide], i64]`, `Result[Option[Option[String]], i64]`
/// and `Result[Result[String, i64], i64]`.
///
/// IT IS TWO MECHANISMS, WHICH IS WHAT THE SPLIT ROW GOT WRONG. It was
/// filed as one boxed-payload gap; the emitted IR says otherwise, and the
/// difference is pure layout arithmetic. `Result`'s inline payload area is
/// 5 words:
///
///   * `Result[Option[String], i64]` — the `Option[String]` is 4 words, so
///     it sits INLINE and nothing boxes anywhere. There is no box in the
///     emitted function at all. The Ok payload simply had no drop, because
///     `type_expr_has_drop_heap` answers a flat `false` for `Option` and
///     `Result` — a predicate with 36 callers, so `inline_struct_payload_drop`
///     asks the question itself rather than changing it there.
///   * `Result[Option[Wide], i64]` — the `Wide` is 7 words, so the
///     `Option[Wide]` still sits inline and only the `Wide` boxes. That is
///     the `NestedBoxedEnumDrop` half the row described, whose DIRECT
///     registration passed `box_contents: None`.
///
/// Both now resolve their interior drop, and both are covered here.
///
/// THE INLINE HALF NEEDS ITS OWN GATE, and skipping it turns this row's
/// leak into a double free rather than a fix.
/// `Result[Option[Option[String]], i64]` puts an `Option[Option[String]]`
/// inline whose OWN payload boxes, so word 1 holds a BOX POINTER, not a
/// `{ptr,len,cap}` — and `emit_option_drop_fn` reads the overlay directly.
/// Admitting it freed a pointer as a buffer: measured as an
/// AddressSanitizer double-free before the `boxed_enum_payload_variants`
/// gate was added. `result-opt-opt-string` is that row.
///
/// THE NESTED HALF RETRACTS ON ANY BINDING, not at a measured depth, and
/// the asymmetry with B-2026-08-29-2's rule is a property of what the
/// source can name. For a DIRECT box the levels between binding and leaf
/// are envelopes the program cannot name, so only a binding at the owning
/// depth is an owner. Here the inline payload IS an `Option`/`Result` the
/// program names — `Ok(o)` binds it and carries the box pointer out — so
/// any binding takes the box and everything under it. `nested-arm-chain` is
/// the row that forced this: it is clean on an unmodified tree because the
/// arm chain owns and frees the `String`, and it double-freed until the
/// retraction covered this family.
///
/// `-O0` only, for the reason
/// `asan_nested_option_envelope_frees_its_leaf_payload` states at length:
/// every leaking row builds a nested value and drops it, and at `-O2` the
/// local non-escaping structure is scalarized and the allocation removed.
#[test]
fn asan_nested_result_payload_frees_its_leaf() {
    const H: &str = "struct Wide { a: String, b: i64, c: i64, d: i64, e: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn slen(s: String) -> i64 { if s.contains(\"payload\") { s.len() } else { 0 } }\n\
             fn mkw() -> Wide { Wide { a: payload(), b: 1, c: 2, d: 3, e: 4 } }\n";
    for (label, body, want) in [
            // ── the INLINE half: no box anywhere in the emitted function ──
            (
                "result-option-string",
                "fn f() -> i64 { let vv: Result[Option[String], i64] = Ok(Some(payload()));\n\
                 \x20  match vv { Ok(_) => 1, Err(_) => 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
            (
                "result-option-vec",
                "fn f() -> i64 { let vv: Result[Option[Vec[i64]], i64] = Ok(Some([seed(), 2, 3, 4, 5, 6, 7, 8, 9, 10]));\n\
                 \x20  match vv { Ok(_) => 1, Err(_) => 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
            // Inline payload whose OWN payload boxes — the gate row. Freeing
            // its box pointer as a buffer is a double free, not a leak fix.
            (
                "result-opt-opt-string",
                "fn f() -> i64 { let vv: Result[Option[Option[String]], i64] = Ok(Some(Some(payload())));\n\
                 \x20  match vv { Ok(_) => 1, Err(_) => 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
            // ── the NESTED-BOX half ──
            (
                "result-option-wide",
                "fn f() -> i64 { let vv: Result[Option[Wide], i64] = Ok(Some(mkw()));\n\
                 \x20  match vv { Ok(_) => 1, Err(_) => 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
            // A `Result` payload wide enough to box at the OUTER level, which
            // reaches the direct box action through a payload type the
            // `Option`-only helper could not name.
            (
                "result-result-string",
                "fn f() -> i64 { let vv: Result[Result[String, i64], i64] = Ok(Ok(payload()));\n\
                 \x20  match vv { Ok(_) => 1, Err(_) => 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
            // ── hazards: an arm owns what the drop would free ──
            (
                "ok-binds-payload",
                "fn f() -> i64 { let vv: Result[Option[String], i64] = Ok(Some(payload()));\n\
                 \x20  match vv { Ok(o) => (match o { Some(s) => slen(s), None => 0 }), Err(_) => 0 } }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            (
                "ok-binds-leaf",
                "fn f() -> i64 { let vv: Result[Option[String], i64] = Ok(Some(payload()));\n\
                 \x20  match vv { Ok(Some(s)) => slen(s), _ => 0 } }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            // The row that forced the nested family's any-binding rule.
            (
                "nested-arm-chain",
                "fn f() -> i64 { let vv: Result[Option[Option[String]], i64] = Ok(Some(Some(payload())));\n\
                 \x20  match vv { Ok(o) => (match o { Some(i) => (match i { Some(s) => slen(s), None => 0 }), None => 0 }), Err(_) => 0 } }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            (
                "iflet-binds-leaf",
                "fn f() -> i64 { let vv: Result[Option[String], i64] = Ok(Some(payload()));\n\
                 \x20  if let Ok(Some(s)) = vv { slen(s) } else { 0 } }\n\
                 fn main() { println(f()); }\n",
                "38",
            ),
            // A wildcard names no owner, so the drop must stay armed.
            (
                "inner-wildcard",
                "fn f() -> i64 { let vv: Result[Option[String], i64] = Ok(Some(payload()));\n\
                 \x20  match vv { Ok(Some(_)) => 1, _ => 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
            // ── axes and boundaries ──
            (
                "flat-result-string",
                "fn f() -> i64 { let vv: Result[String, i64] = Ok(payload());\n\
                 \x20  match vv { Ok(_) => 1, Err(_) => 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
            (
                "flat-result-wide",
                "fn f() -> i64 { let vv: Result[Wide, i64] = Ok(mkw());\n\
                 \x20  match vv { Ok(_) => 1, Err(_) => 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
            // A POD payload owes nothing: the resolver answers `None` and the
            // registration is what it was.
            (
                "result-option-pod",
                "fn f() -> i64 { let vv: Result[Option[i64], i64] = Ok(Some(seed()));\n\
                 \x20  match vv { Ok(Some(x)) => x, _ => 0 } }\n\
                 fn main() { println(f()); }\n",
                "1",
            ),
            // The `Err` side carries the nested payload — both halves of the
            // registration are gated per side, so this must work too.
            (
                "err-side-payload",
                "fn f() -> i64 { let vv: Result[i64, Option[String]] = Err(Some(payload()));\n\
                 \x20  match vv { Ok(_) => 1, Err(_) => 0 } }\n\
                 fn main() { println(f()); }\n",
                "0",
            ),
        ] {
            assert_clean_asan_run(&format!("{H}{body}"), &[want], label);
        }
}

/// B-2026-07-04-7 (FIXED): a `struct A { value: Option[<non-shared enum/struct>] }`
/// field is now DROP-SUPPORTED. Before, `emit_struct_drop_synthesis(A)` emitted no
/// drop for the `Option[<heap enum>]` field (the `OptionInline` pass was gated to
/// String/Vec payloads), so `A` read as heapless and a `Vec[A]` teardown skipped the
/// element walk — the `Some` payload (String + boxed enum) leaked. The fix makes
/// `Option[<struct/enum>]` copy-supported (`field_copy_supported`'s Option arm +
/// `deep_copy_option_struct_enum_payload_in_place`, the box-aware copy peer of
/// `emit_option_drop_fn`) and broadens the `OptionInline` drop pass to that payload
/// class — copy == drop, so an entry-copied param and the caller's retained original
/// own independent heap. The destructure move-out (`let A { value } = a`) zeros the
/// callee-owned source's Option tag (`zero_struct_field_move_cap`) so the source
/// struct-drop skips the moved-out payload (else double-free vs the B-27 leaf drop).
/// Exercises: zero-escape build+drop, pass-by-value borrow, pass-by-value payload
/// move-out, destructure+wildcard (B-27 shape), destructure+move-out (B-31 shape),
/// and an index-alias move-out — all in `Vec[A]`-consuming loops. ≥36-byte payloads
/// for LSan reachability. (Payload len = 40, so `6 + 6 + 6*40 + 6 + 6*40 + 40 = 538`.)
#[test]
fn asan_b04_7_option_heap_enum_struct_field_drop_no_leak() {
    assert_clean_asan_run(
        r#"
enum Val { Nothing, Ident(String), Num(i64) }
#[derive(Clone)]
struct A { value: Option[Val] }
fn ident_len(v: Val) -> i64 { match v { Val.Ident(s) => s.len(), Val.Num(n) => n, Val.Nothing => 0 } }
fn count(a: A) -> i64 { match a.value { Some(_) => 1, None => 0 } }
fn get_len(a: A) -> i64 { match a.value { Some(v) => ident_len(v), None => 0 } }
fn use_wild(a: A) -> i64 { let A { value } = a; match value { Some(_) => 1, None => 0 } }
fn via_match(a: A) -> i64 { let A { value } = a; match value { Some(v) => ident_len(v), None => 0 } }
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 { v.push(A { value: Some(Val.Ident("payload_b047_aaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string())) }); i = i + 1; }
    v
}
fn main() {
    let z: Vec[A] = build();
    let mut t = z.len();
    for a in build() { t = t + count(a); }
    for a in build() { t = t + get_len(a); }
    for a in build() { t = t + use_wild(a); }
    for a in build() { t = t + via_match(a); }
    let xs: Vec[A] = build();
    let c0 = xs[0].clone();
    t = t + get_len(c0);
    println(t);
}
"#,
        &["538"],
        "b04_7_option_heap_enum_struct_field_drop",
    );
}

// ── #20: call / method result as an inline argument ──────────
//
// A heap String produced by a `Call` (`sink(mk(i))`) or `MethodCall`
// (`println(i.to_string())`) and passed DIRECTLY as a by-value argument
// is a fresh owned temp with no consuming binding. Owned String params
// are caller-freed (the callee never drops them), so the temp orphaned
// and leaked one buffer per call — unbounded in a loop, and a real
// accumulating leak in the lexer/parser's inline string building.
// Fixed by materializing the user-fn call-result arg into the caller
// scope (`materialize_owned_temp`) and freeing the `println` arg buffer
// via `free_fresh_owned_str_arg`. Both are Call/MethodCall-only and
// place-/literal-safe, so a `let`-bound arg (owned by its binding) is
// untouched — no double-free.

#[test]
fn asan_call_result_arg_temp_no_leak() {
    assert_clean_asan_run(
        r#"
fn mk(i: i64) -> String {
    let mut s = String.new();
    s.push_str("v");
    s.push_str(i.to_string());
    s
}
fn sink(s: String) { if s.len() > 99999 { println(s); } }
fn main() {
    let mut i = 0i64;
    while i < 5i64 {
        sink(mk(i));
        i = i + 1;
    }
    let t = mk(7i64);
    sink(t);
    println("ok");
}
"#,
        &["ok"],
        "call_result_arg_temp",
    );
}

#[test]
fn asan_ref_param_option_field_consume_no_leak_no_double_free() {
    // B-2026-07-21-9 memory leg: the ref-chain Option clone. Double-free
    // half: a consumed Some payload freed exactly once by its binding,
    // never again by the caller. Leak half: a NON-consuming arm
    // (`Some(_)` / the None-holder paths) leaves the clone's
    // FreeInlineOptionPayload armed — a lost registration would leak one
    // cloned payload per call under LSan. Loop so any per-iteration
    // imbalance accumulates.
    assert_clean_asan_run(
        r#"
struct Holder { opt: Option[String], n: i64 }
fn render(h: ref Holder) -> i64 {
    match h.opt {
        Some(s) => { return ("o:".to_string() + s).len(); }
        None => { return 0; }
    }
    return -1;
}
fn has(h: ref Holder) -> i64 {
    match h.opt {
        Some(_) => { return 1; }
        None => { return 0; }
    }
    return -1;
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a = Holder { opt: Some("payload".to_string()), n: 1 };
        acc = acc + render(a) + render(a) + has(a);
        let e = Holder { opt: None, n: 2 };
        acc = acc + render(e) + has(e);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["760"],
        "ref_param_option_field_consume",
    );
}

#[test]
fn asan_struct_result_field_drop_no_leak_no_double_free() {
    // B-2026-07-21-15 memory leg. Leak half: an UNCONSUMED holder's
    // Result payload (and an unconsumed destructure leaf) must be freed
    // by the struct drop / the leaf's own registration — before the fix
    // every such payload leaked wholesale. Double-free half: by-value
    // passing (entry copy), struct moves, Vec-element holders, and
    // consuming matches/destructures must each free the payload exactly
    // once (every move site zeroes the source payload area). Includes a
    // both-halves-heap `Result[String, String]` Err holder and an
    // Err-scalar holder. Loop so any per-iteration imbalance accumulates.
    assert_clean_asan_run(
        r#"
struct H3 { res: Result[String, i64], n: i64 }
struct P2 { r: Result[String, String], n: i64 }
fn take_n(h: H3) -> i64 { return h.n; }
fn take_s(h: H3) -> i64 {
    match h.res {
        Ok(s) => { return s.len(); }
        Err(e) => { return e; }
    }
    return -1;
}
fn mk(v: String) -> H3 { return H3 { res: Ok(v), n: 9 }; }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a = H3 { res: Ok("unconsumed".to_string()), n: 1 };
        acc = acc + a.n;
        let b1 = H3 { res: Ok("byvalue".to_string()), n: 2 };
        let b2 = H3 { res: Ok("byvalue".to_string()), n: 2 };
        let b3 = H3 { res: Ok("byvalue".to_string()), n: 2 };
        acc = acc + take_n(b1) + take_s(b2) + take_s(b3);
        let c = mk("returned".to_string());
        acc = acc + c.n;
        let d = H3 { res: Ok("moved".to_string()), n: 3 };
        let d2 = d;
        acc = acc + take_s(d2);
        let mut vs: Vec[H3] = vec![];
        vs.push(H3 { res: Ok("invec".to_string()), n: 4 });
        acc = acc + vs[0].n;
        let f = H3 { res: Ok("destr".to_string()), n: 5 };
        let H3 { res: r, n: k } = f;
        acc = acc + k;
        if let Ok(s) = r { acc = acc + s.len(); }
        let f2 = H3 { res: Ok("destr2".to_string()), n: 6 };
        let H3 { res: r2, n: k2 } = f2;
        acc = acc + k2;
        let g = P2 { r: Err("bad".to_string()), n: 7 };
        acc = acc + g.n;
        let e = H3 { res: Err(11), n: 8 };
        acc = acc + take_s(e);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["2760"],
        "struct_result_field_drop",
    );
}

/// B-2026-07-28-17 — the SHARED-ROOT sibling of the test below, and the
/// opposite treatment.
///
/// `match a { V(nd) => consume(nd.hp) }`: the struct holding the field is a
/// payload VIEW into an RC node. B-2026-07-28-16's fix neutralizes the
/// SOURCE, which is not available here — zeroing a field inside a shared
/// node would corrupt every other handle to it — so the callee gets a
/// defensive COPY and the node keeps its original.
///
/// Kept as its own test, and deliberately STRAIGHT-LINE, because the shape
/// turned out to be fragile in two ways that a routine test would have
/// papered over — each verified by running the candidate against the
/// pre-fix compiler:
///
///   * The payload must actually ESCAPE the callee (the arm returns
///     `x.label`). A consumer that merely reads it binds a borrow, frees
///     nothing, and the program is clean with or without the fix.
///   * Wrapping the same body in a `while` loop — the accumulate-so-
///     imbalance-shows idiom the sibling test uses — SUPPRESSES it. The
///     looped form passed pre-fix; the straight-line form aborts.
///
/// So this asserts stdout rather than an accumulated total, and repeats the
/// shape with distinct bindings instead of iterating.
#[test]
fn asan_shared_payload_view_optres_field_call_arg_copy_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
struct H { label: String, k: i64 }
struct Node { hp: Option[H], k: i64 }
shared enum E { V(Node) }
fn hname(h: Option[H]) -> String {
    match h {
        None => "none".to_string(),
        Some(x) => x.label,
    }
}
fn main() {
    let a = E.V(Node { hp: Some(H { label: "deep".to_string(), k: 2 }), k: 7 });
    match a {
        V(nd) => {
            println(hname(nd.hp));
        }
    }
    let b = E.V(Node { hp: Some(H { label: "second".to_string(), k: 3 }), k: 8 });
    match b {
        V(nd) => {
            println(hname(nd.hp));
        }
    }
    let c = E.V(Node { hp: None, k: 9 });
    match c {
        V(nd) => {
            println(hname(nd.hp));
        }
    }
}
"#,
        &["deep", "second", "none"],
        "shared_payload_view_optres_field_call_arg_copy",
    );
}

/// B-2026-08-06-10 — a callee that MOVES OUT OF a heap-BOXED `Option`
/// payload it received BY VALUE, in both move shapes, with the read-only
/// control that has to stay clean.
///
/// `H` is four LLVM words against `Option`'s three-word payload area, so
/// `coerce_to_payload_words` heap-boxes it and the callee's arm binding is a
/// DEBOXED COPY — a private load of a box the CALLER still owns. Three
/// callees over that one struct, and the caller is byte-identical for all
/// three, so any difference is the callee's arm alone:
///
///   * `take_label` moves a FIELD out (`x.label`). Pre-fix the caller zeroed
///     the source's Option tag on the way in, disarming the very box drop it
///     still owned: 32 bytes orphaned per call.
///   * `take_all` moves the WHOLE payload out (`x`). Same leak — and the arm
///     that pins the fix's two halves together, because the caller-side half
///     alone (stop zeroing the tag) turns THIS one into an ASAN double-free
///     abort. It balances only once the callee mirrors its move-out through
///     the box.
///   * `take_key` reads a scalar (`x.k`) and moves nothing. Ownership infers
///     `ref`, the suppressor never runs, and the box drop owns the lot. It
///     was clean before the fix and must stay clean: a neutralizer that
///     fires too widely surfaces HERE, as a leak of a label nobody moved.
///
/// `None` sources are exercised for the reason the sibling test gives — that
/// is the direction where an over-eager source zero strands a payload rather
/// than double-freeing it.
///
/// COVERAGE, stated because it is weaker than the assertion looks: measured
/// against the pre-fix compiler this leaks 640 B in 20 allocations at
/// `KARAC_OPT_LEVEL=0` and is CLEAN at the default `-O2`, where the whole
/// program folds to 3 allocations. Loop-counter-derived labels were not
/// enough to defeat that — the loop is fully constant — so the memory half
/// of this fixture is really carried by the `-O0` leg
/// (`scripts/asan-o0-leg.sh`, B-2026-08-04-17), which is what that leg
/// exists for. What it does assert at BOTH levels is the accumulated total,
/// so a leak traded for a wrong value still fails here.
#[test]
fn asan_boxed_option_param_payload_move_out_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
struct H { label: String, k: i64 }
struct Node { hp: Option[H], k: i64 }

fn take_label(h: Option[H]) -> String {
    match h {
        Option.Some(x) => { return x.label; }
        Option.None => { return "none".to_string(); }
    }
}

fn take_all(h: Option[H]) -> H {
    match h {
        Option.Some(x) => { return x; }
        Option.None => { return H { label: "none".to_string(), k: 0 }; }
    }
}

fn take_key(h: Option[H]) -> i64 {
    match h {
        Option.Some(x) => { return x.k; }
        Option.None => { return 0; }
    }
}

fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 10 {
        let a = Node { hp: Option.Some(H { label: "row-" + i.to_string(), k: i }), k: 1 };
        let s = take_label(a.hp);
        acc = acc + s.len();

        let b = Node { hp: Option.Some(H { label: "wide-" + i.to_string(), k: i }), k: 2 };
        let g = take_all(b.hp);
        acc = acc + g.label.len() + g.k;

        let c = Node { hp: Option.Some(H { label: "read-" + i.to_string(), k: i }), k: 3 };
        acc = acc + take_key(c.hp);

        let d = Node { hp: Option.None, k: 4 };
        acc = acc + take_label(d.hp).len();
        let e = Node { hp: Option.None, k: 5 };
        acc = acc + take_key(e.hp);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["240"],
        "boxed_option_param_payload_move_out",
    );
}

/// B-2026-08-17-45 — the LOCAL-scrutinee sibling of the owned-param test
/// above, and the shape B-2026-08-06-10's mirror was never armed for.
///
/// `record_deboxed_payload_box` recorded the payload box ONLY when the
/// scrutinee was an owned param, on the reasoning that an in-frame owner is
/// a `BoxedEnumDrop` this frame can RETRACT (B-2026-08-04-2). Retraction is
/// whole-action, so it covers the whole payload escaping and nothing else.
/// Move a single `Option`-typed FIELD out of the arm binding and there is
/// no action to retract — the box's drop must still run for the fields the
/// move did not take — while the per-field neutralizer that does exist
/// wrote into the DEBOXED COPY, which that drop cannot see. Both compiled
/// backends aborted with `free(): double free detected in tcache 2` on a
/// program `--interp` ran correctly and `karac check` passed.
///
/// THREE LEGS, and the second is the one that catches an over-eager fix:
///
///   * MOVED (`x.c`) — the field the arm hands out must be freed exactly
///     once. This is the double-free leg.
///   * KEPT (`y.other.len()`) — an arm that moves NOTHING, over a struct
///     carrying TWO owning fields. The mirror writes one field, so `other`
///     and the un-taken `c` must both still be freed by the box's drop. A
///     neutralizer that zeroed the whole payload surfaces here as a leak,
///     which is the trade every neighbouring suppressor's doc records
///     having made at least once.
///   * `None` source — the direction where an over-eager source zero
///     strands a payload instead of double-freeing it.
///
/// COVERAGE IS STRONGER THAN THE SIBLING'S, which is worth stating because
/// that one had to fall back to the `-O0` leg: measured against the pre-fix
/// compiler this SEGVs under ASAN at BOTH the default `-O2` and
/// `KARAC_OPT_LEVEL=0`, so the fixture pins the fix at whichever level CI
/// happens to run.
///
/// The user-`Drop` spelling of the same move lives in its own fixture
/// below (`asan_boxed_option_user_drop_field_move_out_*`). When this one
/// landed it still double-freed and was filed as B-2026-08-18-4, because
/// the mirror excludes it on purpose: the box's words are what a re-homed
/// payload-BODIES walk reads, so a move-site zero corrupts the body. That
/// row is now fixed by QUEUEING the zero and emitting it between the two
/// readers rather than by widening this gate.
#[test]
fn asan_boxed_option_local_scrutinee_field_move_out_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
struct C { name: String, zip: i64 }
struct A { c: Option[C], other: String }

fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        let a = Option.Some(A {
            c: Option.Some(C { name: "payload-string-" + i.to_string(), zip: i }),
            other: "sibling-string-" + i.to_string(),
        });
        let f = match a {
            Option.Some(x) => { x.c }
            Option.None => { Option.None }
        };
        match f {
            Option.Some(cc) => { acc = acc + cc.name.len() + cc.zip; }
            Option.None => { acc = acc + 1; }
        }
        i = i + 1;
    }
    let mut j: i64 = 0;
    while j < 20 {
        let b = Option.Some(A {
            c: Option.Some(C { name: "kept-string-" + j.to_string(), zip: j }),
            other: "kept-sibling-" + j.to_string(),
        });
        let n = match b {
            Option.Some(y) => { y.other.len() }
            Option.None => { 0 }
        };
        acc = acc + n;
        j = j + 1;
    }
    let e: Option[A] = Option.None;
    let g = match e {
        Option.Some(z) => { z.c }
        Option.None => { Option.None }
    };
    match g {
        Option.Some(cc) => { acc = acc + cc.name.len(); }
        Option.None => { acc = acc + 7; }
    }
    println(acc);
}
"#,
        &["817"],
        "boxed_option_local_scrutinee_field_move_out",
    );
}

/// B-2026-08-18-4 — the user-`Drop` spelling of the fixture above, and the
/// shape its gate deliberately excluded.
///
/// With an `impl Drop` on the payload, a re-homed `__karac_dropelems_opt_*`
/// BODIES walk still reads the box after the move. So the two neutralizing
/// channels that already existed were both wrong here: writing the zero at
/// the MOVE SITE (B-2026-08-06-10's mirror) corrupts what that body reads —
/// measured once as a double free traded for a Drop body printing an empty
/// string — and RETRACTION (B-2026-08-04-2) is whole-action, so it cannot
/// express "skip exactly the one field the move took" while the box's drop
/// stays responsible for the rest.
///
/// The fix queues the zero and emits it BETWEEN the two readers: after the
/// bodies walk, before the box's memory drop. This fixture is what proves
/// both halves of that seam, which is why the `impl Drop` body READS a
/// sibling field (`self.other.len()`) rather than merely announcing itself
/// — if the zero lands too early, the body reads a buffer the move already
/// handed away and ASAN reports it here rather than staying quiet.
///
/// Same three legs as the sibling, for the same reasons: MOVED is the
/// double-free leg, KEPT (an arm that moves nothing, over a struct with two
/// owning fields) is where a too-wide zero surfaces as a leak, and the
/// `None` source is the direction an over-eager zero strands a payload.
///
/// NON-VACUITY IS MEASURED, not assumed: against the pre-fix compiler this
/// exits 1 under ASAN at BOTH the default opt level and
/// `KARAC_OPT_LEVEL=0`, and clean at both after.
#[test]
fn asan_boxed_option_user_drop_field_move_out_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
struct C { name: String, zip: i64 }
struct A { c: Option[C], other: String }

impl Drop for A {
    fn drop(mut ref self) {
        let _ = self.other.len();
    }
}

// B-2026-09-03-20 — the match arm's tail `{ x.c }` moves a field out of an
// own-`Drop` `A`; the opt-out keeps the memory behaviour this pins reachable.
#[allow(partial_move_of_drop_struct)]
fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        let a = Option.Some(A {
            c: Option.Some(C { name: "payload-string-" + i.to_string(), zip: i }),
            other: "sibling-string-" + i.to_string(),
        });
        let f = match a {
            Option.Some(x) => { x.c }
            Option.None => { Option.None }
        };
        match f {
            Option.Some(cc) => { acc = acc + cc.name.len() + cc.zip; }
            Option.None => { acc = acc + 1; }
        }
        i = i + 1;
    }
    let mut j: i64 = 0;
    while j < 20 {
        let b = Option.Some(A {
            c: Option.Some(C { name: "kept-string-" + j.to_string(), zip: j }),
            other: "kept-sibling-" + j.to_string(),
        });
        let n = match b {
            Option.Some(y) => { y.other.len() }
            Option.None => { 0 }
        };
        acc = acc + n;
        j = j + 1;
    }
    let e: Option[A] = Option.None;
    let g = match e {
        Option.Some(z) => { z.c }
        Option.None => { Option.None }
    };
    match g {
        Option.Some(cc) => { acc = acc + cc.name.len(); }
        Option.None => { acc = acc + 7; }
    }
    println(acc);
}
"#,
        &["817"],
        "boxed_option_user_drop_field_move_out",
    );
}

#[test]
fn asan_rebound_boxed_option_payload_returned_is_owned_once() {
    // B-2026-08-29-7 — a `match` arm over an owned `Option[T]` param that
    // REBINDS its payload to a local and lets that local escape
    // (`Some(r) => { let k = r; return k; }`) double-freed the payload's
    // interior buffer on both compiled backends, on a program `--interp`
    // runs correctly: `free(): double free detected in tcache 2`, abort 134.
    //
    // `Res` is 4 words, one past `Option`'s 3-word payload area, so the
    // payload is heap-BOXED — which is the whole mechanism. The arm's
    // move-neutralizer zeroes the escaping value's `{ptr,len,cap}` AND, when
    // the box's owner is another frame, mirrors that zero into the box so
    // the caller's `__karac_drop_struct_Res(box)` skips the buffer the
    // return value carried away. That mirror is keyed by SLOT, and the
    // rebind gives the escaping value a NEW slot the map had never heard
    // of, so it was silently skipped and the caller freed the buffer a
    // second time.
    //
    // The same arm WITHOUT the intermediate `let` was always clean, because
    // there the escaping value is the registered slot — which is what made
    // this look like an alias-following gap in a predicate rather than a
    // missing data write. Three controls pinned it as neither: `Result`,
    // a user value enum, and a SCALAR payload all stayed clean through the
    // identical rebind, and only the scalar case says why (no box).
    //
    // The `Drop` body reads `name` so the buffer is genuinely live: printing
    // the id alone lets LLVM delete the allocation together with both frees,
    // which is exactly how a double free hides behind a green -O2 run.
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
fn take(b: Option[Res]) -> Res {
    match b {
        Some(r) => { let k: Res = r; return k; }
        None => { return Res { id: 0, name: f"z" }; }
    }
}
fn main() {
    let b: Option[Res] = Some(Res { id: 7, name: f"e7" });
    let r: Res = take(b);
    println(f"got {r.id}");
}
"#,
        &["got 7", "drop 7 e7"],
        "asan_rebound_boxed_option_payload_returned_is_owned_once",
    );
}

/// B-2026-08-28-68 — a `Result` with ONE side boxed lost the OTHER side's
/// inline payload walk, and the obvious fix for that then leaked the box.
///
/// TWO defects, one shape, and both halves are pinned here because fixing
/// either alone is a regression:
///
///   * `track_inline_result_payload_var` opened with a name-keyed early
///     return on `boxed_enum_payload_vars`. That set records the BINDING,
///     and the let site inserts it as soon as EITHER half boxes — so a
///     `Result` with one boxed half returned before reaching either side's
///     walk, including the inline half whose payload nothing else frees.
///     The per-side gates just below it (`ok_boxed` / `err_boxed`,
///     B-2026-08-06-26) already express the hazard correctly; the
///     binding-keyed return fired first and made them unreachable.
///   * Narrowing that return alone then broke
///     `asan_boxed_result_payload_no_inline_cleanup`, because the consuming
///     arm's disarm calls `zero_result_payload_area`, which zeroes fields
///     `1..n` — EVERY payload word, including word 0, where a boxed side
///     keeps its box POINTER. Nulling it makes the `BoxedEnumDrop` guard
///     skip and the box leaks (128 direct + 64 indirect bytes, 31 allocs /
///     27 frees). Its doc calls whole-area zeroing a "safe superset", which
///     it is only while both sides are inline.
///
/// The MIRROR is what shows the per-side gates were the intended mechanism:
/// B-2026-08-06-26's own note says the sides are gated independently "so a
/// boxed `Ok` beside an inline-heap `Err` keeps the `Err` drop it still
/// needs" — and that exact program (arm C) leaked 2 bytes.
///
/// NOT enum-specific: every arm here has a plain struct payload and no user
/// enum anywhere. Boxedness of the OTHER side is the whole trigger, which is
/// what separates this from B-2026-08-28-64.
///
/// Arm A boxes nothing and was already clean — the control that isolates the
/// trigger to the widened half. Arm D drives the disarm path with a
/// consuming `match`, which is the half that regressed.
#[test]
fn asan_result_with_one_boxed_side_keeps_the_other_sides_walk() {
    let expected: Vec<&str> = vec!["dtrue"; 32];
    assert_clean_asan_run_min_allocs(
        r#"
struct R2 { id: i64, name: String }
impl Drop for R2 { fn drop(mut ref self) { println(f"d{self.name.contains("row")}") } }
struct Wide { a: i64, b: i64, c: i64, d: i64, e: i64, f: i64 }

fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    while i < 8 {
        // A — control: neither side boxes, already clean before the fix.
        let a: Result[R2, i64] = Result.Ok(R2 { id: n + i, name: "row-" + (n + i).to_string() });
        // B — inline `Ok` with heap beside a BOXED `Err`: the reported shape.
        let b: Result[R2, Wide] = Result.Ok(R2 { id: n + i, name: "row-" + (n + i).to_string() });
        // C — the mirror: BOXED `Ok` beside an inline-heap `Err`, the case
        // B-2026-08-06-26's per-side gates were written to preserve.
        let c: Result[Wide, R2] = Result.Err(R2 { id: n + i, name: "row-" + (n + i).to_string() });
        // D — a CONSUMING arm over a one-side-boxed Result, which runs the
        // disarm. Whole-area zeroing here nulls the box pointer and leaks.
        let d: Result[R2, Wide] = Result.Ok(R2 { id: n + i, name: "row-" + (n + i).to_string() });
        match d { Result.Ok(x) => { if x.name.contains("row") { } } Result.Err(_) => {} }
        i = i + 1;
    }
}
"#,
        &expected,
        "result-one-boxed-side-keeps-other-walk",
        8,
    );
}

/// B-2026-08-28-64 — a BOXED enum payload's interior is never walked, so
/// its heap leaks while the box itself is freed correctly.
///
/// `boxed_enum_payload_variants` names the boxed payload so
/// `track_boxed_enum_var` can resolve an inner drop for it, and its filter
/// admitted `struct_types` ALONE. `Option[K]` over `enum K { A(R2), B }`
/// therefore resolved no name at all, `BoxedEnumDrop` ran with
/// `inner_drop_fn: None`, and `R2`'s `String` was stranded behind a box
/// that was itself freed correctly.
///
/// NOT the `Option`-vs-`Result` asymmetry it first reads as. The `Result`
/// twin (`Result[K, i64]`) is clean only because `K` fits `Result`'s
/// 5-word area and stays INLINE, where `track_inline_result_payload_var`
/// already walks it; widen the other half past that area
/// (`Result[K, Wide]`) and the `Result` leaks the identical 2 bytes.
/// BOXEDNESS is the predictor and it cuts across both enums. The
/// `Result`-one-side-boxed residue is a different mechanism — a
/// per-BINDING guard where the boxing is per-SIDE — and is B-2026-08-28-68,
/// deliberately not pinned here.
///
/// COVERAGE, and the part that is load-bearing rather than decorative: the
/// `Drop` body READS the payload buffer (`contains` scans its bytes). With
/// the body suppressed, or reading only the scalar `self.id`, nothing
/// observes that buffer, LLVM deletes the `malloc` outright, and the shape
/// is CLEAN against the broken compiler — the same mask this row's parent
/// (B-2026-08-28-58) tripped over. The payload is seeded from the opaque
/// `env.args().len()` for the same reason, while the body prints a
/// seed-INDEPENDENT `contains` result so the expectation cannot drift with
/// the harness's argv.
///
/// Four registration paths share the one boxed action, so all four run: a
/// plain `let`, a wildcard `match` (non-consuming — the let-site action
/// still fires), a by-value call (which also exercises the
/// `boxed_struct_payload_vars` arg-site skip the widened filter now admits
/// enums to), and a rebind move. A CONSUMING `match` arm is deliberately
/// absent: it currently runs no body at all on any backend, which is
/// B-2026-08-28-63 and not this row — pinning its count here would make
/// this test fail the moment that row is fixed.
///
/// Pre-fix this is RED at every opt level (2 B definitely lost per shape,
/// measured -O0/-O1/-O2), so it does not depend on the default leg.
#[test]
fn asan_boxed_enum_option_payload_interior_is_walked() {
    // 4 shapes x 8 iterations, one body each, all seed-independent.
    let expected: Vec<&str> = vec!["dtrue"; 32];
    assert_clean_asan_run_min_allocs(
        r#"
struct R2 { id: i64, name: String }
impl Drop for R2 { fn drop(mut ref self) { println(f"d{self.name.contains("row")}") } }
enum K { A(R2), B }

fn take(o: Option[K]) -> i64 { 1 }

fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    while i < 8 {
        // A — plain `let`, the shape the row reports.
        let a: Option[K] = Option.Some(K.A(R2 { id: n + i, name: "row-" + (n + i).to_string() }));
        // B — wildcard `match`: non-consuming, so the let-site action still fires.
        let b: Option[K] = Option.Some(K.A(R2 { id: n + i, name: "row-" + (n + i).to_string() }));
        match b { Option.Some(_) => {} Option.None => {} }
        // C — by-value call: the arg-site move must leave exactly one owner.
        let c: Option[K] = Option.Some(K.A(R2 { id: n + i, name: "row-" + (n + i).to_string() }));
        let _ = take(c);
        // D — rebind move: the destination owns the box, the source disarms.
        let d: Option[K] = Option.Some(K.A(R2 { id: n + i, name: "row-" + (n + i).to_string() }));
        let e = d;
        i = i + 1;
    }
}
"#,
        &expected,
        "boxed-enum-option-payload-interior",
        8,
    );
}

#[test]
fn asan_boxed_struct_option_payload_by_value_call_no_leak_no_double_free() {
    assert_clean_asan_run_min_allocs(
        r#"
struct H { label: String, k: i64 }
struct Node { hp: Option[H], k: i64 }

fn readk(h: Option[H]) -> i64 {
    match h {
        Option.Some(x) => { if x.label.contains("row") { x.k } else { 0 - x.k } }
        Option.None => 0,
    }
}
fn take_label(h: Option[H]) -> String {
    match h {
        Option.Some(x) => { return x.label; }
        Option.None => { return "none".to_string(); }
    }
}
fn take_all(h: Option[H]) -> H {
    match h {
        Option.Some(x) => { return x; }
        Option.None => { return H { label: "none".to_string(), k: 0 }; }
    }
}
fn take_destructure(h: Option[H]) -> i64 {
    match h {
        Option.Some(x) => { let H { label, k } = x; if label.contains("row") { k } else { 0 - k } }
        Option.None => 0,
    }
}

fn main() {
    let n = env.args().len() as i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40 {
        // A — NAMED binding, read-only callee.
        let a: Option[H] = Option.Some(H { label: "row-" + (n + i).to_string(), k: n + i });
        acc = acc + readk(a) - n;
        // B — NAMED binding, callee moves a FIELD out.
        let b: Option[H] = Option.Some(H { label: "row-" + (n + i).to_string(), k: n + i });
        if take_label(b).contains("row") { acc = acc + 1; }
        // C — NAMED binding, callee moves the WHOLE payload out.
        let c: Option[H] = Option.Some(H { label: "row-" + (n + i).to_string(), k: n + i });
        let g: H = take_all(c);
        acc = acc + g.k - n;
        if g.label.contains("row") { acc = acc + 1; }
        // C2 — NAMED binding, callee LET-DESTRUCTURES the payload. The
        //      self-host `render_generics` shape; the third move-out mirror.
        let c2: Option[H] = Option.Some(H { label: "row-" + (n + i).to_string(), k: n + i });
        acc = acc + take_destructure(c2) - n;
        // D — FRESH TEMP, read-only callee.
        acc = acc + readk(Option.Some(H { label: "row-" + (n + i).to_string(), k: n + i })) - n;
        // E — FRESH TEMP, callee moves a FIELD out.
        if take_label(Option.Some(H { label: "row-" + (n + i).to_string(), k: n + i })).contains("row") { acc = acc + 1; }
        // F — FRESH TEMP, callee moves the WHOLE payload out.
        let g2: H = take_all(Option.Some(H { label: "row-" + (n + i).to_string(), k: n + i }));
        acc = acc + g2.k - n;
        // F2 — FRESH TEMP, callee LET-DESTRUCTURES the payload.
        acc = acc + take_destructure(Option.Some(H { label: "row-" + (n + i).to_string(), k: n + i })) - n;
        // G — FIELD ARGUMENT: the owning struct's drop stays in charge.
        let nd = Node { hp: Option.Some(H { label: "row-" + (n + i).to_string(), k: n + i }), k: 1 };
        acc = acc + readk(nd.hp) - n;
        // H — None sources on two callee shapes.
        let z: Option[H] = Option.None;
        acc = acc + readk(z) + take_destructure(Option.None);
        if take_label(Option.None).contains("none") { acc = acc + 1; }
        // I — NEVER passed: the let-site drop must still fire.
        let keep: Option[H] = Option.Some(H { label: "row-" + (n + i).to_string(), k: n + i });
        acc = acc + match keep { Option.Some(x) => x.k, Option.None => 0 } - n;
        i = i + 1;
    }
    println(acc);
}
"#,
        &["6400"],
        "boxed_struct_option_payload_by_value_call",
        100,
    );
}

/// B-2026-07-28-16 — the CALL-ARGUMENT sibling of the leg above.
/// `consume(nd.opt)` hands an owned struct's `Option`/`Result` field to a
/// parameter that owns it, so the callee's cleanup frees the payload and
/// the owning struct's drop must skip it.
///
/// The suppressor wired at the call site matched an `Identifier` only, so a
/// `FieldAccess` argument fell through and both sides freed. Interp was
/// correct throughout, which is why output parity never caught it — only a
/// memory tool does.
///
/// Three payload classes, because they take different drop paths and only
/// the first two were recognized as a move: an inline `{ptr,len,cap}`
/// String, a Vec, and a STRUCT that owns heap (boxed payload,
/// `BoxedEnumDrop`-guarded). Each is exercised BOTH consumed-by-call and
/// left alone — the None/unconsumed arms are the leak direction, where an
/// over-eager source zero would strand the payload instead of double-freeing
/// it. Loops so any per-iteration imbalance accumulates.
#[test]
fn asan_owned_struct_optres_field_call_arg_move_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
struct Inner { label: String, k: i64 }
struct Hs { opt: Option[String], n: i64 }
struct Hv { opt: Option[Vec[i64]], n: i64 }
struct Ht { opt: Option[Inner], n: i64 }
struct Hr { res: Result[String, i64], n: i64 }
fn take_str(o: Option[String]) -> i64 {
    match o {
        Some(s) => s.len(),
        None => 0,
    }
}
fn take_vec(o: Option[Vec[i64]]) -> i64 {
    match o {
        Some(v) => v.len() as i64,
        None => 0,
    }
}
fn take_struct(o: Option[Inner]) -> i64 {
    match o {
        Some(x) => x.label.len() + x.k,
        None => 0,
    }
}
fn take_res(r: Result[String, i64]) -> i64 {
    match r {
        Ok(s) => s.len(),
        Err(e) => e,
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        // Consumed by an owning call — the double-free direction.
        let a = Hs { opt: Some("payload".to_string()), n: 1 };
        acc = acc + take_str(a.opt);
        let mut w: Vec[i64] = Vec.new();
        w.push(1);
        w.push(2);
        let b = Hv { opt: Some(w), n: 2 };
        acc = acc + take_vec(b.opt);
        let c = Ht { opt: Some(Inner { label: "deep".to_string(), k: 3 }), n: 3 };
        acc = acc + take_struct(c.opt);
        let d = Hr { res: Ok("res".to_string()), n: 4 };
        acc = acc + take_res(d.res);
        // NOT consumed — the struct drop must still free these (leak direction).
        let e = Hs { opt: Some("kept".to_string()), n: 5 };
        acc = acc + e.n;
        let f = Ht { opt: Some(Inner { label: "kept2".to_string(), k: 6 }), n: 6 };
        acc = acc + f.n;
        // None payloads: nothing to free on either side.
        let g = Hs { opt: None, n: 7 };
        acc = acc + take_str(g.opt);
        let h = Ht { opt: None, n: 8 };
        acc = acc + take_struct(h.opt);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["1200"],
        "owned_struct_optres_field_call_arg_move",
    );
}

#[test]
fn asan_owned_struct_optres_field_consume_no_leak_no_double_free() {
    // B-2026-07-21-16 memory leg. Double-free half: a payload bound out
    // of an owned struct's Option field (direct match / if-let /
    // let-else scrutinee, let-move, assign-move) must be freed exactly
    // once by its binding — the source field is zeroed so the struct
    // drop's OptionInline arm skips it. Leak half: a NON-binding arm
    // (`Some(_)` / miss edges) leaves the source armed and the struct
    // drop must still free it; the let-move leg must also arm the new
    // binding's own cleanup (an unconsumed `let x = d.opt` frees via x).
    // Result shapes ride the same legs (the drain route). Loop so any
    // per-iteration imbalance accumulates.
    assert_clean_asan_run(
        r#"
struct H2 { opt: Option[String], n: i64 }
struct H3 { res: Result[String, i64], n: i64 }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a = H2 { opt: Some("payload".to_string()), n: 1 };
        match a.opt {
            Some(s) => { acc = acc + ("m:".to_string() + s).len(); }
            None => { }
        }
        let b = H2 { opt: Some("iff".to_string()), n: 2 };
        if let Some(s) = b.opt {
            acc = acc + s.len();
        }
        let c = H2 { opt: Some("lel".to_string()), n: 3 };
        let Some(t) = c.opt else {
            return;
        }
        acc = acc + (t + "!").len();
        let d = H2 { opt: Some("moved".to_string()), n: 4 };
        let x = d.opt;
        if let Some(s) = x {
            acc = acc + s.len();
        }
        let d2 = H2 { opt: Some("unconsumed".to_string()), n: 5 };
        let x2 = d2.opt;
        let e = H2 { opt: Some("assign".to_string()), n: 6 };
        let mut y: Option[String] = None;
        y = e.opt;
        if let Some(s) = y {
            acc = acc + s.len();
        }
        let w = H2 { opt: Some("wild".to_string()), n: 7 };
        match w.opt {
            Some(_) => { acc = acc + 1; }
            None => { }
        }
        let g = H3 { res: Ok("resmv".to_string()), n: 8 };
        let dr = g.res;
        match dr {
            Ok(s) => { acc = acc + s.len(); }
            Err(e2) => { acc = acc + e2; }
        }
        i = i + 1;
    }
    println(acc);
}
"#,
        &["1320"],
        "owned_struct_optres_field_consume",
    );
}

#[test]
fn asan_ref_param_result_field_consume_no_leak_no_double_free() {
    // B-2026-07-21-14 memory leg: the ref-chain Result clone. Double-free
    // half: a consumed Ok/Err payload freed exactly once by its binding,
    // never again against the caller's retained field. Leak half: a
    // NON-consuming arm (`Ok(_)` / the scalar-Err paths) leaves the
    // clone's FreeInlineResultPayload armed — a lost registration would
    // leak one cloned payload per call under LSan. Covers match /
    // if-let / let-else, a `Result[String, String]` with BOTH halves
    // heap (both live tags), and the drain epilogues each iteration
    // (struct drops do not free Result field payloads — the deliberate
    // caller-retains exclusion, tracked as its own ledger bug). Loop so
    // any per-iteration imbalance accumulates.
    assert_clean_asan_run(
        r#"
struct Holder { res: Result[String, i64], n: i64 }
struct Pair { r: Result[String, String] }
fn render(h: ref Holder) -> i64 {
    match h.res {
        Ok(s) => { return ("o:".to_string() + s).len(); }
        Err(e) => { return e; }
    }
    return -1;
}
fn tag(h: ref Holder) -> i64 {
    match h.res {
        Ok(_) => { return 1; }
        Err(e) => { return e; }
    }
    return -1;
}
fn tell(p: ref Pair) -> i64 {
    match p.r {
        Ok(s) => { return s.len(); }
        Err(m) => { return ("e:".to_string() + m).len(); }
    }
    return -1;
}
fn ifl(h: ref Holder) -> i64 {
    if let Ok(s) = h.res {
        return (s + "?").len();
    }
    return 0;
}
fn lel(h: ref Holder) -> i64 {
    let Ok(s) = h.res else {
        return 0;
    }
    return (s + "!").len();
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a = Holder { res: Ok("payload".to_string()), n: 1 };
        acc = acc + render(a) + render(a) + tag(a) + ifl(a) + lel(a);
        let b = Holder { res: Err(7), n: 2 };
        acc = acc + render(b) + tag(b) + ifl(b) + lel(b);
        let g = Pair { r: Ok("good".to_string()) };
        let w = Pair { r: Err("bad".to_string()) };
        acc = acc + tell(g) + tell(w) + tell(g);
        let dra = a.res;
        if let Ok(s) = dra { acc = acc + s.len(); }
        let drg = g.r;
        if let Ok(s) = drg { acc = acc + s.len(); }
        let drw = w.r;
        match drw {
            Ok(s) => { acc = acc + s.len(); }
            Err(m) => { acc = acc + m.len(); }
        }
        i = i + 1;
    }
    println(acc);
}
"#,
        &["3040"],
        "ref_param_result_field_consume",
    );
}

#[test]
fn asan_option_pop_discarded_freed() {
    // `v.pop();` discards an `Option[String]` temp (the popped element).
    // Borrow accessors (`get`) are excluded; `pop` owns its result.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("he" + "llo");
    v.push("wor" + "ld");
    v.pop();
    println(v[0]);
}
"#,
        &["hello"],
        "option_pop_discarded_freed",
    );
}

#[test]
fn asan_option_if_else_fresh_payload_freed() {
    // Non-`Call` let-RHS: `let x = if c { Some(a) } else { None };`
    // yields a FRESH inline Option — the let-path registration is
    // broadened past `Call` to provably-fresh if/match/block tails
    // (`rhs_is_fresh_inline_enum`). The `Some` String must be freed.
    assert_clean_asan_run(
        r#"
fn main() {
    let c = true;
    let mut a = String.new();
    a.push_str("noncall-if-runtime-heap-payload");
    let x = if c { Some(a) } else { None };
    println("done");
}
"#,
        &["done"],
        "option_if_else_fresh_payload_freed",
    );
}

#[test]
fn asan_single_field_struct_option_payload_sizing_no_bad_access() {
    // #49 (phase-12 self-hosting, found while minimizing #48): a struct
    // whose ONLY field is an `Option[T]`, used as a shared-enum payload
    // (`struct Block { tail: Option[Expr] }` in `Expr.Blk(Block)`). The
    // variant's payload AREA is undersized to 1 word (the `Option` field
    // hits the enum-in-enum carve-out in `payload_word_count_for_type_expr`),
    // and `coerce_to_payload_words`'s scalar fast path (`num_words <= 1`)
    // then collapsed the real 4-word `Block` value to `0` via `coerce_to_i64`
    // — dropping the payload. Unpack/drop independently treat it as BOXED
    // (`llvm_type_word_count(T) > area`) and `inttoptr` the `0`. With a heap
    // String inner this manifests as a wild-pointer read/free ASAN flags
    // (≥36-byte payload so it lands on instrumented heap); the value-correct
    // form would SIGSEGV. The fix guards the fast path on the value's real
    // width so the payload boxes (the proven-correct multi-field path) and
    // pack/unpack/drop stay coherent. Mirrors the codegen E2E
    // `test_e2e_single_field_struct_option_payload_sizing`.
    assert_clean_asan_run(
        r#"
shared enum Expr { Str(String), Blk(Block), Error }
struct Block { tail: Option[Expr] }
fn render_block(b: Block) -> String {
    let Block { tail } = b;
    match tail { Some(e) => render_expr(e), None => "no-tail".to_string() }
}
fn render_expr(e: Expr) -> String {
    match e {
        Str(s) => s,
        Blk(b) => render_block(b),
        Error => "error".to_string(),
    }
}
fn main() {
    let mut payload = String.new();
    payload.push_str("single-field-struct-option-payload-sizing-payload");
    let blk = Block { tail: Some(Expr.Str(payload)) };
    println(render_expr(Expr.Blk(blk)));
}
"#,
        &["single-field-struct-option-payload-sizing-payload"],
        "single_field_struct_option_payload_sizing_no_bad_access",
    );
}

#[test]
fn asan_question_multiword_ok_payload_owned_once() {
    // The `?` multi-word Ok-payload reconstruction (phase-8-stdlib-floor
    // item 8) rebuilds a 3-word String/Vec from all its payload words. The
    // unwrapped heap value must be owned by the binding and freed exactly
    // once at scope exit — a reconstruction that aliased or dropped a word
    // would double-free or leak. `?`-unwrap a heap `String` and a heap
    // `Vec[i64]` (cap > 0 so the free fires), use both, return.
    assert_clean_asan_run(
        r#"
fn take() -> Result[i64, AllocError] {
    let mut a: String = String.new();
    a.push_str("hello");
    let r: Result[String, AllocError] = Ok(a);
    let s: String = r?;
    let src: Vec[i64] = Vec.filled(3, 9);
    let v: Vec[i64] = Vec.try_from_slice(src)?;
    Ok(s.len() + v.len())
}
fn main() {
    match take() { Ok(n) => println(n), Err(_) => println("err") }
}
"#,
        &["8"],
        "question_multiword_ok_payload_owned_once",
    );
}

// ── ? operator drains scope cleanup actions on the failure path ──────
// The early-return emitted by `?` for `Result`/`Option` must run the
// function's accumulated `scope_cleanup_actions` (free Vec/String buffers,
// RC-dec shared values, free Map handles) before returning. Without the
// drain, a Vec live at the `?` site leaks its data buffer when `?` fires.

#[test]
fn asan_question_drains_scope_cleanup_on_err() {
    assert_clean_asan_run(
        r#"
fn boom() -> Result[i64, i64] { Err(7_i64) }
fn use_vec() -> Result[i64, i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    v.push(2_i64);
    v.push(3_i64);
    let _ = boom()?;
    Ok(v.len() as i64)
}
fn main() {
    match use_vec() {
        Ok(n) => println(n),
        Err(e) => println(e),
    }
}
"#,
        &["7"],
        "question_drains_scope_cleanup_on_err",
    );
}

#[test]
fn asan_question_drains_scope_cleanup_on_none() {
    assert_clean_asan_run(
        r#"
fn maybe() -> Option[i64] { None }
fn use_vec() -> Option[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(10_i64);
    v.push(20_i64);
    let _ = maybe()?;
    Some(v.len() as i64)
}
fn main() {
    match use_vec() {
        Some(n) => println(n),
        None => println(0),
    }
}
"#,
        &["0"],
        "question_drains_scope_cleanup_on_none",
    );
}

#[test]
fn asan_whole_value_move_of_boxed_option_frees_the_box_once() {
    // B-2026-08-05-20: `let b2 = body;` — a plain binding-to-binding move
    // of an Option whose payload is heap-BOXED (Wide is 4 words, wider than
    // the 3-word inline payload area) — left BOTH names owning the box, so
    // scope exit freed it twice.
    //
    // This was NOT an -O0 curiosity: it SIGSEGV'd at -O0/-O1 and aborted
    // with glibc `double free detected in tcache 2` at -O2/-O3, i.e. on a
    // default `karac build`. The row was filed believing -O2 was clean;
    // that reading came from a reduction whose payload is read only through
    // `.len()`, which is B-2026-08-04-17's dead-payload masking.
    //
    // The payload here is therefore consumed BYTE BY BYTE, not through
    // `.len()`, so the allocation cannot be deleted and the fixture cannot
    // pass vacuously against a compiler that still aborts.
    assert_clean_asan_run(
        r#"
struct Wide { tag: i64, payload: String }

fn mk_wide() -> Wide {
    Wide { tag: 7, payload: "boxed-option-payload-string-long-enough-aaaa".to_string() }
}

fn byte_sum(s: String) -> i64 {
    let bs = s.bytes();
    let mut n = 0i64;
    let mut i = 0i64;
    while i < bs.len() { n = n + (bs[i] as i64); i = i + 1i64; }
    return n;
}

fn read2() -> i64 {
    let mut body: Option[Wide] = None;
    body = Some(mk_wide());
    let b2 = body;
    match b2 { Some(w) => { w.tag + byte_sum(w.payload) } None => 0 }
}

fn main() {
    let mut acc = 0i64;
    let mut i = 0;
    while i < 50 { acc = acc + read2(); i = i + 1; }
    println(f"{acc}");
}
"#,
        // 50 iterations of (tag 7 + the payload's byte sum 4340).
        &["217350"],
        "whole_value_move_boxed_option",
    );
}

#[test]
fn asan_call_result_tuple_var_no_leak() {
    // #24 (phase-12 self-hosting) — a let-bound tuple VAR sourced from a CALL
    // (`let p = ret_tuple(i)`) whose only heap is an enum / Map leaf leaked: the
    // call-result source missed the annotation/literal arms of
    // `tuple_binding_elem_tes`, so no `TypeExpr`-driven drop was registered and
    // `track_tuple_var`'s LLVM walk is enum/Map-blind. The fix recovers the
    // element TEs from the callee's return type (`fn_return_type_exprs`). The
    // coupled fix (`B-2026-06-14-1`) makes the drop-fn memoization key
    // (`type_expr_sig`) generic-args-aware, so `Map[i64,i64]` and
    // `Map[String,i64]` no longer alias one drop fn — a scalar-first program had
    // leaked a later `Map[String,_]`'s keys; a String-first program ran a
    // `drop_key=1` over a scalar map (the #23 garbage-free class).
    //
    // Loop-stressed: call-sourced enum-leaf tuple UNUSED (the leak), the same
    // destructured + consumed (no double-free), call-sourced scalar-map and
    // String-key-map tuples (both UNUSED — the generic-args memo key), and both
    // map shapes used in one loop body so the memo collision would fire. The
    // f-string payloads/keys keep each iteration's heap non-foldable so Linux
    // LSan sees a real leak if any leaf's drop is missing or mis-keyed.
    assert_clean_asan_run(
        r#"
enum Tok { Id(String), Num(i64) }
fn ret_tuple(i: i64) -> (Tok, i64) { return (Tok.Id(f"id{i}"), i); }
fn ret_imap(i: i64) -> (Map[i64, i64], i64) {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(i, i);
    return (m, i);
}
fn ret_smap(i: i64) -> (Map[String, i64], i64) {
    let mut m: Map[String, i64] = Map.new();
    m.insert(f"k{i}", i); m.insert(f"j{i}", i);
    return (m, i);
}
fn use_tok(t: Tok) -> i64 {
    match t { Id(s) => s.len(), Num(n) => n }
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 8 {
        // Call-sourced enum-leaf tuple var, UNUSED (the #24 leak).
        let p = ret_tuple(i);

        // Call-sourced enum-leaf tuple var, destructured + consumed (no double-free).
        let q = ret_tuple(i + 50);
        let (t, n) = q;
        acc = acc + use_tok(t) + n;

        // Call-sourced scalar-map tuple var, UNUSED (memo key (0,0) flags).
        let im = ret_imap(i);

        // Call-sourced String-key-map tuple var, UNUSED (memo key (1,0) flags) —
        // in the SAME loop body as the scalar map so the old shared memo key would
        // drop this with (0,0) and leak the f-string keys.
        let sm = ret_smap(i);

        i = i + 1;
    }
    if acc > 999999 { println("never"); }
    println("done");
}
"#,
        &["done"],
        "call_result_tuple_var_no_leak",
    );
}

#[test]
fn asan_optres_payload_nested_positions_clean() {
    // B-2026-08-03-1 — the three nested Option positions whose payload
    // MEMORY was already correct, now that their bodies fire too: the
    // added walks free nothing, so arming them must not disturb the
    // existing frees. Deliberately EXCLUDES the tuple-element and
    // Result-field positions: making their bodies fire unmasked a
    // pre-existing missing free in each (the payload was previously
    // never allocated at all, LLVM having elided it as dead — proven with
    // `valgrind --trace-malloc`), tracked as B-2026-08-03-3. The
    // TUPLE-ELEMENT half of that row is now fixed and covered by
    // `asan_tuple_held_optres_payload_freed`; only the Result STRUCT FIELD
    // stays out, still waiting on its entry-copy counterpart.
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct H { o: Option[Res], t: i64 }
fn main() {
    println("field:");
    {
        let h = H { o: Option.Some(Res { id: 1, name: f"aa{1}" }), t: 2 };
        println(h.t);
    }
    println("vecelem:");
    {
        let mut v: Vec[Option[Res]] = Vec.new();
        v.push(Option.Some(Res { id: 2, name: f"bb{2}" }));
        println(v.len());
    }
    println("mapval:");
    {
        let mut m: Map[i64, Option[Res]] = Map.new();
        m.insert(5, Option.Some(Res { id: 3, name: f"cc{3}" }));
        println(m.len());
    }
    println("end");
}
"#,
        &[
            "field:",
            "2",
            "drop 1 aa1",
            "vecelem:",
            "1",
            "drop 2 bb2",
            "mapval:",
            "1",
            "drop 3 cc3",
            "end",
        ],
        "optres_payload_nested_positions_clean",
    );
}

// Regression for the "fn taking `Option[shared T]` chain hangs" bug
// surfaced during kata 2 (add-two-numbers) reduction. Pre-fix, calling
// a helper fn with `list: Option[Node]` argument on a linked list built
// by a `from_arr` loop (`tail.next = Some(node); tail = node`) hung
// indefinitely — somewhere in the scope-exit recursive drop path of
// the chain. The companion test `asan_auto_par_shared_struct_option_
// return_slot` below had to use inline match to avoid this codegen
// bug. The hang is gone on current main as a side effect of the
// intervening Option[shared T] refcount tracking + par-branch RC
// suppression work (commits 3c77a10, 19b998d, codegen.rs §
// `fn_return_option_inner_shared` / `track_rc_option_var`). Keep both
// shapes — helper-fn and inline-match — to lock the fix in place.
#[test]
fn asan_option_shared_chain_through_helper_fn() {
    assert_clean_asan_run(
        r#"
shared struct Node {
    val: i64,
    mut next: Option[Node],
}

fn from_arr(arr: Vec[i64]) -> Option[Node] {
    let n = arr.len();
    if n == 0 {
        return None;
    }
    let head = Node { val: arr[0], next: None };
    let mut tail = head;
    let mut i = 1u64;
    while i < n {
        let node = Node { val: arr[i], next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1u64;
    }
    Some(head)
}

fn first_val(list: Option[Node]) -> i64 {
    match list {
        Some(n) => n.val,
        None => -1i64,
    }
}

fn main() {
    let mut a: Vec[i64] = Vec.new();
    a.push(10);
    a.push(20);
    a.push(30);
    let list = from_arr(a);
    println(first_val(list));
}
"#,
        &["10"],
        "option_shared_chain_through_helper_fn",
    );
}

/// Oversized boxed enum payload — box-free double-free gate. A 4-word
/// `Wide` exceeds Option's 3-word area, so `Some(Wide)` heap-boxes it
/// (`coerce_to_payload_words`); the annotated `let o: Option[Wide]`
/// frees the box at scope exit. The matched-out `e` is a scalar copy
/// (no inner heap), so the only owner of the box is `o`'s slot —
/// looping faults under ASAN if the box is freed twice or the
/// matched-out copy aliases it. macOS has no LeakSanitizer, so the
/// leak side is pinned by the IR free-count test; this is the
/// double-free gate. See docs/spikes/oversized-enum-payload.md.
#[test]
fn asan_boxed_option_let_no_double_free() {
    assert_clean_asan_run(
        r#"
struct Wide { a: i64, b: i64, c: i64, d: i64 }
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let o: Option[Wide] = Some(Wide { a: i, b: 2, c: 3, d: 4 });
        match o {
            Some(e) => println(e.a + e.d),
            None => println(-1),
        }
        i = i + 1;
    }
    println(99);
}
"#,
        &["4", "5", "6", "7", "8", "99"],
        "boxed_option_let_no_double_free",
    );
}

/// Boxed payload whose `T` itself owns heap: `Option[H]` with a `Vec`
/// field (5 words → boxed). Scope exit runs the inner struct drop
/// (frees the Vec buffer) then frees the box. The `vv` moved into
/// `Some(H { v: vv, .. })` must have its own scope cleanup suppressed
/// — otherwise the Vec buffer is freed by both `vv`'s cleanup and the
/// box's inner drop. Looping turns any such imbalance into an ASAN
/// fault. (No `match` here, to isolate the box-drop + move-in
/// suppression from the move-OUT-of-box path, a separate follow-up.)
#[test]
fn asan_boxed_option_inner_heap_no_double_free() {
    assert_clean_asan_run(
        r#"
struct H { v: Vec[i64], a: i64, b: i64 }
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let mut vv: Vec[i64] = Vec.new();
        vv.push(i);
        let o: Option[H] = Some(H { v: vv, a: 1, b: 2 });
        println(i);
        i = i + 1;
    }
    println(99);
}
"#,
        &["0", "1", "2", "3", "4", "99"],
        "boxed_option_inner_heap_no_double_free",
    );
}

/// Oversized-enum-payload §3 (untyped-let inference, inner heap): an
/// untyped `let o = make(i)` over a boxed `Option[H]` (`H` owns a `Vec`,
/// 5 words → boxed). The box drop is inferred from the callee's return
/// type — it must free the inner Vec (via the inner struct drop) and the
/// box exactly once each. The loop turns any imbalance into a
/// deterministic ASAN leak/double-free. Untyped analogue of
/// `asan_boxed_option_inner_heap_no_double_free`.
#[test]
fn asan_untyped_let_boxed_option_inner_heap_no_double_free() {
    assert_clean_asan_run(
        r#"
struct H { v: Vec[i64], a: i64, b: i64 }
fn make(i: i64) -> Option[H] {
    let mut vv: Vec[i64] = Vec.new();
    vv.push(i);
    return Some(H { v: vv, a: 1, b: 2 });
}
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let o = make(i);
        println(i);
        i = i + 1;
    }
    println(99);
}
"#,
        &["0", "1", "2", "3", "4", "99"],
        "untyped_let_boxed_option_inner_heap_no_double_free",
    );
}

/// B-2026-08-27-34 — the LEAK half of the branch-leaf retain fix, and the
/// reason this fixture exists at all: the bug itself is INVISIBLE here.
///
/// Its fault is a stray refcount dec through a freed box, which writes
/// `-1` over the freed chunk rather than calling `free` twice, so nothing
/// at the allocator boundary sees it.
/// `option_shared_from_every_branch_leaf_form_survives_repeated_consumption`
/// in `tests/codegen.rs` is what gates the use-after-free.
///
/// B-2026-09-07-40 UPDATE: on the DEFAULT leg that is still exactly true —
/// `link_executable_with_sanitizer` passes `-fsanitize=address` at the link
/// step, which interposes the allocator and nothing more. The
/// `KARAC_SANITIZE_ADDRESS=1` leg (`scripts/asan-instrumented-leg.sh`) does
/// instrument the emitted object, so the write through the freed box IS
/// visible there. The E2E twin stays the named gate — it runs on every leg,
/// where this fixture's access coverage exists only on the opt-in one.
///
/// What LSan DOES gate is the obvious wrong fix. The defect is a missing
/// `+1`, so retaining harder makes the symptom go away — and a retain
/// applied at the phi, or applied to an arm whose producer already carries
/// its own `+1`, leaks instead of double-freeing. The mixed-arm leg here
/// takes each direction on alternating iterations, so an over-retain on
/// either side accumulates across the loop into a leak LSan reports. Same
/// for the `if let` leg, whose then-arm hook this fix added.
#[test]
fn asan_option_shared_branch_leaf_repeated_consumption_is_clean() {
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64 }

fn make(n: i64) -> Option[Node] { return Some(Node { val: n }); }

fn show(t: Option[Node]) -> i64 {
    match t { None => { return 0; } Some(n) => { return n.val; } }
}

fn main() {
    let mut s = 0;
    let mut i = 0;
    while i < 5 {
        // Plain `if`, alternating which arm is taken.
        let a = make(i);
        let b = make(i + 100);
        let t = if i % 2 == 0 { a } else { b };
        s = s + show(t) + show(t) + show(t);
        // MIXED arms: a borrowed binding against a fresh producer. The
        // over-retaining wrong fix leaks here, which is what LSan catches.
        let c = make(i + 1000);
        let u = if i % 2 == 0 { c } else { make(7) };
        s = s + show(u) + show(u) + show(u);
        // `if let`, alternating THEN and ELSE arms.
        let d = make(i + 10000);
        let e = make(i + 20000);
        let o = if i % 2 == 0 { Some(1) } else { None };
        let w = if let Some(z) = o { d } else { e };
        s = s + show(w) + show(w) + show(w);
        i = i + 1;
    }
    println(s);
}
"#,
        &["219720"],
        "option_shared_branch_leaf_repeated_consumption",
    );
}

#[test]
fn asan_option_shared_walk_unwrap_cursor_repeat() {
    // Regression for the walk-cursor refcount pair (2026-06-05):
    // (1) `Option[shared T]` variable-assign released the old inner
    // BEFORE retaining the new — `cur = node.next` freed the chain
    // out from under the cursor (UAF); (2) `let node = cur.unwrap()`
    // skipped the receive-inc (MethodCall misclassified as a fresh
    // +1 source) while still queueing the scope-exit dec — one
    // over-dec per iteration. Build + walk + drop repeated so a leak
    // (inverse failure: over-retain) trips LeakSanitizer too.
    assert_clean_asan_run(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn make() -> Option[ListNode] {
    let mut head = ListNode { val: 1, next: None };
    let second = ListNode { val: 2, next: None };
    head.next = Some(second);
    Some(head)
}
fn walk(head: Option[ListNode]) -> i64 {
    let mut cur = head;
    let mut sum = 0;
    while cur.is_some() {
        let node = cur.unwrap();
        sum = sum + node.val;
        cur = node.next;
    }
    sum
}
fn main() {
    let mut total: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 64i64 {
        total = total + walk(make());
        k = k + 1i64;
    }
    println(total);
}
"#,
        &["192"],
        "option_shared_walk_unwrap_cursor_repeat",
    );
}

/// B-2026-08-26-12 — an `Option[shared T]` binding handed out of a
/// VALUE-POSITION BRANCH LEAF (an `if`-expression arm, a `match`-expression
/// arm, a bare `{ }` block) into an OWNED parameter.
///
/// The leaf moved the binding by ALIAS: no retain was emitted, while the
/// source kept its scope-exit `RcDecOption` and the callee's param queued
/// its own. Two decs against the one ref `make` allocated — the callee's
/// took the box to rc 0 and freed it, and the source's then decremented
/// THROUGH the freed box. That write lands in the freed chunk's tcache `fd`
/// word on glibc, so a shipped binary aborts at the NEXT `malloc` with
/// `malloc(): unaligned tcache chunk detected` rather than at the fault —
/// hence the two loop iterations, which are what make the corruption
/// observable at all.
///
/// The `mixed` leg is the one that pins the fix's SHAPE. Its two arms are
/// not alike: `{ a }` hands out a borrowed binding and needs the `+1`,
/// `{ make(99) }` carries the producer's own `+1` and must not take a
/// second. Only a per-arm retain — emitted in the arm's own basic block —
/// satisfies both; a single retain at the phi either double frees one arm
/// or leaks the other.
///
/// READ THIS BEFORE TRUSTING IT: on the default leg this fixture is the
/// LEAK half of the gate and nothing more. It was MEASURED green against
/// the unfixed compiler — it does not catch the double free it is named
/// for. `link_executable_with_sanitizer` passes `-fsanitize=address` to
/// `cc` at LINK time only, so ASAN interposes the allocator but does not
/// instrument the Kāra-emitted object: it sees double free, invalid free
/// and leaks, and is blind to a use-after-free ACCESS. The sentence that
/// stood here — "and no ASAN fixture can" — was wrong about the tool rather
/// than about the fixture, and B-2026-09-07-40 removed the reason for it:
/// `KARAC_SANITIZE_ADDRESS=1` runs the `asan` pass over the module, and the
/// instrumented leg does see this access class. This bug's fault is
/// a stray refcount decrement THROUGH a freed box — one extra dec takes
/// the count to `-1`, not to a second `free` — so nothing crosses the
/// allocator boundary. The double-free half is gated E2E, by
/// `tests/codegen.rs::option_shared_through_every_branch_leaf_form_survives_a_rebinding_loop`.
/// What this fixture DOES gate is the inverse failure: over-retain a leaf
/// (the obvious wrong fix for the mixed arm) and LeakSanitizer fails it.
#[test]
fn asan_option_shared_through_branch_leaf_into_owned_param_no_double_free() {
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64 }
fn make(n: i64) -> Option[Node] { return Some(Node { val: n }); }
fn take(t: Option[Node]) -> i64 {
    match t { None => { return 0; } Some(n) => { return n.val; } }
}
fn main() {
    let mut s = 0i64;
    let mut total = 0i64;
    while s < 8i64 {
        let a = make(s);
        total = total + take(if s == 0i64 { a } else { a });
        let b = make(s);
        total = total + take(match s { 0 => b, _ => b });
        let c = make(s);
        total = total + take({ c });
        let d = make(s);
        total = total + take(if s < 4i64 { d } else { make(100i64) });
        s = s + 1i64;
    }
    println(total);
}
"#,
        // 3 * (0+1+..+7) = 84, plus the mixed leg: s<4 yields s (0+1+2+3=6),
        // s>=4 yields 100 four times (400) — 84 + 6 + 400 = 490.
        &["490"],
        "option_shared_through_branch_leaf_into_owned_param",
    );
}

#[test]
fn asan_option_shared_prepend_builder_rc_fallback_repeat() {
    // Regression for the RC-fallback boxing / `Option[shared T]`
    // collision (2026-06-05). The ownership checker flags the
    // prepend-builder's `head` for RC fallback; boxing redirected
    // the slot to a `{rc, Option}` heap ptr that the Option-assign /
    // arg-share / scope-exit paths misread as a raw Option struct
    // (32-byte store into the 8-byte slot — stack smash, then UAF
    // on the decoded-garbage tag). Option[shared] bindings are now
    // excluded from boxing. MUST run via the ownership-loaded
    // harness — the plain run never populates the RC-fallback set.
    assert_clean_asan_run_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn make(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = 0;
    while i < n {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i + 1;
    }
    head
}
fn walk(head: Option[ListNode]) -> i64 {
    let mut cur = head;
    let mut sum = 0;
    while cur.is_some() {
        let node = cur.unwrap();
        sum = sum + node.val;
        cur = node.next;
    }
    sum
}
fn main() {
    let mut total: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 32i64 {
        let chain = make(50);
        total = total + walk(chain);
        k = k + 1i64;
    }
    println(total);
}
"#,
        "option_shared_prepend_builder_rc_fallback_repeat",
    );
}

#[test]
fn asan_option_shared_method_tail_field_step_repeat() {
    // Method niche-ABI extension (2026-06-05): `node.step()` where
    // `step(ref self) -> Option[ListNode] { self.next }` is a tail
    // field return from a BORROWED receiver, looped with fresh
    // chains, with the receiver's chain summed afterwards. Pins
    // three fixes under ASAN:
    //   1. ref-rooted tail field returns are NOT move-out zeroed
    //      (the zeroing also wrote through the un-deref'd ref-param
    //      slot into the caller's stack frame);
    //   2. the returned alias carries its own +1 (the ref-rooted
    //      FieldAccess arm in `compile_tail_final_expr`);
    //   3. method arg loops share-inc tracked `Option[shared]` args
    //      (`m.total(...)` consumes through the niche-ABI method
    //      param without stealing the caller's ref).
    assert_clean_asan_run(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
shared struct Merger { count: i64 }
impl ListNode {
    fn build(n: i64) -> Option[ListNode] {
        let mut head: Option[ListNode] = None;
        let mut i = n;
        while i > 0 {
            let node = ListNode { val: i, next: head };
            head = Some(node);
            i = i - 1;
        }
        head
    }
    fn step(ref self) -> Option[ListNode] { self.next }
}
impl Merger {
    fn total(ref self, head: Option[ListNode]) -> i64 {
        let mut t = 0;
        let mut cur = head;
        while cur.is_some() {
            let n = cur.unwrap();
            t = t + n.val;
            cur = n.next;
        }
        t
    }
}
fn main() {
    let m = Merger { count: 0 };
    let mut total = 0;
    let mut iter = 0;
    while iter < 50 {
        let chain = ListNode.build(50);
        let node = chain.unwrap();
        let stepped = node.step();
        total = total + m.total(stepped);
        total = total + m.total(chain);
        iter = iter + 1;
    }
    println(total);
}
"#,
        &["127450"],
        "option_shared_method_tail_field_step_repeat",
    );
}

#[test]
fn asan_option_shared_field_let_alias_repeat() {
    // `let stepped = node.next;` — Identifier-object field read
    // bound by an untyped let (case (c)). The registration queued a
    // scope-exit dec with no balancing inc: stepped's dec freed the
    // sub-chain the field still owned, and the owner's drop walked
    // freed memory — LATENT on main (masked by garbage rc-words
    // stopping the walk) until the niche-ABI allocation shift made
    // it trap. Now takes the case-(d) aliasing-acquire +1. Summing
    // both the alias and the original chain catches both failure
    // directions under ASAN.
    assert_clean_asan_run(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = n;
    while i > 0 {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i - 1;
    }
    head
}
fn sum(head: Option[ListNode]) -> i64 {
    let mut t = 0;
    let mut cur = head;
    while cur.is_some() {
        let n = cur.unwrap();
        t = t + n.val;
        cur = n.next;
    }
    t
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 50 {
        let chain = build(50);
        let node = chain.unwrap();
        let stepped = node.next;
        total = total + sum(stepped);
        total = total + sum(chain);
        iter = iter + 1;
    }
    println(total);
}
"#,
        &["127450"],
        "option_shared_field_let_alias_repeat",
    );
}

#[test]
fn asan_option_shared_owned_self_receiver_repeat() {
    // Owned-`self` shared receiver (the bugs.md receiver-move
    // segfault): the usermethod dispatch used to pass the stack-slot
    // address where owned-shared `self` expects the heap pointer —
    // the callee's receive-inc corrupted a stack word; and the tail
    // `self.next` zeroing severed the caller's list. Fixed pair
    // pinned under ASAN: receiver discriminated via the source-level
    // ref flag, tail field returns take the loaded-inner inc. The
    // post-call `m_total(chain)` read proves non-destructive reads;
    // the loop catches drift both directions.
    assert_clean_asan_run(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
impl ListNode {
    fn step(self) -> Option[ListNode] { self.next }
    fn value(self) -> i64 { self.val }
}
fn make(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = n;
    while i > 0 {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i - 1;
    }
    head
}
fn sum(head: Option[ListNode]) -> i64 {
    let mut t = 0;
    let mut cur = head;
    while cur.is_some() {
        let n = cur.unwrap();
        t = t + n.val;
        cur = n.next;
    }
    t
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 50 {
        let chain = make(50);
        let node = chain.unwrap();
        total = total + node.value();
        let rest = node.step();
        total = total + sum(rest);
        total = total + sum(chain);
        iter = iter + 1;
    }
    println(total);
}
"#,
        &["127500"],
        "option_shared_owned_self_receiver_repeat",
    );
}

#[test]
fn asan_option_shared_niche_abi_convergence_repeat() {
    // Niche call ABI for `Option[shared T]` signatures (Slice 1,
    // 2026-06-05) + the explicit-return alias compensation it
    // surfaced. One loop exercising every convergence point under
    // ASAN: chained call-result args (`ident(make(...))` packs and
    // unpacks at each boundary), explicit `return head;` /
    // `return node.next;` aliases (each needs the Return-arm +1 so
    // the param's scope-exit dec doesn't free the returned chain),
    // recursion (`nth`), and the `?` operator (shared-typed `let`
    // from `q_w0` + null early-return through the niche). Repeats
    // catch both failure directions: UAF (under-count) trips ASAN,
    // leak (over-count) trips LeakSanitizer on platforms that have
    // it.
    assert_clean_asan_run(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn make(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = n;
    while i > 0 {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i - 1;
    }
    head
}
fn ident(head: Option[ListNode]) -> Option[ListNode] { head }
fn ret_field(head: Option[ListNode]) -> Option[ListNode] {
    if head.is_some() {
        let node = head.unwrap();
        return node.next;
    }
    return None;
}
fn nth(head: Option[ListNode], k: i64) -> Option[ListNode] {
    if k == 0 {
        return head;
    }
    if head.is_none() {
        return None;
    }
    let node = head.unwrap();
    nth(node.next, k - 1)
}
fn second(head: Option[ListNode]) -> Option[ListNode] {
    let first = head?;
    let rest = first.next?;
    Some(rest)
}
fn sum(head: Option[ListNode]) -> i64 {
    let mut total = 0;
    let mut cur = head;
    while cur.is_some() {
        let node = cur.unwrap();
        total = total + node.val;
        cur = node.next;
    }
    total
}
fn main() {
    let mut total: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 32i64 {
        total = total + sum(ident(make(10)));
        total = total + sum(ret_field(make(10)));
        total = total + sum(nth(make(10), 4));
        total = total + sum(second(make(10)));
        k = k + 1i64;
    }
    println(total);
}
"#,
        &["6656"],
        "option_shared_niche_abi_convergence_repeat",
    );
}

#[test]
fn asan_try_clone_question_unwrap_single_free() {
    // The `?`-unwrap of a `try_clone` result yields an owned Vec the callee
    // returns through; the source and the unwrapped clone each free once.
    assert_clean_asan_run(
        r#"
fn dup() -> Result[i64, AllocError] {
    let mut v: Vec[String] = Vec.new();
    v.push(f"x{1}");
    v.push(f"y{2}");
    let c: Vec[String] = v.try_clone()?;
    Ok(c.len())
}

fn main() {
    match dup() {
        Ok(n) => println(n),
        Err(_) => println("err"),
    }
}
"#,
        &["2"],
        "try_clone_question_unwrap_single_free",
    );
}

#[test]
fn asan_letbound_result_option_heap_unwrap_no_double_free() {
    // B-2026-07-10-2: `unwrap`/`unwrap_err`/`expect` on a LET-BOUND
    // Option/Result receiver with a HEAP payload. The extracted String is a
    // shallow alias of the receiver's inline buffer; unwrap CONSUMES the
    // receiver, so its scope-exit drop must be disarmed or it double-frees the
    // buffer the returned value now owns. Fix:
    // `suppress_inline_option_result_binding_move` zeros the tracked receiver
    // slot. Looped x20 under LSan.
    assert_clean_asan_run(
        r#"
fn rok(ok: bool) -> Result[String, i64] {
    if ok { Result.Ok("the ok payload padded out".to_string()) } else { Result.Err(1) }
}
fn rerr(ok: bool) -> Result[i64, String] {
    if ok { Result.Ok(1) } else { Result.Err("the err payload padded out".to_string()) }
}
fn opt(some: bool) -> Option[String] {
    if some { Some("the some payload padded".to_string()) } else { None }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        let a = rok(true);
        let sa = a.unwrap();
        let b = rerr(false);
        let sb = b.unwrap_err();
        let c = opt(true);
        let sc = c.expect("wanted some");
        total = total + (sa.len() as i64) + (sb.len() as i64) + (sc.len() as i64);
        i = i + 1;
    }
    println(total);
}
"#,
        &["1480"],
        "letbound_result_option_heap_unwrap_no_double_free",
    );
}

/// slice-3c-iv: a heap-BOXED `Option[Wide]` local moved whole into a
/// struct literal (`Holder { body: body }` for `let mut body =
/// Some(mk_wide())`) must transfer the box's ownership to the new struct.
/// Before the fix the builder fn's `BoxedEnumDrop` freed the box at scope
/// exit while the returned `Holder` still referenced it — a use-after-free
/// the reader then dereferenced (selfhost slice 3c-iv's `parse_trait_method`
/// → `render_block` garbage / SIGSEGV). The boxed `Wide` owns a ≥36-byte
/// `String` so a wrongly-freed-or-leaked box is visible to LSan (Linux) and
/// the UAF read is caught by macOS ASAN; the 50-iter loop forces allocator
/// reuse so a dangling box reads back corrupted.
#[test]
fn asan_boxed_option_moved_into_struct_literal_no_uaf() {
    assert_clean_asan_run(
            "struct Wide { tag: i64, payload: String }\n\
             struct Holder { name: String, body: Option[Wide] }\n\
             fn mk_wide() -> Wide {\n\
             \x20   Wide { tag: 7, payload: \"boxed-option-payload-string-long-enough-aaaa\".to_string() }\n\
             }\n\
             fn build() -> Holder {\n\
             \x20   let mut body: Option[Wide] = None;\n\
             \x20   body = Some(mk_wide());\n\
             \x20   Holder { name: \"holder-name-payload-string-long-enough\".to_string(), body: body }\n\
             }\n\
             fn read(h: Holder) -> i64 {\n\
             \x20   let Holder { name, body } = h;\n\
             \x20   let n = name.len() as i64;\n\
             \x20   match body {\n\
             \x20       Some(w) => { let Wide { tag, payload } = w; tag + (payload.len() as i64) + n }\n\
             \x20       None => n,\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let mut acc = 0i64;\n\
             \x20   let mut i = 0;\n\
             \x20   while i < 50 {\n\
             \x20       acc = acc + read(build());\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   println(acc);\n\
             }\n",
            &["4450"],
            "asan_boxed_option_moved_into_struct_literal_no_uaf",
        );
}

#[test]
fn asan_getmove_get_or_result_binding_no_double_free() {
    // Slice 3s adjacency probe: `let s = m.get_or(k, default)` byte-copies
    // the stored value out as the RESULT — the binding must own an
    // independent copy (or the map's value walk double-frees).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, String] = Map.new();
        m.insert(i, f"map string payload padded beyond thirty-six bytes {i}");
        let s = m.get_or(i, f"default-{i}");
        println(s.len());
        i = i + 1;
    };
}
"#,
        &["51", "51", "51"],
        "getmove_get_or_result_binding_no_double_free",
    );
}

#[test]
fn asan_tail3v_vecdeque_option_elem_no_leak() {
    // Slice 3v: `Vec[Option[VecDeque[String]]]` — the VecDeque admission
    // must reach through the Option payload gates too.
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> Vec[Option[VecDeque[String]]] {
    let mut d: VecDeque[String] = VecDeque.new();
    d.push_back(f"deque payload padded beyond thirty-six bytes {n}");
    let mut v: Vec[Option[VecDeque[String]]] = Vec.new();
    v.push(Some(d));
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
        &["1", "1", "1"],
        "tail3v_vecdeque_option_elem_no_leak",
    );
}

/// B-2026-07-03-28 Facet A — plain-drop of a struct whose only heap is an
/// `Option[String]` field (a `Vec[A]` element). The struct is copy-supported,
/// so it is callee-owned and its synthesized struct drop frees the `Some`
/// payload (`OptionInline`); before the fix the payload leaked.
#[test]
fn asan_option_field_plain_drop_freed() {
    assert_clean_asan_run(
        r#"
struct A { sv: Option[String] }
fn main() {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 { v.push(A { sv: Some("facet_a_plaindrop_option_string_payload_x".to_string()) }); i = i + 1; }
    println(v.len());
}
"#,
        &["6"],
        "option_field_plain_drop_freed",
    );
}

/// B-2026-07-03-28 Facet A — `let x = a.sv` moves the `Option` field out of a
/// callee-owned struct; the field-access move-out zeros the source tag so the
/// struct drop skips it, and `x` owns the payload. No double-free, no leak.
#[test]
fn asan_option_field_moveout_clean() {
    assert_clean_asan_run(
        r#"
struct A { keep: i64, sv: Option[String] }
fn f(a: A) -> i64 {
    let x = a.sv;
    match x { Some(s) => { if s.len() >= 0 { 1 } else { 0 } } None => 0 }
}
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 { v.push(A { keep: i, sv: Some("facet_a_field_moveout_option_payload_eta_ee".to_string()) }); i = i + 1; }
    v
}
fn main() {
    let xs = build();
    let mut t = 0;
    for a in xs { t = t + f(a); }
    println(t);
}
"#,
        &["6"],
        "option_field_moveout_clean",
    );
}

/// B-2026-07-10-3 — a `Result`/`Option` scrutinee whose INLINE struct payload
/// is bound WHOLE as `e` and a heap field is read as a DIRECT call argument
/// (`println(e.msg)`). The inline `Result`/`Option` cleanup frees only a bare
/// `{ptr,len,cap}` payload, not a struct payload's own fields, and the
/// consuming-arm suppressor zeroed the source anyway — so `e.msg` was owned by
/// nobody and leaked. The bound struct is now `track_struct_var`-tracked so its
/// scope-exit drop frees the field. Covers the fresh-temp scrutinee (this test),
/// the `Option` sibling, the `{code,msg}` 4-word inline-`Result` payload (still
/// inline for `Result`'s area of 5), and the move-out shapes that must NOT
/// double-free (whole `e` into a by-value callee; whole `e` out as the match
/// value).
#[test]
fn asan_result_struct_payload_direct_field_arg_no_leak() {
    assert_clean_asan_run(
        r#"
struct AppError { msg: String }
struct E2 { code: i64, msg: String }
fn run(x: i64) -> Result[i64, AppError] {
    if x > 0i64 { Result.Ok(x + 1i64) } else { Result.Err(AppError { msg: "neg_error_payload_alpha_aaaaaaaa".to_string() }) }
}
fn run_opt(x: i64) -> Option[AppError] {
    if x > 0i64 { Option.None } else { Option.Some(AppError { msg: "neg_option_payload_beta_bbbbbbbb".to_string() }) }
}
fn run2(x: i64) -> Result[i64, E2] {
    if x > 0i64 { Result.Ok(x + 1i64) } else { Result.Err(E2 { code: 7i64, msg: "neg_error_two_field_gamma_cccccccc".to_string() }) }
}
fn take(a: AppError) -> i64 { if a.msg.len() >= 0 { 1 } else { 0 } }
fn main() {
    let mut t = 0;
    // direct-field-arg read (the reported leak)
    match run(-1i64) { Ok(v) => { if v >= 0i64 { t = t + 1; } } Err(e) => { if e.msg.len() >= 0 { t = t + 1; } } }
    // Option sibling
    match run_opt(-1i64) { Some(e) => { if e.msg.len() >= 0 { t = t + 1; } } None => {} }
    // 4-word inline-Result struct payload
    match run2(-1i64) { Ok(v) => { if v >= 0i64 { t = t + 1; } } Err(e) => { if e.msg.len() >= 0 { t = t + 1; } } }
    // move whole `e` into a by-value callee (must not double-free)
    match run(-1i64) { Ok(v) => { if v >= 0i64 { t = t + 1; } } Err(e) => { t = t + take(e); } }
    // move whole `e` out as the match value (must not double-free)
    let held = match run(-1i64) { Ok(_v) => AppError { msg: "ok_payload_delta_dddddddddddd".to_string() }, Err(e) => e };
    if held.msg.len() >= 0 { t = t + 1; }
    println(t);
}
"#,
        &["5"],
        "result_struct_payload_direct_field_arg",
    );
}

#[test]
#[ignore = "B-2026-08-05-35 sweep: does not compile — chained field receivers (`a.b.c…`) are deferred to v1.x. Silently SKIPPED until the harness learned to fail on a codegen error."]
fn asan_forloop_struct_element_nested_and_option_field_no_double_free() {
    // B-2026-07-04-17 variants named in the ledger: a NESTED heap struct
    // field and an `Option[String]` field. Moving such an element to a new
    // owner deep-copies recursively (copy-depth == drop-depth), so neither
    // the nested String nor the Option payload double-frees.
    assert_clean_asan_run(
        r#"
struct Inner { s: String }
struct Outer { inner: Inner, tag: Option[String] }
fn build() -> Vec[Outer] {
    let mut v: Vec[Outer] = Vec.new();
    let mut i = 0;
    while i < 5 {
        v.push(Outer {
            inner: Inner { s: "nested_inner_heap_payload_kappa_field_xx".to_string() },
            tag: Some("outer_option_string_payload_lambda_field_yy".to_string()),
        });
        i = i + 1;
    }
    v
}
fn main() {
    let items = build();
    let mut n: i64 = 0;
    for a in items {
        let x = a;
        n = n + x.inner.s.len();
        match x.tag { Some(t) => { n = n + t.len(); } None => {} }
    }
    println(n);
}
"#,
        &["420"], // 5 * (40 + 44)
        "forloop_struct_element_nested_and_option_field_no_double_free",
    );
}

#[test]
fn asan_if_expr_option_shared_reused_no_use_after_free() {
    // B-2026-07-16-8: an `Option[shared]` bound from an `if`/`match`/block
    // EXPRESSION (`let root = if c { mk() } else { mk() }`) passed BY VALUE
    // more than once. The `shared_option_info` detection block only matched a
    // single RHS shape (Call / Some / Index / FieldAccess / Identifier) and
    // never looked inside a control-flow expression, so the binding was left
    // UNREGISTERED: no call-site retain, no scope-exit dec. The first `readv`
    // call's param drop freed the node, the second read freed memory — a
    // use-after-free (interpreter correct; JIT+AOT garbage / heap corruption).
    // Same class as B-2026-07-11-21 (`Some(..)` RHS) / -29 (`v[i]` RHS),
    // extended to control-flow-expression RHSs (case g). 50*(7+7) + 50*(9+9)
    // = 1600.
    assert_clean_asan_run(
        r#"
shared struct N { val: i64, mut left: Option[N], mut right: Option[N] }
fn mk(v: i64) -> Option[N] { Some(N { val: v, left: None, right: None }) }
fn readv(r: Option[N]) -> i64 { match r { None => 0i64, Some(n) => n.val } }
fn main() {
    let mut acc = 0i64;
    let mut i = 0i64;
    while i < 100i64 {
        let root = if (i % 2i64) == 0i64 { mk(7i64) } else { mk(9i64) };
        acc = acc + readv(root) + readv(root);
        i = i + 1;
    }
    println(acc)
}
"#,
        &["1600"],
        "if_expr_option_shared_reused_no_use_after_free",
    );
}

/// B-2026-09-07-54 — an untyped `let` whose RHS is `<receiver>.clone()`
/// yielding `Option[shared T]` was never registered into the caller-retains
/// model, so REUSING the binding over-decremented the payload.
///
/// The arithmetic, which is what makes each cell's outcome predictable
/// rather than incidental. `push` leaves the element at rc 1 and `.clone()`
/// retains it to 2 — `emit_option_value_clone_fn` admits a shared payload
/// precisely so the copy gets "an independent +1". Each by-value pass into
/// `clone_offset` should then be an arg-site inc against the callee's exit
/// dec, but an unregistered binding gets no inc while the callee decs
/// anyway, so the count after N passes is 2 - N:
///
///   1 pass  → rc 1. Balanced by luck. Cell (c) is clean pre-fix and is here
///             to say so: a one-pass fixture cannot see this defect at all.
///   2 passes → rc 0. The payload is freed while `src` still holds the
///             element, and the container's teardown
///             `__karac_vec_elem_rc_dec_Node` then READS the freed control
///             block. A read and nothing more — no double free, no leak —
///             which is exactly why every gate this repo ran before the
///             instrumented leg reported this family clean.
///
/// SO ONLY THE INSTRUMENTED LEG GATES THIS TEST. Both live cells are
/// read-only violations, and `KARAC_SANITIZE_ADDRESS=1` (B-2026-09-07-40) is
/// what makes a read through freed memory visible; the default link-only
/// `-fsanitize=address` buys allocator interposition only. Measured on the
/// parent: this test PASSES on the default leg and fails on the instrumented
/// one with `heap-use-after-free ... in __karac_vec_elem_rc_dec_Node`. Do not
/// read a green default-leg run of this test as evidence of anything.
///
/// THE THREE-PASS ESCALATION IS DELIBERATELY NOT A CELL HERE, and the reason
/// is worth recording. At three passes rc goes to -1 and the block is handed
/// to the allocator a SECOND time; under glibc that is `Invalid free()` and
/// the process aborts with `malloc(): unaligned tcache chunk detected`. That
/// IS an allocator event, so it looked like the cell that would finally gate
/// this family on the leg CI runs — and it is not: measured on the parent,
/// this test with a three-pass cell added PASSES the DEFAULT leg of this
/// suite outright, because ASAN replaces the allocator and its quarantine
/// changes what the second free does. (The instrumented leg never reaches
/// that cell — it stops at cell (a) — so what is measured is the default
/// leg, which is precisely the leg the cell was supposed to gate.) A cell
/// that passes pre-fix pins nothing. The escalation is gated by
/// `e2e_cloned_option_shared_binding_survives_repeated_reuse`
/// (tests/codegen.rs) instead, which runs the binary under the NATIVE
/// allocator where the abort is real.
///
/// Cell (d) is the control that localizes the defect to the REGISTRATION
/// rather than to `.clone()`: the same program with the binding ANNOTATED
/// (`let s: Option[Node] = src[0].clone()`) is clean pre-fix, because the
/// annotation reaches case (a) and registers. Only the untyped spelling
/// falls through every case.
///
/// Cell (b) is why the fix keys on the CLONE and not on the index: a
/// `.clone()` over a tracked `Option[shared]` binding has the identical hole.
///
/// Worth recording next to the fixture: the bare spelling the original case
/// (f) was written for (`let s = src[0];`) no longer compiles — the
/// typechecker rejects it with `E_INDEX_MOVE_NON_COPY`, and that
/// diagnostic's own remedy text names `v[i].clone()`. So every user who
/// follows the compiler's advice lands on the spelling that was
/// unregistered.
#[test]
fn asan_cloned_option_shared_binding_is_owned_on_every_reuse() {
    // (a) The index-receiver spelling, two passes — the row's report.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn clone_offset(node: Option[Node], delta: i64) -> Option[Node] {
    match node {
        None => None,
        Some(n) => Some(Node { val: n.val + delta, left: clone_offset(n.left, delta), right: clone_offset(n.right, delta) }),
    }
}
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None }));
    let s = src[0].clone();
    let l0 = clone_offset(s, 10);
    let l1 = clone_offset(s, 20);
    println(count_nodes(l0) + count_nodes(l1));
}
"#,
        &["4"],
        "b0907-54-index-clone-two-passes",
    );
    // (b) The same hole one receiver spelling over: `.clone()` on a tracked
    // `Option[shared]` BINDING.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn clone_offset(node: Option[Node], delta: i64) -> Option[Node] {
    match node {
        None => None,
        Some(n) => Some(Node { val: n.val + delta, left: clone_offset(n.left, delta), right: clone_offset(n.right, delta) }),
    }
}
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None }));
    let base: Option[Node] = src[0].clone();
    let s = base.clone();
    let l0 = clone_offset(s, 10);
    let l1 = clone_offset(s, 20);
    println(count_nodes(l0) + count_nodes(l1));
}
"#,
        &["4"],
        "b0907-54-binding-clone-two-passes",
    );
    // (c) ONE pass — clean before the fix and after it. Present so a future
    // reduction cannot lose the fact that this defect needs REUSE.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn clone_offset(node: Option[Node], delta: i64) -> Option[Node] {
    match node {
        None => None,
        Some(n) => Some(Node { val: n.val + delta, left: clone_offset(n.left, delta), right: clone_offset(n.right, delta) }),
    }
}
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None }));
    let s = src[0].clone();
    let l0 = clone_offset(s, 10);
    println(count_nodes(l0));
}
"#,
        &["2"],
        "b0907-54-index-clone-one-pass",
    );
    // (d) The ANNOTATED control — clean before the fix, via case (a).
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn clone_offset(node: Option[Node], delta: i64) -> Option[Node] {
    match node {
        None => None,
        Some(n) => Some(Node { val: n.val + delta, left: clone_offset(n.left, delta), right: clone_offset(n.right, delta) }),
    }
}
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None }));
    let s: Option[Node] = src[0].clone();
    let l0 = clone_offset(s, 10);
    let l1 = clone_offset(s, 20);
    println(count_nodes(l0) + count_nodes(l1));
}
"#,
        &["4"],
        "b0907-54-annotated-clone-control",
    );
}

/// B-2026-08-05-7 — the heap BOX behind an `Option`/`Result` payload of a
/// USER enum. `payload_word_count_for_type_expr`'s enum-in-enum carve-out
/// sizes such a payload at ONE word while its real LLVM width is four
/// (`Option`) or six (`Result`), so `coerce_to_payload_words` ALWAYS boxes
/// it and parks the pointer in that word. Nothing owned the box: the drop
/// switch classified the field `None` and emitted no cleanup at all, the
/// match debox in `reconstruct_payload_value` only LOADS through the
/// pointer, and `?`'s free is a different carrier. So EVERY construction of
/// such a variant leaked 32 bytes (48 for `Result`) — heap payload or not,
/// matched out or not.
///
/// String payloads here are LITERALS (cap 0, `.rodata`) on purpose. The new
/// drop is BOX-ONLY: the interior belongs to whoever matched it out, and
/// freeing it here would double-free the buffer a `W(Some(s))` arm's `s`
/// already owns. Reclaiming a heap interior that is NEVER matched out needs
/// the entry-copy to duplicate the box and is a separate slice — so this
/// fixture gates the box and says so rather than pretending otherwise.
///
/// The whole-value MOVE (`let w2 = w;`) is the double-free direction and is
/// exercised deliberately: it hands the destination the same box pointer,
/// and until `zero_enum_payload_caps` learned this kind it aborted under
/// ASAN the moment the drop existed. The by-value call and the `Empty`
/// (no-box) variant are the other two directions — a param registers no
/// drop, and a null box must run nothing.
///
/// The FRESH-TEMP enum argument (`take(Wrap.N(Some(2)))`) is here because it
/// was the first casualty of the new drop and needed its own fix
/// (B-2026-08-06-9 leg B): the temp has no binding, and the call site's
/// registrar asked `enum_has_heap_payload`, which answers false for a kind
/// that is not heap-BEARING even though its drop switch frees a box. It now
/// asks `enum_drop_switch_does_work` instead. Measured 320 B / 10 before
/// that fix.
///
/// STILL NOT COVERED: a NAMED `Option` binding passed by value
/// (`let bound = Some(Some(1)); classify(bound);`) leaks its box — the call
/// site's move-zeroing disarms the let-site `BoxedEnumDrop` and the callee
/// takes nothing over. Pre-existing and untouched here; B-2026-08-06-9
/// leg A.
#[test]
fn asan_enum_boxed_optres_payload_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
enum Wrap {
    W(Option[String]),
    N(Option[i64]),
    R(Result[String, i64]),
    Empty,
}

fn take(w: Wrap) -> i64 {
    match w {
        Wrap.W(Option.Some(s)) => s.len(),
        Wrap.W(Option.None) => -1,
        Wrap.N(Option.Some(n)) => n,
        Wrap.N(Option.None) => -2,
        Wrap.R(Result.Ok(s)) => s.len(),
        Wrap.R(Result.Err(e)) => e,
        Wrap.Empty => -3,
    }
}

fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        // matched out
        let a: Wrap = Wrap.W(Option.Some("boxed payload well past inline width"));
        match a {
            Wrap.W(Option.Some(s)) => { acc = acc + s.len(); },
            _ => { acc = acc - 1; },
        }
        // NEVER matched out — the box must still be freed
        let b: Wrap = Wrap.N(Option.Some(7));
        match b {
            Wrap.Empty => { acc = acc - 1; },
            _ => { acc = acc + 1; },
        }
        // whole-value MOVE — source and destination share one box
        let c: Wrap = Wrap.R(Result.Ok("boxed result payload past inline width"));
        let c2 = c;
        acc = acc + take(c2);
        // a FRESH TEMP enum by value — no binding to hang a drop on, so the
        // call site is the only frame that can own its box
        acc = acc + take(Wrap.N(Option.Some(2)));
        // no box at all — the null-box direction through the same callee
        acc = acc + take(Wrap.Empty);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["2960"],
        "enum_boxed_optres_payload",
    );
}

/// B-2026-08-05-7 — the ARGUMENT leg of the same family: a fresh-temp
/// `Option[T]` with a heap-BOXED payload handed straight to an owned param.
/// A NAMED binding gets its box drop at the let site
/// (`track_boxed_enum_var`) and a fresh-temp SCRUTINEE gets one from
/// `materialize_freshtemp_enum_scrutinee`; `classify(Some(Some(42)))` had
/// neither, and a param registers no drop of its own, so the box leaked once
/// per call.
///
/// `Option.None` is the null-box direction — nothing to free.
///
/// NOT covered here when this landed: passing a NAMED `Option` binding by
/// value (`let bound = Some(Some(1)); classify(bound);`), which leaked its
/// box for a different owner reason. Closed as B-2026-08-06-9 leg A, which
/// MOVED the owner of this fixture's shape too — the box is now the
/// callee's, not the caller's, so this fixture is the over-suppression
/// control for that change. Its sibling below carries the rest.
#[test]
fn asan_freshtemp_boxed_option_arg_no_leak() {
    assert_clean_asan_run(
        r#"
fn classify(v: Option[Option[i64]]) -> i64 {
    match v {
        Option.Some(Option.Some(x)) => x,
        Option.Some(Option.None) => -1,
        Option.None => -2,
    }
}

fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        // the leaking shape: fresh temp into an owned param
        acc = acc + classify(Option.Some(Option.Some(42)));
        acc = acc + classify(Option.Some(Option.None));
        // null box — nothing to free
        acc = acc + classify(Option.None);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["1560"],
        "freshtemp_boxed_option_arg",
    );
}

/// B-2026-08-06-9 leg A — a NAMED `Option` binding whose payload is
/// heap-BOXED, passed BY VALUE to a callee that consumes it.
///
/// `Option[Option[i64]]`'s inner value is 4 LLVM words against the seeded
/// 3-word payload area, so `coerce_to_payload_words` boxes it. The caller
/// then runs `suppress_inline_option_result_binding_move`, which zeroes the
/// whole source slot — correct for the INLINE payload it was written for,
/// but for a BOXED one it disarms the let site's `BoxedEnumDrop` while the
/// callee registered nothing. Neither frame owned the box: 32 B per call.
///
/// The fix gives the CALLEE the box, which is the only frame that can own
/// it here, and every arm below exists because moving that ownership broke
/// something a narrower reading missed. Each was measured, not assumed:
///
///   * NAMED binding consumed — the headline leak.
///   * FRESH temp — the caller-side arm that used to own this
///     (B-2026-08-05-7) is retracted for non-struct payloads in the same
///     change, so this is the double-free control for the retraction and
///     the leak control for going too far.
///   * BOUND PASSTHROUGH then consumed (`let a = idnest(src);
///     classify(a);`). The result binding owns nothing — B-2026-08-06-21
///     skips its registration and leaves the source sole owner — so the
///     consume has to disarm the SOURCE's box. Without that this aborts
///     with a glibc double free.
///   * CHAINED passthrough (two bindings deep), because the alias is
///     resolved at the record site rather than walked at lookup; a version
///     that resolved only one hop aborted here.
///   * DISCARDED passthrough (`idnest(d);`) — the source IS the only owner
///     there, and B-2026-08-06-21 measured a 320-byte leak from zeroing it.
///     This is the over-suppression control for the disarm above.
///   * `None` sources, the direction where an over-eager disarm strands a
///     payload rather than double-freeing it.
///
/// Each consuming arm runs TWICE over the same shape: once with a SCALAR
/// inner (`Option[Option[i64]]`, the row's own reduction) and once with a
/// HEAP-BEARING one (`Option[Option[String]]`). The box-only free this
/// registers is identical for both, but only the second can show an
/// interior that the box drop wrongly took with it.
///
/// NOT COVERED, and deliberately: a boxed payload that is a user STRUCT.
/// Its box already has a callee-side owner (B-2026-08-06-10's move-out
/// mirror), and registering for it too double-frees
/// `asan_boxed_option_param_payload_move_out`,
/// `asan_owned_struct_optres_field_call_arg_move` and
/// `asan_shared_payload_view_optres_field_call_arg_copy` at both opt
/// levels — those three ARE the exclusion, written as programs. A NAMED
/// `Option[StructWide]` binding consumed by a read-only callee still leaks
/// its box; that is filed separately rather than papered over here.
///
/// COVERAGE, stated because it is weaker than the assertion looks. Against
/// the PRE-FIX compiler this leaks 2,560 B in 80 allocations at
/// `KARAC_OPT_LEVEL=0` and is CLEAN at the default `-O2`, where every box
/// folds away — so the memory half is carried entirely by the `-O0` leg
/// (`scripts/asan-o0-leg.sh`, B-2026-08-04-17), which is what that leg
/// exists for. What the `-O2` run still asserts is the accumulated total
/// and the allocation floor, so a leak traded for a wrong value, or a
/// fixture optimized into nothing, fails there.
///
/// The expected value is COMPUTED, not read off a run: every payload is
/// seeded from the opaque `env.args().len()`, each scalar arm subtracts the
/// seed back out to leave `i`, and each String arm contributes 1 for a
/// successful byte read — `4i + 5` per iteration, so
/// `4 * (0+…+39) + 40 * 5 = 3320`.
#[test]
fn asan_named_boxed_option_binding_consumed_by_callee_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
fn classify(v: Option[Option[i64]]) -> i64 {
    match v {
        Option.Some(Option.Some(x)) => x,
        Option.Some(Option.None) => -1,
        Option.None => -2,
    }
}
fn idnest(o: Option[Option[i64]]) -> Option[Option[i64]] { o }

fn classifys(v: Option[Option[String]]) -> i64 {
    match v {
        Option.Some(Option.Some(s)) => { if s.contains("row") { 1 } else { 0 } }
        Option.Some(Option.None) => -1,
        Option.None => -2,
    }
}
fn idnests(o: Option[Option[String]]) -> Option[Option[String]] { o }

fn main() {
    let n = env.args().len() as i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40 {
        // A — the headline: a named binding consumed by a callee.
        let bound: Option[Option[i64]] = Option.Some(Option.Some(n + i));
        acc = acc + classify(bound) - n;
        // B — fresh temp; the caller-side owner was retracted for this shape.
        acc = acc + classify(Option.Some(Option.Some(n + i))) - n;
        // C — bound passthrough, then consumed.
        let src: Option[Option[i64]] = Option.Some(Option.Some(n + i));
        let aka: Option[Option[i64]] = idnest(src);
        acc = acc + classify(aka) - n;
        // D — chained passthrough, then consumed.
        let s2: Option[Option[i64]] = Option.Some(Option.Some(n + i));
        let a1: Option[Option[i64]] = idnest(s2);
        let a2: Option[Option[i64]] = idnest(a1);
        acc = acc + classify(a2) - n;
        // E — discarded passthrough: the source stays sole owner.
        let d: Option[Option[i64]] = Option.Some(Option.Some(n + i));
        idnest(d);
        acc = acc + 1;
        // F — None source: nothing to free on either side.
        let z: Option[Option[i64]] = Option.None;
        acc = acc + classify(z) + 2;
        // A' .. D' — the same four consuming arms over a HEAP-BEARING inner,
        // so a box drop that took the interior with it shows up as a
        // double free and one that never ran shows up as a leak.
        let hb: Option[Option[String]] = Option.Some(Option.Some("row-" + (n + i).to_string()));
        acc = acc + classifys(hb);
        acc = acc + classifys(Option.Some(Option.Some("row-" + (n + i).to_string())));
        let hs: Option[Option[String]] = Option.Some(Option.Some("row-" + (n + i).to_string()));
        let hk: Option[Option[String]] = idnests(hs);
        acc = acc + classifys(hk);
        let h2: Option[Option[String]] = Option.Some(Option.Some("row-" + (n + i).to_string()));
        let g1: Option[Option[String]] = idnests(h2);
        let g2: Option[Option[String]] = idnests(g1);
        acc = acc + classifys(g2);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["3320"],
        "named_boxed_option_binding_consumed_by_callee",
        30,
    );
}

#[test]
fn asan_struct_field_boxed_heapless_option_envelope_owned() {
    assert_clean_asan_run_min_allocs(
        r#"
struct W { o: Option[Option[i64]] }
struct W2 { o: Option[Option[String]] }
struct Wide { a: i64, b: i64, c: i64, d: i64 }
struct H { o: Option[Wide] }
struct Outer { w: W, tag: i64 }
fn cls(r: Result[W, i64]) -> i64 {
    match r { Result.Ok(w) => match w.o { Option.Some(Option.Some(_)) => 1, _ => -1 }, Result.Err(e) => e }
}
fn cls_moved(r: Result[W, i64]) -> i64 {
    match r {
        Result.Ok(w) => {
            let inner: Option[Option[i64]] = w.o;
            match inner { Option.Some(Option.Some(_)) => 8, _ => -1 }
        },
        Result.Err(e) => e,
    }
}
fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        acc = acc + cls(Result.Ok(W { o: Option.Some(Option.Some(n + i)) }));
        let never_read: W = W { o: Option.Some(Option.Some(n + i)) };
        acc = acc + 2;
        let bare: W = W { o: Option.Some(Option.Some(n + i)) };
        let moved: Option[Option[i64]] = bare.o;
        acc = acc + match moved { Option.Some(Option.Some(_)) => 4, _ => -1 };
        acc = acc + cls_moved(Result.Ok(W { o: Option.Some(Option.Some(n + i)) }));
        let h: H = H { o: Option.Some(Wide { a: n + i, b: 2, c: 3, d: 4 }) };
        acc = acc + match h.o { Option.Some(_) => 16, Option.None => -1 };
        let s: W2 = W2 { o: Option.Some(Option.Some(f"p{n + i}")) };
        acc = acc + match s.o { Option.Some(Option.Some(_)) => 32, _ => -1 };
        let s2: W2 = W2 { o: Option.Some(Option.Some(f"q{n + i}")) };
        let smoved: Option[Option[String]] = s2.o;
        acc = acc + match smoved { Option.Some(Option.Some(t)) => if t.len() > 0 { 64 } else { 0 }, _ => -1 };
        let ou: Outer = Outer { w: W { o: Option.Some(Option.Some(n + i)) }, tag: 1 };
        acc = acc + match ou.w.o { Option.Some(Option.Some(_)) => 128, _ => -1 };
        let mut v: Vec[W] = Vec.new();
        v.push(W { o: Option.Some(Option.Some(n + i)) });
        acc = acc + match v[0].o { Option.Some(Option.Some(_)) => 256, _ => -1 };
        i = i + 1;
    }
    println(acc);
}
"#,
        &["20440"],
        "struct_field_boxed_heapless_option_envelope_owned",
        40,
    );
}

/// Expected value is COMPUTED: 1+2+4+8+16+16+32+64+128 = 271 per iteration,
/// x40 = 10840.
#[test]
fn asan_by_value_struct_option_field_owned_no_leak_no_double_free() {
    assert_clean_asan_run_min_allocs(
        r#"
struct P { s: String }
struct O { s: Option[String] }
struct R { s: Result[String, i64] }
struct Ov { s: Option[Vec[String]] }
struct B { o: Option[Option[String]] }
struct M { a: String, o: Option[Option[String]] }
struct T { a: String, s: Option[String] }
struct Sc { o: Option[Option[i64]] }
fn take_p(x: P) -> i64 { 1 }
fn take_o(x: O) -> i64 { 2 }
fn take_r(x: R) -> i64 { 4 }
fn take_ov(x: Ov) -> i64 { 8 }
fn take_b(x: B) -> i64 { 16 }
fn take_m(x: M) -> i64 { 32 }
fn take_t(x: T) -> i64 { 64 }
fn borrow_o(x: ref O) -> i64 { 128 }
fn take_sc(x: Sc) -> i64 { 256 }
fn mk_b(k: i64) -> B { B { o: Option.Some(Option.Some(f"boxed-call-{k}")) } }
fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        acc = acc + take_p(P { s: f"plain-{n + i}" });
        acc = acc + take_o(O { s: Option.Some(f"opt-{n + i}") });
        acc = acc + take_r(R { s: Result.Ok(f"res-{n + i}") });
        let mut v: Vec[String] = Vec.new();
        v.push(f"vec-{n + i}");
        acc = acc + take_ov(Ov { s: Option.Some(v) });
        let b: B = B { o: Option.Some(Option.Some(f"boxed-named-{n + i}")) };
        acc = acc + take_b(b);
        acc = acc + take_b(mk_b(n + i));
        acc = acc + take_m(M { a: f"sib-{n + i}", o: Option.Some(Option.Some(f"boxed-sib-{n + i}")) });
        acc = acc + take_t(T { a: f"a-{n + i}", s: Option.Some(f"t-{n + i}") });
        let bo: O = O { s: Option.Some(f"borrow-{n + i}") };
        acc = acc + borrow_o(bo);
        acc = acc + take_sc(Sc { o: Option.Some(Option.Some(n + i)) });
        let scn: Sc = Sc { o: Option.Some(Option.Some(n + i)) };
        acc = acc + take_sc(scn) - 256;
        i = i + 1;
    }
    println(acc);
}
"#,
        &["21080"],
        "by_value_struct_option_field_owned",
        100,
    );
}

/// B-2026-08-06-29 — a BOUND passthrough result, then CONSUMED by a callee.
///
/// `let x = idopt(d); peek(x);` aborted with `free(): double free detected
/// in tcache 2` on a DEFAULT -O2 build. It is the third distinct arm of one
/// shape, and the three are worth reading together because each needed its
/// own fix:
///   * bound then MATCHED — B-2026-08-06-27;
///   * DISCARDED — B-2026-08-06-28;
///   * bound then CONSUMED by a callee — this row.
///
/// The alias record was already correct. B-2026-08-06-27 deliberately does
/// NOT arm `x`: the passthrough result aliases `d`, so `x` records
/// `passthrough_owner_alias[x] = d` and `d` stays sole owner. What was
/// missing is that consuming the ALIAS must disarm the binding it aliases —
/// the moved-arg suppressors zeroed the named binding, found `x` unarmed,
/// and did nothing, so the callee freed the payload and `d`'s own cleanup
/// freed it again.
///
/// That makes this a NARROWING in the same sense the family's other rows
/// use: the suppression already existed for a directly-armed argument
/// (`peek(d)`), and it now lands on the binding that actually owns the
/// payload. It fires only where a consume is already happening — not a
/// blanket zeroing of a source, the move B-2026-08-06-21 showed leaks.
///
/// Both SIBLING PAYLOADS are carried because each has its own suppressor
/// and each was measured broken: inline `Result[String, i64]` and heap-
/// BOXED `Option[Wide]`.
///
/// THE PRIOR TWO ARMS ARE REGRESSION CONTROLS here — bound-then-matched
/// (-27) and discarded (-28) must both stay clean, since all three arms now
/// run through the same alias record. So are a directly-armed consumed
/// binding and a FRESH temp, which must still be freed: the fix is a
/// suppression, and over-suppressing shows up as a LEAK, which this file's
/// LSan harness reports on Linux.
///
/// Every string is built at runtime from `env.args().len()` — a literal
/// payload is rodata and cannot fail (B-2026-08-04-17).
#[test]
fn asan_bound_passthrough_result_consumed_by_callee_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct Wide { a: String, b: String, c: i64, d: i64 }
fn idopt(o: Option[String]) -> Option[String] { o }
fn idres(r: Result[String, i64]) -> Result[String, i64] { r }
fn idbox(o: Option[Wide]) -> Option[Wide] { o }
fn peek(o: Option[String]) -> i64 {
    match o { Option.Some(s) => s.len(), Option.None => 0 }
}
fn peekr(r: Result[String, i64]) -> i64 {
    match r { Result.Ok(s) => s.len(), Result.Err(e) => e }
}
fn peekb(o: Option[Wide]) -> i64 {
    match o { Option.Some(w) => w.a.len() + w.c, Option.None => 0 }
}
fn mk(n: i64) -> String {
    let mut s: String = String.new();
    s.push_str("payload-");
    s.push_str(n.to_string());
    s.push_str("-padding-to-force-a-real-heap-buffer");
    s
}
fn main() {
    let n = env.args().len() as i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40 {
        // (a) the reported shape — bound passthrough, then CONSUMED.
        let d: Option[String] = Option.Some(mk(n + i));
        let x = idopt(d);
        acc = acc + peek(x);
        // (b) the inline Result sibling.
        let r: Result[String, i64] = Result.Ok(mk(n + i));
        let rx = idres(r);
        acc = acc + peekr(rx);
        // (c) the heap-BOXED sibling.
        let bw: Option[Wide] = Option.Some(Wide { a: mk(n + i), b: mk(n + i), c: 1, d: 2 });
        let bx = idbox(bw);
        acc = acc + peekb(bx);
        // CONTROL (B-2026-08-06-27): bound, then MATCHED.
        let d2: Option[String] = Option.Some(mk(n + i));
        let y = idopt(d2);
        acc = acc + match y { Option.Some(s) => s.len(), Option.None => 0 };
        // CONTROL (B-2026-08-06-28): DISCARDED passthrough.
        let d3: Option[String] = Option.Some(mk(n + i));
        idopt(d3);
        // CONTROL: a directly-armed binding consumed — always worked.
        let d4: Option[String] = Option.Some(mk(n + i));
        acc = acc + peek(d4);
        // CONTROL: a FRESH temp has no named source and must still be freed.
        idopt(Option.Some(mk(n + i)));
        i = i + 1;
    }
    println(acc);
}
"#,
        // Oracle-checked, not pasted back from the compiler: `--interp`,
        // `KARAC_AUTO_PAR=0` AOT and the default auto-par build all agree,
        // and valgrind reports 0 errors / 0 bytes lost on the same program.
        &["9195"],
        "bound_passthrough_result_consumed_by_callee_frees_once",
        // 40 rounds x eight runtime-built Strings (Wide carries two).
        200,
    );
}

/// B-2026-08-06-28 — a DISCARDED passthrough call result must not free its
/// argument's payload.
///
/// `let d: Option[String] = Some(mk()); idopt(d);` aborted with
/// `free(): double free detected in tcache 2` on a DEFAULT -O2 build (and
/// identically at -O0, so not an -O0 curiosity). The returned Option is an
/// ALIAS of `d`'s payload, but the discarded temp was materialized into a
/// slot with a free of its own while `d` stayed armed — two owners.
///
/// Calling a function for its effect and ignoring a passthrough return is
/// ordinary code, which is why this was high severity despite looking
/// contrived.
///
/// THREE PAYLOAD SHAPES, each measured broken before the fix rather than
/// assumed by symmetry: inline `Option[String]`, inline
/// `Result[String, i64]`, and heap-BOXED `Option[Wide]`. An
/// `Option[shared T]` payload was ALREADY clean — rc handles are not
/// buffer-owned — so it is carried as a control rather than a fourth fix.
///
/// THE TWO NEGATIVE CONTROLS ARE THE LOAD-BEARING ONES, because the fix is
/// a SUPPRESSION and over-suppressing leaks:
///   * a FRESH temp (`idopt(Some(mk(n)))`) has no named source, so it must
///     still be registered and still freed;
///   * a non-passthrough consuming callee must keep freeing normally.
///
/// Under LSan on Linux this file's harness reports either direction, so a
/// suppression that went too far fails here rather than silently leaking.
///
/// The payload must be genuinely heap — every string is built at runtime
/// from `env.args().len()`. A string LITERAL payload is rodata and cannot
/// fail; that flawed control has now tripped this family three times
/// (B-2026-08-06-21, -27, and this row's own note).
#[test]
fn asan_discarded_passthrough_result_does_not_free_source_payload() {
    assert_clean_asan_run_min_allocs(
        r#"struct Wide { a: String, b: String, c: i64, d: i64 }
shared struct Node { s: String }
fn idopt(o: Option[String]) -> Option[String] { o }
fn idres(r: Result[String, i64]) -> Result[String, i64] { r }
fn idbox(o: Option[Wide]) -> Option[Wide] { o }
fn idsh(o: Option[Node]) -> Option[Node] { o }
fn peek(o: Option[String]) -> i64 {
    match o { Option.Some(s) => s.len(), Option.None => 0 }
}
fn mk(n: i64) -> String {
    let mut s: String = String.new();
    s.push_str("payload-");
    s.push_str(n.to_string());
    s.push_str("-padding-to-force-a-real-heap-buffer");
    s
}
fn main() {
    let n = env.args().len() as i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40 {
        // (a) the reported shape — inline Option, discarded passthrough.
        let d: Option[String] = Option.Some(mk(n + i));
        idopt(d);
        // (b) the inline Result sibling.
        let r: Result[String, i64] = Result.Ok(mk(n + i));
        idres(r);
        // (c) the heap-BOXED payload sibling.
        let bw: Option[Wide] = Option.Some(Wide { a: mk(n + i), b: mk(n + i), c: 1, d: 2 });
        idbox(bw);
        // (d) shared payload — already clean, carried as a control.
        let sh: Option[Node] = Option.Some(Node { s: mk(n + i) });
        idsh(sh);
        // CONTROL: a FRESH temp has no named source and must still be freed.
        idopt(Option.Some(mk(n + i)));
        // CONTROL: a non-passthrough consuming callee still frees normally.
        let d3: Option[String] = Option.Some(mk(n + i));
        acc = acc + peek(d3);
        i = i + 1;
    }
    println(acc);
}
"#,
        // Oracle-checked, not pasted back from the compiler: `--interp`,
        // `KARAC_AUTO_PAR=0` AOT and the default auto-par build all print
        // 1831, and valgrind reports 0 errors / 0 bytes lost on the same
        // program. The interpreter shares none of codegen's ownership
        // machinery, so its agreement is what makes this an expected value.
        &["1831"],
        "discarded_passthrough_result_does_not_free_source_payload",
        // 40 rounds x seven runtime-built Strings (Wide carries two), all
        // heap. A real run measures far above this floor; a folded-away
        // version could not reach it.
        200,
    );
}

/// B-2026-08-05-9: `Result[T, E].unwrap_or(d)` DISCARDS the `Err` payload on
/// the absent path, and codegen emitted no free for it — the present path
/// freed its discarded value (the eagerly-evaluated default, B-2026-07-16-22)
/// but the absent path had no counterpart. A heap `E` therefore leaked once
/// per Err-tagged call, unbounded in a loop.
///
/// Why the sibling fixture above missed it for so long, which is the part
/// worth keeping: its `unwrap_or` defaults are literal `"…".to_string()`,
/// which LLVM constant-folds and deletes outright — with no default
/// allocation surviving, the shape never exercised the branch. Swapping just
/// those two literals for f-strings made it leak 133 allocations at the
/// DEFAULT -O2. So this fixture keeps BOTH payloads runtime-derived: the
/// `Err` value that leaks and the default that reaches the absent path. A
/// literal anywhere in that chain re-hides the bug.
///
/// Directions pinned: a leak if the absent-path free goes missing again, and
/// a double-free (SIGABRT) if it is ever emitted twice — once here and once
/// via a receiver's scope-exit payload cleanup.
#[test]
fn asan_unwrap_or_result_discarded_err_payload_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
fn res(i: i64) -> Result[String, String] {
    if i % 3 == 0 { Ok("ok-value".to_string()) } else { Err(f"err-{i}") }
}
fn main() {
    let base: i64 = env.args().len();
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < base + 199 {
        let b = res(i).unwrap_or(f"res-default-{i}");
        total = total + (b.len() as i64);
        i = i + 1;
    }
    println(total);
}
"#,
        // base=1 -> 200 iterations: 67 Ok ("ok-value", 8) = 536, plus 133
        // absent iterations taking f"res-default-{i}" = 1923. Total 2459.
        &["2459"],
        "unwrap_or_result_discarded_err_payload_no_leak",
        // ~400 real allocations (a receiver payload and a default per
        // iteration). The floor sits far above the 3 an allocation-free run
        // reports, so a future fold-away regression fails loudly instead of
        // passing vacuously.
        200,
    );
}

#[test]
fn asan_optional_chain_heap_payload_clean() {
    // B-2026-08-17-28 leg (3) — `?.` is lowered to the `match` it stands
    // for, so it now emits that path's payload move-out and arm-body
    // drops. This is the memory dimension of that lowering: a chain over
    // a payload carrying a `String`, walked 200 times with the level
    // alternating between present and absent, so a double free aborts
    // immediately and any per-iteration leak accumulates for LSan.
    //
    // The alternation matters: the absent arm short-circuits without
    // touching the payload, and the present arm projects a field out of
    // it — the two arms have different ownership obligations and only
    // running both exercises the pair.
    //
    // BOUND TO A `let` RATHER THAN MATCHED DIRECTLY, and the difference
    // was not this row's: a match whose SCRUTINEE is itself a match
    // producing a boxed `Option` payload leaked that box, reproducing with
    // no `?.` anywhere. Filed and fixed as B-2026-08-18-8, whose two
    // fixtures below cover the scrutinee position in both spellings. This
    // one keeps the `let` form deliberately, so the pair reads as the
    // contrast that isolated the defect: `?.`'s own lowering was always
    // clean, which is what this pins.
    assert_clean_asan_run(
        r#"
struct City { name: String, zip: i64 }
struct Address { city: Option[City], tag: String }
struct User { address: Option[Address] }

fn mk(i: i64) -> User {
    if i % 2 == 0 { return User { address: Some(Address { city: Some(City { name: "Paris", zip: 7 }), tag: "t" }) }; }
    return User { address: None };
}

fn main() {
    let mut total = 0;
    let mut i = 0;
    while i < 200 {
        let u = mk(i);
        let c = u.address?.city;
        match c { Some(v) => { total = total + (v.name.len() as i64); } None => { total = total + 1; } }
        i = i + 1;
    }
    println(total);
}
"#,
        &["600"],
        "optional_chain_heap_payload_clean",
    );
}

#[test]
fn asan_published_option_slot_payload_never_consumed_no_leak() {
    // B-2026-07-16-19 (parent-ownership leg): published Option[String]
    // slots whose payloads are never consumed after the join (`is_some()`
    // reads only). The parent's re-registered `FreeInlineOptionPayload`
    // is the ONLY thing standing between this shape and a per-slot
    // payload leak — LSan on the Linux CI leg is the authoritative gate.
    assert_clean_asan_run(
        r#"
fn first_word(s: String) -> Option[String] {
    let words = s.split(" ");
    if words.len() > 0 { Some(words[0]) } else { None }
}
fn main() {
    let a = first_word("aaaaaaaaaaaaaaaaaaaaaaaaaaaaa bb");
    let b = first_word("ccccccccccccccccccccccccccccc dd");
    println(a.is_some());
    println(b.is_some());
}
"#,
        &["true", "true"],
        "published_option_slot_payload_never_consumed_no_leak",
    );
}

#[test]
fn asan_sequential_unwrap_or_on_named_option_binding_no_double_free() {
    // B-2026-07-17-4 (sequential, surfaced while fixing B-2026-07-16-19):
    // `let a = r.unwrap_or("x").len();` on a let-bound Option[String].
    // `unwrap_or`'s present branch reconstitutes the payload as a SHALLOW
    // alias that the chained `.len()` consumes and frees as an owned temp;
    // pre-fix the receiver binding's scope-exit `FreeInlineOptionPayload`
    // freed the same buffer again (`free(): double free` with NO auto-par
    // involved — main serializes here). `unwrap`/`expect` already
    // suppressed the source (B-2026-07-10-2); `unwrap_or` now does too.
    assert_clean_asan_run(
        r#"
fn first_word(s: String) -> Option[String] {
    let words = s.split(" ");
    if words.len() > 0 { Some(words[0]) } else { None }
}
fn main() {
    let r = first_word("hello world");
    let a = r.unwrap_or("x").len();
    println(a);
}
"#,
        &["5"],
        "sequential_unwrap_or_on_named_option_binding_no_double_free",
    );
}

// B-2026-07-30-11 (Option/Result leg) — payload bodies for let-bound
// `Option`/`Result` bindings, plus the match / consuming-combinator /
// ctor-arg disarms.
//
// Same unsafe-direction guard as the enum leg: the bodies walk frees
// nothing (payload memory stays with `FreeInline*` / `BoxedEnumDrop`), so
// LSan cannot witness the fix landing — what this catches is an
// OVER-fire (the body reading a moved-out payload's wiped words, or the
// boxed case reading through a freed box) and any DOUBLE-free the walk
// could introduce beside the existing frees. The BOXED shape is the
// load-bearing one here: `Full { name, buf }` exceeds the inline word
// budget, so the walk goes through the box pointer, null-guarded — a
// stale box would be a use-after-free ASAN aborts on. The `self.buf[0]`
// read keeps the element allocations live (the Vec-leg vacuity lesson).
#[test]
fn asan_optres_payload_user_drop_bodies_fire_once() {
    assert_clean_asan_run(
        r#"
struct Full { name: String, buf: Vec[i64] }
impl Drop for Full {
    fn drop(mut ref self) {
        if let Some(v) = self.buf.first() { if v < 0i64 { println(v); } }
        self.buf.clear();
    }
}

fn mk(i: i64) -> Full {
    let mut b: Vec[i64] = Vec.new();
    b.push(i);
    return Full { name: "payload-string-data", buf: b };
}

fn opt(i: i64) -> Option[Full] {
    return Option.Some(mk(i));
}

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        // Boxed payload held to the binding's end — the body fires through
        // the box at the NLL point; the box + payload heap free exactly once.
        let a = opt(i);
        n = n + 1i64;

        // Result Ok side, annotated ctor let.
        let b: Result[Full, i64] = Ok(mk(i));
        n = n + 1i64;

        // Payload moved out by a match arm — the source walk must not read
        // the moved-from slot.
        let c = opt(i);
        match c {
            Some(r) => { n = n + 1i64; }
            None => { n = n + 100i64; }
        }

        // Consuming combinator — the result binding owns the payload; the
        // source walk must not fire over the moved-from slot.
        let d = opt(i);
        let r2 = d.unwrap();
        n = n + 1i64;
        i = i + 1;
    }
    println(n);
}
"#,
        // 4 per iteration x 200 = 800.
        &["800"],
        "optres_payload_user_drop_bodies_fire_once",
    );
}

/// B-2026-08-04-9 — `?` on a heap-BOXED payload owns the box it deboxes.
///
/// `?` CONSUMES its operand, so once the box is read the unwrapped value
/// owns the interior outright and the box allocation is dead — freeing it
/// at the debox is what makes the ownership transfer complete. Before the
/// fix nothing freed either: both the box and its interior leaked, and the
/// value was garbage besides.
///
/// `Mid` is here through `Option` only, where its 4 words exceed the
/// 3-word area and it boxes; through `Result` it is inline and never
/// reaches this path (the codegen twin covers that half for correctness).
///
/// Both spellings are here — through an intermediate binding, and
/// destructuring DIRECTLY on the `?`. They took different paths and broke
/// separately: the direct destructure additionally leaked both heap fields
/// because `expr_yields_fresh_owned_temp` matches only `Call`/`MethodCall`,
/// so a `?` RHS looked like a place-source and the leaf bindings never
/// registered their cleanup (B-2026-08-04-10). `.unwrap()` and a direct
/// call were always fine, which is what localized it to that predicate.
#[test]
fn asan_question_deboxed_ok_payload_frees_the_box() {
    assert_clean_asan_run(
        r#"
struct Full { name: String, buf: Vec[i64] }
struct Mid { name: String, pad: i64 }
fn mkf(i: i64) -> Full {
    let mut b: Vec[i64] = Vec.new();
    b.push(i);
    let mut s: String = String.new();
    s.push_str("payload-string-data");
    return Full { name: s, buf: b };
}
fn mkm(i: i64) -> Mid {
    let mut s: String = String.new();
    s.push_str("payload-string-data");
    return Mid { name: s, pad: i };
}
fn resf(i: i64) -> Result[Full, String] { return Result.Ok(mkf(i)); }
fn optf(i: i64) -> Option[Full] { return Option.Some(mkf(i)); }
fn optm(i: i64) -> Option[Mid] { return Option.Some(mkm(i)); }

fn step_res(i: i64) -> Result[i64, String] {
    // 6 words — boxed at Result's 5-word area.
    let a = resf(i)?;
    return Result.Ok(a.name.len() + a.buf.len());
}

fn step_opt(i: i64) -> Option[i64] {
    // 6 words — boxed at Option's 3-word area.
    let b = optf(i)?;
    // 4 words — fits Result inline, but BOXES here.
    let c = optm(i)?;
    return Option.Some(b.name.len() + b.buf.len() + c.name.len() + c.pad);
}

fn step_destructure(i: i64) -> Result[i64, String] {
    // Destructured DIRECTLY on the `?`, with no binding in between.
    let Full { name, buf } = resf(i)?;
    return Result.Ok(name.len() + buf.len());
}

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        match step_res(i) {
            Result.Ok(v) => { n = n + v; }
            Result.Err(_) => { n = n + 1000i64; }
        }
        match step_opt(i) {
            Option.Some(v) => { n = n + v; }
            Option.None => { n = n + 1000i64; }
        }
        match step_destructure(i) {
            Result.Ok(v) => { n = n + v; }
            Result.Err(_) => { n = n + 1000i64; }
        }
        i = i + 1;
    }
    println(n);
}
"#,
        // step_res 19+1 = 20, step_opt 19+1 + 19+i = 39+i, step_destructure
        // 19+1 = 20. Per iteration 79+i; over i in 0..200 that is
        // 200*79 + 19900 = 35700.
        &["35700"],
        "question_deboxed_ok_payload_frees_the_box",
    );
}

/// B-2026-08-05-3 — an `Option`/`Result` TUPLE payload owned exactly once,
/// on both carriers. Three gaps: the `Result` consuming-arm suppression was
/// ungated; a GUARDED arm leaked because `x.1 == 5i64` lowers to
/// `Call { Path(["i64","eq"]) }`, which the consumption classifier read as
/// a construction; and the `Option` box drop was box-ONLY, because
/// `inner_drop_fn` derives from a struct NAME and a tuple has none.
///
/// Arms O9 (element moved out of the match) and O10 (tuple PATTERN
/// destructure) are pinned because they DOUBLE-FREE if the box's inner drop
/// is not retracted for a consuming arm — they are what defeated the first
/// attempt at the Option leg, alongside drop_fuzz seed 4272.
///
/// Seeded from `env.args().len()` with element and byte reads throughout so
/// nothing folds or dead-strips (B-2026-08-04-17); ~3,800 allocations,
/// floored well below that.
/// B-2026-08-05-3, RESULT leg — a `Result` tuple payload owned exactly once.
/// Two gaps: the consuming-arm suppression was ungated (disarming the source
/// for a binding that owns nothing), and a GUARDED arm leaked because
/// `x.1 == 5i64` lowers to `Call { Path(["i64","eq"]) }`, which the
/// consumption classifier read as a construction.
///
/// The OPTION carrier's leak is deliberately NOT covered — the row stays
/// open. The first attempt at it added a second let-site drop, which passed
/// every hand probe and double-freed in the wild (drop_fuzz seed 4272): a
/// droppable tuple payload is always >= 4 words and so is already boxed with
/// a `BoxedEnumDrop` owning it.
///
/// Seeded from `env.args().len()` with element and byte reads throughout so
/// nothing folds or dead-strips (B-2026-08-04-17); ~2,000 allocations,
/// floored well below that.
/// B-2026-08-05-3 — an `Option`/`Result` payload that is a TUPLE with a
/// heap element must be owned exactly once. Three gaps, one fixture: the
/// let-site drop never registered for a tuple payload at all; the `Result`
/// carrier's consuming-arm suppression was ungated, disarming the source
/// for a binding that owns nothing; and a GUARDED arm leaked on both
/// carriers because `x.1 == 5i64` lowers to `Call { Path(["i64","eq"]) }`,
/// which the consumption classifier read as a construction.
///
/// The controls are load-bearing — each caught a real regression while the
/// fix was being developed. `Some((v, k))` binds elements that DO own
/// themselves, and the struct payloads are already registered on another
/// channel; adding a second owner to either turned the leak into a double
/// free (`Option[H]`: 8 allocations / 8 frees became 11 / 12 and aborted).
///
/// Seeded from `env.args().len()` with element and byte reads throughout so
/// nothing folds or dead-strips (B-2026-08-04-17); ~1,800 allocations,
/// floored well below that. Pre-fix: 1,807 allocations, 1,007 frees.
#[test]
fn asan_optres_tuple_payload_is_owned_exactly_once() {
    assert_clean_asan_run_min_allocs(
                "struct H { a: Vec[i64], b: i64 }\n\
                 fn mkv(k: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(k); v.push(k + 1i64); return v; }\n\
                 fn mks(k: i64) -> String { let mut s: String = String.new(); s.push_str(f\"pay-{k}\"); return s; }\n\
                 fn dig(i: i64) -> String { let mut d: String = String.new(); d.push_str(f\"{i}\"); return d; }\n\
                 fn sinkt(t: (Vec[i64], i64)) -> i64 { return t.0[0i64]; }\n\
                 fn main() {\n\
                 \x20   let base: i64 = env.args().len();\n\
                 \x20   let mut acc = 0i64;\n\
                 \x20   let mut i = base;\n\
                 \x20   while i < base + 100i64 {\n\
                 \x20       // 1. Result[tuple] bound and READ, never moved — the leg the ungated\n\
                 \x20       //    arm suppression broke.\n\
                 \x20       let r1: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
                 \x20       match r1 {\n\
                 \x20           Result.Ok(x) => { acc = acc + x.0[0i64] + x.1; }\n\
                 \x20           Result.Err(e) => { acc = acc + e; }\n\
                 \x20       }\n\
                 \x20       // 2. Result[tuple] never matched at all — the let-site walk alone.\n\
                 \x20       let r2: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
                 \x20       acc = acc + 1i64;\n\
                 \x20       // 3. GUARDED arm — the `x.1 == 5` operator desugar.\n\
                 \x20       let r3: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
                 \x20       match r3 {\n\
                 \x20           Result.Ok(x) if x.1 == 5i64 => { acc = acc + x.0[0i64]; }\n\
                 \x20           _ => { acc = acc - 1i64; }\n\
                 \x20       }\n\
                 \x20       // 4. String element, read through its BYTES.\n\
                 \x20       let r4: Result[(String, i64), i64] = Result.Ok((mks(i), 5i64));\n\
                 \x20       match r4 {\n\
                 \x20           Result.Ok(x) => { if x.0.contains(dig(i)) { acc = acc + x.0.len(); } }\n\
                 \x20           Result.Err(e) => { acc = acc + e; }\n\
                 \x20       }\n\
                 \x20       // 5. Vec[String] element.\n\
                 \x20       let mut vs: Vec[String] = Vec.new();\n\
                 \x20       vs.push(mks(i));\n\
                 \x20       vs.push(mks(i + 1i64));\n\
                 \x20       let r5: Result[(Vec[String], i64), i64] = Result.Ok((vs, 5i64));\n\
                 \x20       match r5 {\n\
                 \x20           Result.Ok(x) => { acc = acc + x.0.len() + x.0[0i64].len(); }\n\
                 \x20           Result.Err(e) => { acc = acc + e; }\n\
                 \x20       }\n\
                 \x20       // 6. Heap on the Err half.\n\
                 \x20       let r6: Result[i64, (Vec[i64], i64)] = Result.Err((mkv(i), 5i64));\n\
                 \x20       match r6 {\n\
                 \x20           Result.Ok(v) => { acc = acc + v; }\n\
                 \x20           Result.Err(e) => { acc = acc + e.0[0i64]; }\n\
                 \x20       }\n\
                 \x20       // 7. if-let, borrow-only and moving.\n\
                 \x20       let r7: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
                 \x20       if let Result.Ok(x) = r7 { acc = acc + x.0[0i64]; } else { acc = acc - 1i64; }\n\
                 \x20       let r8: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
                 \x20       let mut g: Vec[i64] = Vec.new();\n\
                 \x20       if let Result.Ok(x) = r8 { g = x.0; } else { acc = acc - 1i64; }\n\
                 \x20       acc = acc + g[0i64] + g.len();\n\
                 \x20       // 8. Moved out of the match into an owned-param callee.\n\
                 \x20       let r9: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
                 \x20       match r9 {\n\
                 \x20           Result.Ok(x) => { acc = acc + sinkt(x); }\n\
                 \x20           Result.Err(e) => { acc = acc + e; }\n\
                 \x20       }\n\
                 \x20       // 9. Element MOVED out through the match value.\n\
                 \x20       let r10: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
                 \x20       let g2: Vec[i64] = match r10 { Result.Ok(x) => x.0, Result.Err(e) => Vec.new() };\n\
                 \x20       acc = acc + g2[1i64];\n\
                 \x20       // --- CONTROLS: shapes that must NOT gain a second owner ---\n\
                 \x20       // C1. Tuple PATTERN destructure — the elements own themselves.\n\
                 \x20       let r11: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
                 \x20       match r11 {\n\
                 \x20           Result.Ok((v, k)) => { acc = acc + v[0i64] + k; }\n\
                 \x20           Result.Err(e) => { acc = acc + e; }\n\
                 \x20       }\n\
                 \x20       // C2. STRUCT payload — must keep its unconditional arm suppression.\n\
                 \x20       let r12: Result[H, i64] = Result.Ok(H { a: mkv(i), b: 5i64 });\n\
                 \x20       match r12 {\n\
                 \x20           Result.Ok(x) => { acc = acc + x.a[0i64] + x.b; }\n\
                 \x20           Result.Err(e) => { acc = acc + e; }\n\
                 \x20       }\n\
                 \x20       let r13: Result[H, i64] = Result.Ok(H { a: mkv(i), b: 5i64 });\n\
                 \x20       match r13 {\n\
                 \x20           Result.Ok(x) if x.b == 5i64 => { acc = acc + x.a[0i64]; }\n\
                 \x20           _ => { acc = acc - 1i64; }\n\
                 \x20       }\n\
                 \x20       // C4. All-scalar tuple payload — must get no drop at all.\n\
                 \x20       let r15: Result[(i64, i64), i64] = Result.Ok((i, 5i64));\n\
                 \x20       match r15 {\n\
                 \x20           Result.Ok(x) => { acc = acc + x.0 + x.1; }\n\
                 \x20           Result.Err(e) => { acc = acc + e; }\n\
                 \x20       }\n\
                 \x20       // --- OPTION carrier (B-2026-08-05-3's own shape; second attempt) ---\n\
                 \x20       // O1. Bound and READ, never moved.\n\
                 \x20       let o1: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
                 \x20       match o1 {\n\
                 \x20           Option.Some(x) => { acc = acc + x.0[0i64] + x.1; }\n\
                 \x20           Option.None => { acc = acc - 1i64; }\n\
                 \x20       }\n\
                 \x20       // O2. NEVER matched at all — the let-site walk alone. This is what\n\
                 \x20       //     shows the bug is scope-exit, not arm binding.\n\
                 \x20       let o2: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
                 \x20       acc = acc + 1i64;\n\
                 \x20       // O3. GUARDED arm.\n\
                 \x20       let o3: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
                 \x20       match o3 {\n\
                 \x20           Option.Some(x) if x.1 == 5i64 => { acc = acc + x.0[0i64]; }\n\
                 \x20           _ => { acc = acc - 1i64; }\n\
                 \x20       }\n\
                 \x20       // O4. String element, read through its BYTES.\n\
                 \x20       let o4: Option[(String, i64)] = Option.Some((mks(i), 5i64));\n\
                 \x20       match o4 {\n\
                 \x20           Option.Some(x) => { if x.0.contains(dig(i)) { acc = acc + x.0.len(); } }\n\
                 \x20           Option.None => { acc = acc - 1i64; }\n\
                 \x20       }\n\
                 \x20       // O5. Vec[String] element.\n\
                 \x20       let mut ovs: Vec[String] = Vec.new();\n\
                 \x20       ovs.push(mks(i));\n\
                 \x20       ovs.push(mks(i + 1i64));\n\
                 \x20       let o5: Option[(Vec[String], i64)] = Option.Some((ovs, 5i64));\n\
                 \x20       match o5 {\n\
                 \x20           Option.Some(x) => { acc = acc + x.0.len() + x.0[0i64].len(); }\n\
                 \x20           Option.None => { acc = acc - 1i64; }\n\
                 \x20       }\n\
                 \x20       // O6. if-let, borrow-only and moving.\n\
                 \x20       let o6: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
                 \x20       if let Option.Some(x) = o6 { acc = acc + x.0[0i64]; } else { acc = acc - 1i64; }\n\
                 \x20       let o7: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
                 \x20       let mut og: Vec[i64] = Vec.new();\n\
                 \x20       if let Option.Some(x) = o7 { og = x.0; } else { acc = acc - 1i64; }\n\
                 \x20       acc = acc + og[0i64] + og.len();\n\
                 \x20       // O8. Moved into an owned-param callee.\n\
                 \x20       let o8: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
                 \x20       match o8 {\n\
                 \x20           Option.Some(x) => { acc = acc + sinkt(x); }\n\
                 \x20           Option.None => { acc = acc - 1i64; }\n\
                 \x20       }\n\
                 \x20       // --- The two shapes that DEFEATED the first attempt at this leg. ---\n\
                 \x20       // O9. Element MOVED out through the match value. The arm takes the\n\
                 \x20       //     box's interior, so the box drop must retract to box-only.\n\
                 \x20       let o9: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
                 \x20       let og2: Vec[i64] = match o9 { Option.Some(x) => x.0, Option.None => Vec.new() };\n\
                 \x20       acc = acc + og2[1i64];\n\
                 \x20       // O10. Tuple PATTERN destructure — the leaf bindings own their\n\
                 \x20       //      elements, so the box drop must retract here too.\n\
                 \x20       let o10: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
                 \x20       match o10 {\n\
                 \x20           Option.Some((v, k)) => { acc = acc + v[0i64] + k; }\n\
                 \x20           Option.None => { acc = acc - 1i64; }\n\
                 \x20       }\n\
                 \x20       // O11. STRUCT payload control — a different channel owns it.\n\
                 \x20       let o11: Option[H] = Option.Some(H { a: mkv(i), b: 5i64 });\n\
                 \x20       match o11 {\n\
                 \x20           Option.Some(x) => { acc = acc + x.a[0i64] + x.b; }\n\
                 \x20           Option.None => { acc = acc - 1i64; }\n\
                 \x20       }\n\
                 \x20       // O12. All-scalar tuple payload — must get no drop at all.\n\
                 \x20       let o12: Option[(i64, i64)] = Option.Some((i, 5i64));\n\
                 \x20       match o12 {\n\
                 \x20           Option.Some(x) => { acc = acc + x.0 + x.1; }\n\
                 \x20           Option.None => { acc = acc - 1i64; }\n\
                 \x20       }\n\
                 \x20       i = i + 1i64;\n\
                 \x20   }\n\
                 \x20   println(f\"acc={acc}\");\n\
                 }\n",
            &["acc=108568"],
            "optres_tuple_payload_is_owned_exactly_once",
            3000,
        );
}

/// B-2026-08-04-2 — a boxed payload bound whole and then MOVED must leave
/// exactly one owner of the box's interior.
///
/// Four destinations, all of which aborted under glibc before the fix
/// because the destination's drop and the box's inner walk freed the same
/// buffers: a struct literal, the match's own tail value, a plain `let`
/// rebind, and a container push. Plus the two controls in the other
/// direction, which is where this pin earns its keep — the neutralizer must
/// NOT fire for a by-value fn arg (an entry copy, not a move: firing there
/// leaked the box's copy) and must not fire when nothing moved at all.
///
/// Reading `.name` in every case is load-bearing: with the buffer dead the
/// allocation is elided and all six cases pass vacuously, which is exactly
/// why the class was first mis-read as depending on `impl Drop`.
#[test]
#[ignore = "B-2026-08-05-35 sweep: does not compile — chained field receivers (`a.b.c…`) are deferred to v1.x. Silently SKIPPED until the harness learned to fail on a codegen error."]
fn asan_boxed_optres_payload_view_move_has_one_owner() {
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
struct W { r: Res }
fn mko(i: i64) -> Option[Res] { return Option.Some(Res { id: i, name: "payload-string-data" }); }
fn eat(r: Res) -> i64 { return r.name.len(); }

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        // moved into a struct literal
        match mko(i) {
            Option.Some(r) => { let w = W { r: r }; n = n + w.r.name.len(); }
            Option.None => { n = n + 100i64; }
        }
        // escapes as the match's tail value
        let o2: Option[Res] = Option.Some(Res { id: i, name: "payload-string-data" });
        let r2 = match o2 {
            Option.Some(r) => r,
            Option.None => Res { id: 0, name: "z" },
        };
        n = n + r2.name.len();
        // rebound by a plain let
        let o3: Option[Res] = Option.Some(Res { id: i, name: "payload-string-data" });
        match o3 {
            Option.Some(r) => { let x = r; n = n + x.name.len(); }
            Option.None => { n = n + 100i64; }
        }
        // pushed into a container
        let o4: Option[Res] = Option.Some(Res { id: i, name: "payload-string-data" });
        let mut v: Vec[Res] = Vec.new();
        match o4 {
            Option.Some(r) => { v.push(r); }
            Option.None => { n = n + 100i64; }
        }
        n = n + v[0].name.len();
        // CONTROL: by-value fn arg — an entry copy, so the box keeps the
        // interior. Neutralizing here leaked it.
        let o5: Option[Res] = Option.Some(Res { id: i, name: "payload-string-data" });
        match o5 {
            Option.Some(r) => { n = n + eat(r); }
            Option.None => { n = n + 100i64; }
        }
        // CONTROL: not moved.
        let o6: Option[Res] = Option.Some(Res { id: i, name: "payload-string-data" });
        match o6 {
            Option.Some(r) => { n = n + r.name.len(); }
            Option.None => { n = n + 100i64; }
        }
        i = i + 1;
    }
    println(n);
}
"#,
        // 6 reads of a 19-char name per iteration x 200 = 22800.
        &["22800"],
        "boxed_optres_payload_view_move_has_one_owner",
    );
}

// B-2026-07-31-7 — an UNANNOTATED `let` of an `Option`/`Result` whose
// payload is heap-BOXED registered no cleanup at all and leaked the whole
// box, payload heap included.
//
// The box-drop registration resolved the binding's type from the
// annotation, else from `fn_return_type_exprs` keyed on a bare-`Identifier`
// callee. `Option.Some(mkres())`'s callee is the PATH `Option.Some`, so
// neither source fired. The annotated sibling (`a`) always worked, which is
// why this hid: the natural way to write the test is with the annotation.
//
// Unlike the bodies-only tests above, LSan CAN witness this one directly —
// it is a genuine leak of a malloc'd box plus its String and Vec buffers.
// Both bindings are kept so a fix that swings too far and double-frees the
// annotated case shows up here as an ASAN error rather than silently.
//
// B-2026-08-08-7 — `d` used to be `let d = Result.Ok(mkres(i))`, which does
// not typecheck and never did: an unannotated `Result.Ok(x)` leaves `E`
// with no source, and "cannot infer type parameter 'E'" is the CORRECT
// answer, not a gap. (The harness only asserted ownership, so the whole
// fixture ran on a program `karac build` rejects.) The unannotated-`let`
// shape under test is preserved by binding a call whose declared return
// type supplies `E` — still no annotation on the binding, still a boxed
// `Result` payload reaching the drop path, and now a legal program.
#[test]
fn asan_unannotated_boxed_optres_let_frees_its_box() {
    assert_clean_asan_run(
        r#"
struct Res { name: String, buf: Vec[i64] }

fn mkres(tag: i64) -> Res {
    let mut b: Vec[i64] = Vec.new();
    b.push(tag);
    return Res { name: "boxed-payload-wide-enough-to-spill", buf: b };
}

fn mkok(tag: i64) -> Result[Res, i64] { return Result.Ok(mkres(tag)); }

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        let a: Option[Res] = Option.Some(mkres(i));   // annotated — always worked
        let b = Option.Some(mkres(i));                // unannotated — leaked
        let c: Result[Res, i64] = Result.Ok(mkres(i));
        let d = mkok(i);                             // unannotated — leaked
        n = n + 4i64;
        i = i + 1;
    }
    println(n);
}
"#,
        &["800"],
        "unannotated_boxed_optres_let_frees_its_box",
    );
}

#[test]
fn asan_struct_field_tuple_optres_payload_freed() {
    // B-2026-08-03-7 (memory half) — a struct field holding a tuple with an
    // Option payload registered NO memory drop: the `NestedTuple` field
    // classifier shares `type_expr_has_drop_heap`'s deliberate Option/Result
    // blind spot, so the payload's buffer was orphaned. Latent until the
    // bodies fix here made the Drop body read `self.name` — the same
    // elided-allocation discriminator B-2026-08-03-3 documents.
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct W { p: (Option[Res], i64) }
fn main() {
    { let w = W { p: (Option.Some(Res { id: 1, name: f"a{1}" }), 10) }; println(w.p.1); }
    {
        let mut m: Map[i64, (Option[Res], i64)] = Map.new();
        m.insert(5, (Option.Some(Res { id: 2, name: f"bb{2}" }), 20));
        println(m.len());
    }
    println("end");
}
"#,
        &["10", "drop 1 a1", "1", "drop 2 bb2", "end"],
        "struct_field_tuple_optres_payload_freed",
    );
}

#[test]
fn asan_result_struct_payload_field_freed() {
    // B-2026-08-03-3 leg B — the LEAK oracle for the `Result[<Drop struct>,
    // E]` struct field, whose payload buffer was orphaned in every position.
    // LSan (the Linux CI leg — a macOS `-fsanitize=address` run misses leaks
    // entirely) is the only one of the three oracles that sees it: parity
    // agreed with the interpreter for three of these four shapes, and the
    // Drop body fired the right number of times, so nothing else was wrong
    // except that the `name` buffer was never freed.
    //
    // ASAN is also the gate the first attempt failed: arming the free
    // without a threshold-correct entry copy turned the by-value-param shape
    // into a double free (the `Result` payload area is 5 words, the Option
    // twin's is 3, so a 4-word payload is INLINE here and BOXED there — the
    // copy read the struct's first field as a box pointer).
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct H { r: Result[Res, i64], t: i64 }
fn take(h: H) -> i64 { h.t }
fn mk() -> H { H { r: Result.Ok(Res { id: 2, name: f"bb{2}" }), t: 20 } }
fn consume(h: H) -> i64 { match h.r { Result.Ok(x) => x.id, Result.Err(e) => e } }
fn main() {
    { let h = H { r: Result.Ok(Res { id: 1, name: f"a{1}" }), t: 10 }; println(h.t); }
    { let h = mk(); println(h.t); }
    { let h = H { r: Result.Ok(Res { id: 3, name: f"ccc{3}" }), t: 30 }; println(take(h)); }
    { let h = H { r: Result.Ok(Res { id: 4, name: f"dddd{4}" }), t: 40 }; let x = h.r; println(h.t); }
    { let h = H { r: Result.Ok(Res { id: 5, name: f"eeeee{5}" }), t: 50 }; println(consume(h)); }
    println("end");
}
"#,
        &[
            "10",
            "drop 1 a1",
            "20",
            "drop 2 bb2",
            "30",
            "drop 3 ccc3",
            "drop 4 dddd4",
            "40",
            "5",
            "drop 5 eeeee5",
            "end",
        ],
        "result_struct_payload_field_freed",
    );
}

#[test]
fn asan_mixed_halves_result_struct_field_freed() {
    // B-2026-08-03-11 — the LEAK oracle for the mixed `Result[<struct>,
    // String]` field. This one is invisible to the other two oracles by
    // construction: the interpreter agrees with AOT on every line (nothing
    // is printed wrong, no body double-fires), and the Drop body count is
    // correct. Only LSan sees that the Ok payload's `name` buffer is never
    // freed — and only on Linux, since `-fsanitize=address` runs no leak
    // detector on macOS.
    //
    // `swapped-sides` puts the Vec on Ok and the struct on Err to pin that
    // the admit is per HALF, not per position.
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Hm { r: Result[Res, String], t: i64 }
struct Hv { r: Result[Vec[String], Res], t: i64 }
fn take(h: Hm) -> i64 { h.t }
fn main() {
    { let h = Hm { r: Result.Ok(Res { id: 1, name: f"aaaa{1}" }), t: 10 }; println(h.t); }
    { let h = Hm { r: Result.Err(f"bbbbbb{2}"), t: 20 }; println(h.t); }
    { let h = Hm { r: Result.Ok(Res { id: 3, name: f"ccc{3}" }), t: 30 }; println(take(h)); }
    { let h = Hm { r: Result.Ok(Res { id: 4, name: f"dddd{4}" }), t: 40 }; let x = h.r; println(h.t); }
    {
      let mut v: Vec[String] = Vec.new();
      v.push(f"eeeee{5}");
      let h = Hv { r: Result.Ok(v), t: 50 };
      println(h.t);
    }
    { let h = Hv { r: Result.Err(Res { id: 6, name: f"ffffff{6}" }), t: 60 }; println(h.t); }
    println("end");
}
"#,
        &[
            "10",
            "drop 1 aaaa1",
            "20",
            "30",
            "drop 3 ccc3",
            "drop 4 dddd4",
            "40",
            "50",
            "60",
            "drop 6 ffffff6",
            "end",
        ],
        "mixed_halves_result_struct_field_freed",
    );
}

#[test]
fn asan_option_struct_field_move_out_no_double_free() {
    // B-2026-08-03-8 (memory half) — `let x = h.o` with `o: Option[Res]`
    // SEGV'd: `Res` is 4 words so the payload is BOXED, and both `x` and
    // `h`'s `OptionInline` field drop freed the same box — a use-after-free
    // then an invalid free, exit 139, no output at all. The whole-STRUCT move
    // has had this neutralizer since B-2026-07-03-28
    // (`zero_struct_move_caps_mono`'s Option/Result arms); the single-FIELD
    // move-out fell through to the generic enum arm, which is a no-op for
    // Option/Result by construction. `t` is the control that must still be
    // readable after the move.
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct H4 { o: Option[Res], t: i64 }
fn main() {
    println("a");
    {
        let h = H4 { o: Option.Some(Res { id: 1, name: f"aa{1}" }), t: 2 };
        let x = h.o;
        println(h.t);
    }
    println("end");
}
"#,
        &["a", "drop 1 aa1", "2", "end"],
        "option_struct_field_move_out_no_double_free",
    );
}

#[test]
fn asan_tuple_held_optres_payload_freed() {
    // B-2026-08-03-3 — the leak this row was filed for. An `Option[P]` /
    // `Result[O, E]` held inside a tuple got NO memory drop in any position
    // (`emit_tuple_elem_drops` no-op'd on both heads), so the payload's heap
    // was orphaned. Six positions here; the last two also needed the walker
    // SELECTOR widened, since it read only inner head names. Covers the
    // move-out shape too: cap-zeroing alone would have double-freed once the
    // tuple drop started freeing, so the Option/Result neutralizer landed in
    // the same slice.
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
fn take(t: (Option[Res], i64)) -> i64 { t.1 }
fn mk() -> (Option[Res], i64) { (Option.Some(Res { id: 3, name: f"c{3}" }), 30) }
fn main() {
    { let t = (Option.Some(Res { id: 1, name: f"a{1}" }), 10); println(t.1); }
    { let t = (Option.Some(Res { id: 2, name: f"bb{2}" }), 20); println(take(t)); }
    { let t = mk(); println(t.1); }
    { let t: (Result[Res, i64], i64) = (Result.Ok(Res { id: 4, name: f"dddd{4}" }), 40); println(t.1); }
    {
        let mut v: Vec[(Option[Res], i64)] = Vec.new();
        v.push((Option.Some(Res { id: 5, name: f"eeeee{5}" }), 50));
        println(v.len());
    }
    { let t = ((Option.Some(Res { id: 6, name: f"ffffff{6}" }), 60), 600); println(t.1); }
    {
        let t = (Option.Some(Res { id: 7, name: f"ggggggg{7}" }), 70);
        let x = t.0;
        println(t.1);
    }
    println("end");
}
"#,
        &[
            "10",
            "drop 1 a1",
            "20",
            "drop 2 bb2",
            "30",
            "drop 3 c3",
            "40",
            "drop 4 dddd4",
            "1",
            "drop 5 eeeee5",
            "600",
            "drop 6 ffffff6",
            "drop 7 ggggggg7",
            "70",
            "end",
        ],
        "tuple_held_optres_payload_freed",
    );
}

/// B-2026-08-09-8 — the same read, one `let p = o;` in front of it.
///
/// A bare rebind is a whole-value MOVE that the let-site's registration gate
/// could not see, so the destination got no cleanup and the source kept its.
/// The destination was then invisible to `scrutinee_is_inline_optres_local`
/// — the membership test the caller-retains classifier consults — so every
/// `match p` took the OWNED path, freed at arm exit, and the source's
/// untouched scope-exit action freed the same buffer again.
///
/// Both directions are in one fixture on purpose. The double free / UAF is
/// what the row reports; the loop plus the payload read-back catch the
/// opposite mistake, because the fix DISARMS the source, and disarming
/// without registering the destination would turn this into a per-iteration
/// leak that LSan reports rather than an abort.
///
/// The last block is the shape that chose transfer over alias: reassigning
/// the SOURCE after the move. Recording the destination as a passthrough
/// alias fixes every other case here but leaks 2 bytes on that one
/// (measured, both variants built), because the surviving action would read
/// the source's slot after it had been overwritten.
#[test]
fn asan_rebound_option_local_transfers_payload_ownership() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    while i < 8 {
        let o: Option[String] = Some(f"hi{i}");
        let p: Option[String] = o;
        match p { Some(v) => { println(v); } None => { println("-"); } }
        match p { Some(v) => { println(v); } None => { println("-"); } }
        // Chained rebind: the transfer has to keep moving, not stop at `p`.
        let q: Option[String] = p;
        match q { Some(v) => { println(v); } None => { println("-"); } }
        // Vec payload, element read (touches the buffer).
        let vo: Option[Vec[i64]] = Some(vec![i, i + 1i64]);
        let vp: Option[Vec[i64]] = vo;
        match vp { Some(x) => { println(x[1].to_string()); } None => { println("-"); } }
        // Result sibling registry.
        let ro: Result[String, i64] = Ok(f"ok{i}");
        let rp: Result[String, i64] = ro;
        match rp { Ok(v) => { println(v); } Err(_) => { println("-"); } }
        // Source reassigned after the move — clean only if the DESTINATION owns.
        let mut mo: Option[String] = Some(f"mv{i}");
        let mp: Option[String] = mo;
        match mp { Some(v) => { println(v); } None => { println("-"); } }
        mo = None;
        match mo { Some(v) => { println(v); } None => { println("-"); } }
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "hi0", "hi0", "hi0", "1", "ok0", "mv0", "-", "hi1", "hi1", "hi1", "2", "ok1", "mv1",
            "-", "hi2", "hi2", "hi2", "3", "ok2", "mv2", "-", "hi3", "hi3", "hi3", "4", "ok3",
            "mv3", "-", "hi4", "hi4", "hi4", "5", "ok4", "mv4", "-", "hi5", "hi5", "hi5", "6",
            "ok5", "mv5", "-", "hi6", "hi6", "hi6", "7", "ok6", "mv6", "-", "hi7", "hi7", "hi7",
            "8", "ok7", "mv7", "-", "end",
        ],
        "rebound_option_local_transfers_payload_ownership",
    );
}

/// B-2026-08-11-11 under ASAN — `get(k).unwrap_or(d)` over a heap value
/// must hand back a buffer the container does not also own.
///
/// The default opt level aborts outright on this (`free(): double free
/// detected in tcache 2`) while -O0 shows a single silent invalid read, so
/// the two levels disagree about how bad it looks; ASAN names it either way.
///
/// Case 3 is the direction that would break if the clone were made
/// unconditional: on an ABSENT key the result IS the caller's own default,
/// and cloning it there would leak one buffer per call. LSan is what holds
/// that line.
#[test]
fn asan_borrow_accessor_unwrap_or_clones_heap_payload() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    while i < 4i64 {
        // 1. Map value — the filed reproduction.
        let mut m: Map[String, String] = Map.new();
        m.insert(f"k{i}", f"al{i}");
        println(m.get(f"k{i}").unwrap_or(f"-"));
        // 2. Vec element — the same accessor family.
        let mut vv: Vec[String] = Vec.new();
        vv.push(f"bb{i}");
        println(vv.get(0).unwrap_or(f"-"));
        // 3. CONTROL — absent key, so the DEFAULT is returned and must not be
        // cloned (that would leak it once per iteration).
        println(m.get(f"zz{i}").unwrap_or(f"dd{i}"));
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "al0", "bb0", "dd0", "al1", "bb1", "dd1", "al2", "bb2", "dd2", "al3", "bb3", "dd3",
            "end",
        ],
        "borrow_accessor_unwrap_or",
    );
}

/// B-2026-08-11-30, the safety property the fix rests on. Own-by-transfer
/// registers the callee's payload drop WITHOUT an entry copy, which is only
/// sound because the caller has already zeroed the source slot. Passing the
/// same binding TWICE therefore hands the second call all zeros, and every
/// `cap > 0` guard in its drop must skip — otherwise this is a double free,
/// which is the failure mode the first three passes of the row declined to
/// risk. Kāra deliberately accepts double-consume (param_own.rs, "Why not
/// move-by-default"), so this shape has to keep working.
///
/// Reads the payload in the callee as well, so a drop that fired twice
/// would show up as a use-after-free rather than passing quietly.
#[test]
fn asan_by_value_optres_arg_passed_twice_no_double_free() {
    assert_clean_asan_run(
        r#"
enum E { Missing(String) }
fn mkv() -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push("x"); return v; }
fn mk() -> Result[Vec[String], E] { return Ok(mkv()); }
fn takr(r: Result[Vec[String], E]) -> i64 { match r { Ok(v) => v.len(), Err(_e) => -1 } }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let r = mk();
        acc = acc + takr(r);
        acc = acc + takr(r);
        i = i + 1;
    }
    println(f"{acc > -1000}");
}
"#,
        &["true"],
        "by_value_optres_arg_passed_twice",
    );
}

#[test]
fn asan_probe_shared_struct_niche_option_field_eq() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, PartialEq)]
shared struct Tiny { next: Option[Tiny] }
fn main() {
    let mut i = 0i64;
    let mut hits = 0i64;
    while i < 50i64 {
        let a = Tiny { next: None };
        let b = Tiny { next: None };
        if a == b { hits = hits + 1i64; }
        i = i + 1;
    }
    println(f"{hits}");
}
"#,
        &["50"],
        "shared-niche-option-field-eq-probe",
    );
}

/// B-2026-09-01-29 — a fresh-temp `Option`/`Result` argument handed to a
/// callee that STORES it must not also be owned by the caller.
///
/// `track_optres_arg_temp` (B-2026-08-12-1) gives the caller ownership of a
/// fresh temp on the reasoning that the callee entry-copies the param and
/// frees only its own copy. That reasoning holds only while the callee
/// actually copies, and it does not: the entry copy (functions.rs) fires
/// solely for a param in `nonescaping_param_names`, and a param the callee
/// pushes into a `mut ref` accumulator is escaping by that analysis's own
/// definition. So for a STORING callee the caller owned a buffer the
/// container also owns, and the program aborted:
///
///     fn sink(acc: mut ref Vec[Option[String]], x: Option[String]) { acc.push(x) }
///     sink(mut acc, Some(mks(i)))     ASAN: attempting double-free
///
/// The `let`-bound spelling of the same value is clean, because an
/// identifier argument is not a temp and never reached the ownership at
/// all — which is what isolates this to the fresh-temp path.
///
/// LEAK DETECTION WAS OFF HERE when this landed, because standing the
/// caller's owner down left NOBODY freeing the buffer: the callee did not
/// copy it, and its own param slot registers no cleanup for an escaping
/// param either. That was a leak where there had been a double free —
/// strictly safer and strictly incomplete — and the fixture carried an
/// instruction to move to `assert_clean_asan_run` once the leak half
/// landed. B-2026-09-01-35 landed it, so this is now a full balance
/// assertion; the `detect_leaks=0` form is gone.
///
/// Three call shapes, because each reaches a DIFFERENT ownership site with
/// its own copy of the gate: a free function (`call_dispatch.rs`), a method
/// (`method_call.rs`), and an associated function (`assoc_call.rs`).
#[test]
fn asan_stored_optres_temp_arg_has_one_owner() {
    let cases: &[(&str, &str)] = &[
        (
            "free fn",
            r#"
fn mks(n: i64) -> String { return f"payloadpayload{n}"; }
fn sink(acc: mut ref Vec[Option[String]], x: Option[String]) { acc.push(x); }
fn main() {
    let mut acc: Vec[Option[String]] = Vec.new();
    let mut i = 0;
    while i < 3 {
        sink(mut acc, Some(mks(i)));
        println(f"{acc.len()}");
        i = i + 1;
    }
    println("done");
}
"#,
        ),
        (
            "method",
            // The accumulator is a LOCAL rather than a field of the
            // receiver, deliberately. The `self.xs.push(x)` spelling of the
            // same test leaks 173 B in 4 allocations — a `Vec[Option[
            // String]]` STRUCT FIELD never frees its elements — and that is
            // an independent defect measured identically on an unmodified
            // tree, filed as B-2026-09-01-41. Pinning it here would make
            // this fixture fail for a reason that has nothing to do with
            // argument ownership; the local-accumulator spelling exercises
            // the same method-path escape route and is clean.
            r#"
struct Mk { n: i64 }
impl Mk {
    fn sink(ref self, acc: mut ref Vec[Option[String]], x: Option[String]) { acc.push(x); }
}
fn mks(n: i64) -> String { return f"payloadpayload{n}"; }
fn main() {
    let m = Mk { n: 1 };
    let mut acc: Vec[Option[String]] = Vec.new();
    let mut i = 0;
    while i < 3 {
        m.sink(mut acc, Some(mks(i)));
        println(f"{acc.len()}");
        i = i + 1;
    }
    println("done");
}
"#,
        ),
        (
            "assoc fn",
            r#"
struct Bag { xs: Vec[Option[String]] }
impl Bag {
    fn fill(acc: mut ref Vec[Option[String]], x: Option[String]) { acc.push(x); }
}
fn mks(n: i64) -> String { return f"payloadpayload{n}"; }
fn main() {
    let mut acc: Vec[Option[String]] = Vec.new();
    let mut i = 0;
    while i < 3 {
        Bag.fill(mut acc, Some(mks(i)));
        println(f"{acc.len()}");
        i = i + 1;
    }
    println("done");
}
"#,
        ),
    ];
    for (label, src) in cases {
        assert_clean_asan_run(
            src,
            &["1", "2", "3", "done"],
            &format!("b29-stored-optres-temp-{label}"),
        );
    }
}

/// B-2026-09-02-12 — the memory half of the declined-temporary fix.
///
/// The defect was bodies-only (`valgrind --leak-check=full` at
/// `KARAC_OPT_LEVEL=0` showed the declined `Result`'s `String` buffer
/// freed, `definitely lost: 0`, with the same profile as the correct
/// `match` spelling), so the fix ADDS a body over storage the existing
/// drop still frees — which is exactly the shape that becomes a
/// use-after-free or a double free if the two get out of order.
///
/// Both hazards are live here and both are gated. The bodies READ
/// `self.s.len()` through the payload's heap buffer, so a body emitted
/// AFTER the drop that frees it reads poisoned memory; and the miss-edge
/// call spills the value into its own slot (an all-scalar `Result`
/// materializes nowhere on these paths, so there is no slot to be handed),
/// so a walker that freed rather than merely read would double-free the
/// caller's copy. Measured clean: 32 allocs / 32 frees, 0 errors.
///
/// The loop reallocates on every pass, so a stale pointer lands on reused
/// memory rather than on quiet garbage.
#[test]
fn asan_declined_optres_temporary_body_reads_live_memory() {
    assert_clean_asan_run(
        r#"
fn pad(t: i64) -> String {
    let mut s: String = String.new();
    s.push_str("payload-padded-out-well-past-thirty-six-bytes-");
    s.push_str(f"{t}");
    return s;
}
struct W { s: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.s.len()}"); } }
fn mkerr() -> Result[W, W] { return Err(W { s: pad(7) }); }
fn viaelse() -> i64 {
    let Ok(w) = mkerr() else { return 0 };
    return w.s.len();
}
fn main() {
    let mut i = 0;
    while i < 3 {
        if let Ok(w) = mkerr() { println(f"a{w.s.len()}"); }
        println(f"e{viaelse()}");
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "dW47", "dW47", "e0", "dW47", "dW47", "e0", "dW47", "dW47", "e0", "done",
        ],
        "b0902-12-declined-optres-body-live",
    );
}

/// B-2026-09-03-19 — a WHOLE-VALUE REBIND of a tuple holding an
/// `Option[<heap struct>]` element double-freed the payload.
///
/// `let t = (mkD(1), Option.Some(mkD(2))); let t2 = t;` — no destructure, no
/// `match`, no move-out, and (in the `noDrop` case) NO `impl Drop` anywhere
/// in the program. The pre-fix binary segfaults before printing a single
/// line; valgrind reported three `Invalid free()`, each naming a block freed
/// earlier in the same run, so a duplicate owner rather than a wild pointer.
///
/// THE MEMORY HALF OF B-2026-07-31-22, which fixed the BODIES half of the
/// same statement ("a whole-value MOVE of a binding carrying a
/// container-bodies walk left the source's `__karac_dropelems_*` action
/// armed"). `noDrop` is the case that separates them: with no user `Drop`
/// there is no bodies walker at all and the abort is unchanged.
///
/// THE TWO BRANCHES ARE THE POINT. A tuple `let` picks its drop
/// registration three ways, and the source neutralizer that follows —
/// `suppress_source_vec_cleanup_for_arg` — is LLVM-type driven. That matches
/// the `aggregate_has_heap_field` branch, whose drop is the LLVM-type
/// walker, and `ctlPlain` (`(D, D)`) rides it and was always clean. An
/// `Option[<heap struct>]` element instead selects the deeper,
/// `TypeExpr`-driven `synthesize_tuple_drop_fn_te` — measured directly, by
/// instrumenting which branch each program takes — and that walker frees a
/// payload sitting inline behind a tag, which the neutralizer cannot see. So
/// the destination freed it and the un-neutralized source freed it again.
/// The fix pairs the deep drop with the deep neutralizer
/// (`zero_tuple_elem_caps`, the declared dual of `emit_tuple_elem_drops`,
/// which has carried an Option/Result arm since B-2026-08-03-3).
///
/// EVERY CONTROL IS A MEASURED INGREDIENT, not a guess: removing any one of
/// them made the pre-fix program clean. `ctlPlain` has no Option; `ctlNoReb`
/// omits the rebind; `ctlNone` is `None` at runtime. `strElem0` shows the
/// other element need only own heap — a bare `String` is enough, so this was
/// never struct-specific.
///
/// TWO FURTHER CONTROLS ARE DELIBERATELY ABSENT, and their absence is a
/// measurement rather than a gap. `(0, Option.Some(mkD(8)))` and
/// `(mkD(9), Option.Some(f"p{9}"))` both leak here — 56 and 2 bytes — but
/// PRE-FIX AND POST-FIX ALIKE, and only when the other tuple shapes in this
/// file are also DEFINED: each is clean as a one-function program and clean
/// in a controls-only program. Compiling all eight functions and calling
/// only the five that do not crash pre-fix gives 58 bytes on both sides of
/// the fix, which is what rules this change out as the cause. Filed as its
/// own row; including them here would leave this test red for someone
/// else's defect.
#[test]
fn asan_tuple_rebind_with_an_optres_element_frees_once() {
    assert_clean_asan_run(
            "struct D { id: i64, xs: Vec[i64], name: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"  dD{self.id}:{self.xs.len()}:{self.name}\") } }\n\
             fn mkD(id: i64) -> D {\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   v.push(id);\n\
             \x20   return D { id: id, xs: v, name: f\"n{id}\" }\n\
             }\n\
             struct Q { id: i64, xs: Vec[i64], name: String }\n\
             fn mkQ(id: i64) -> Q {\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   v.push(id);\n\
             \x20   return Q { id: id, xs: v, name: f\"q{id}\" }\n\
             }\n\
             fn optHeap()   { let t = (mkD(1), Option.Some(mkD(2))); let t2 = t; println(\"  oh\") }\n\
             fn noDrop()    { let t = (mkQ(3), Option.Some(mkQ(4))); let t2 = t; println(\"  nd\") }\n\
             fn strElem0()  { let t = (f\"s{5}\", Option.Some(mkD(5))); let t2 = t; println(\"  se\") }\n\
             fn ctlPlain()  { let t = (mkD(6), mkD(7)); let t2 = t; println(\"  cp\") }\n\
             fn ctlNoReb()  { let t = (mkD(10), Option.Some(mkD(11))); println(\"  cn\") }\n\
             fn ctlNone()   { let t: (D, Option[D]) = (mkD(12), Option.None); let t2 = t; println(\"  cz\") }\n\
             fn main() {\n\
             \x20   optHeap(); noDrop(); strElem0(); ctlPlain();\n\
             \x20   ctlNoReb(); ctlNone();\n\
             \x20   println(\"end\")\n\
             }\n",
            &[
                "dD1:1:n1",
                "  dD2:1:n2",
                "  oh",
                "  nd",
                "  dD5:1:n5",
                "  se",
                "  dD6:1:n6",
                "  dD7:1:n7",
                "  cp",
                "  dD10:1:n10",
                "  dD11:1:n11",
                "  cn",
                "  dD12:1:n12",
                "  cz",
                "end",
            ],
            "b19-tuple-rebind-optres-elem",
        );
}

/// B-2026-09-03-6 — A BY-VALUE `Result[Option[H], E]` PARAM DOUBLE-FREES ITS
/// INNER PAYLOAD. `karac run` aborted with
/// `free(): double free detected in tcache 2` on a SINGLE call; the AOT
/// binary logged an `Invalid free()` per call under valgrind and only
/// escaped a crash because glibc tolerated it in that arrangement. With a
/// `Vec` payload the AOT binary aborted as well.
///
/// `optres_param_entry_copied_te` admits the param, so the caller stops
/// retaining the temp on the promise that the callee owns its own copy. The
/// copy is emitted by `deep_copy_result_struct_enum_payload_in_place`, which
/// classified the `Option[String]` half as "an enum half" (`Option` is in
/// `enum_layouts`) and handed it to `deep_copy_enum_heap_payload_in_place` —
/// a BY-NAME copier that reads `field_drop_kinds["Some"]` of the ERASED
/// generic `Option` declaration, whose payload is the type parameter `T`.
/// `T` is not heap-bearing, so the `Some` case was not even emitted and the
/// half was copied by NOTHING. `emit_result_drop_fn` meanwhile recurses with
/// the fully instantiated TypeExpr and frees the payload for real, so copy
/// depth was shallower than free depth — the one invariant this family's doc
/// comments say must hold — and the two frames freed one buffer.
///
/// The fix routes a nested `Option`/`Result` half to
/// `deep_copy_optres_param_in_place`, the TYPE-EXPR-driven copier, at the
/// same `payload_base` (the Result's word 1) that `emit_result_drop_fn`
/// hands the half's drop fn.
///
/// THE DISCRIMINATORS ARE ALL HERE, each measured:
///
///   * `Result[String, E]` — the un-nested control, always clean.
///   * `Result[Option[i64], E]` — nested but heapless, always clean.
///   * `Result[String, Option[String]]` — the OK half is direct and the ERR
///     half is the nested one, and it was always clean, because only the
///     LIVE half is copied and these programs build `Ok`.
///   * `Option[Result[String, E]]` — the other NESTING ORDER, always clean:
///     the outer `Option`'s own copier is TypeExpr-driven already.
///   * `Err(e)` and `Ok(None)` — the live half carries no nested payload.
///
/// The body deliberately does NOT read `x` in one case: the abort survives a
/// callee that only prints a constant, which is what places this on the
/// entry-copy path rather than the render path (B-2026-09-03-5's subject).
#[test]
fn asan_nested_optres_by_value_param_owns_its_own_payload() {
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "result-option-string",
            r#"
fn show(x: Result[Option[String], String]) { println(f"{x}"); }
fn main() {
    let mut i = 0;
    while i < 3 { let s = f"payloadpayload{i}"; show(Ok(Some(s))); i = i + 1; }
    println("done");
}
"#,
            &[
                "Ok(Some(payloadpayload0))",
                "Ok(Some(payloadpayload1))",
                "Ok(Some(payloadpayload2))",
                "done",
            ],
        ),
        (
            "result-option-vec",
            r#"
fn show(x: Result[Option[Vec[i64]], String]) { println(f"{x}"); }
fn main() {
    let mut i = 0;
    while i < 3 { let v: Vec[i64] = [i, i + 1]; show(Ok(Some(v))); i = i + 1; }
    println("done");
}
"#,
            &[
                "Ok(Some([0, 1]))",
                "Ok(Some([1, 2]))",
                "Ok(Some([2, 3]))",
                "done",
            ],
        ),
        (
            "callee-never-reads-the-param",
            r#"
fn show(x: Result[Option[String], String]) { println("in"); }
fn main() {
    let mut i = 0;
    while i < 3 { let s = f"payloadpayload{i}"; show(Ok(Some(s))); i = i + 1; }
    println("done");
}
"#,
            &["in", "in", "in", "done"],
        ),
        (
            "heapless-err-half",
            r#"
fn show(x: Result[Option[String], i64]) { println(f"{x}"); }
fn main() {
    let mut i = 0;
    while i < 3 { let s = f"payloadpayload{i}"; show(Ok(Some(s))); i = i + 1; }
    println("done");
}
"#,
            &[
                "Ok(Some(payloadpayload0))",
                "Ok(Some(payloadpayload1))",
                "Ok(Some(payloadpayload2))",
                "done",
            ],
        ),
        // CONTROLS — clean before this change and after it.
        (
            "control-unnested-result",
            r#"
fn show(x: Result[String, String]) { println(f"{x}"); }
fn main() {
    let mut i = 0;
    while i < 3 { let s = f"payloadpayload{i}"; show(Ok(s)); i = i + 1; }
    println("done");
}
"#,
            &[
                "Ok(payloadpayload0)",
                "Ok(payloadpayload1)",
                "Ok(payloadpayload2)",
                "done",
            ],
        ),
        // The OTHER NESTING ORDER, `Option[Result[String, String]]`, is
        // deliberately NOT pinned here. Its OUTPUT is correct on every
        // surface (the codegen twin asserts that), but it LEAKS 45 B in 3
        // allocations, which would fail this fixture for a defect that has
        // nothing to do with the double free. It is B-2026-09-02-22's shape,
        // not this one: `optres_param_entry_copied_te` gates an `Option`
        // param's payload at 3 words and a `Result` is 6, so the param is
        // never ADMITTED to the entry-copy convention at all and no frame is
        // offered the temp. Measured identical to that row's own
        // `Option[Option[String]]` figure, and unchanged by this fix, which
        // only runs for an ADMITTED param — the measurement is recorded on
        // that row, whose "NOT MEASURED" list asked for exactly it.
        (
            "control-heapless-nested-payload",
            r#"
fn show(x: Result[Option[i64], String]) { println(f"{x}"); }
fn main() {
    let mut i = 0;
    while i < 3 { show(Ok(Some(i))); i = i + 1; }
    println("done");
}
"#,
            &["Ok(Some(0))", "Ok(Some(1))", "Ok(Some(2))", "done"],
        ),
        (
            "control-nested-half-is-the-dead-one",
            r#"
fn show(x: Result[String, Option[String]]) { println(f"{x}"); }
fn main() {
    let mut i = 0;
    while i < 3 { let s = f"payloadpayload{i}"; show(Ok(s)); i = i + 1; }
    println("done");
}
"#,
            &[
                "Ok(payloadpayload0)",
                "Ok(payloadpayload1)",
                "Ok(payloadpayload2)",
                "done",
            ],
        ),
        (
            "control-none-payload",
            r#"
fn show(x: Result[Option[String], String]) { println(f"{x}"); }
fn main() {
    let mut i = 0;
    while i < 3 { show(Ok(None)); i = i + 1; }
    println("done");
}
"#,
            &["Ok(None)", "Ok(None)", "Ok(None)", "done"],
        ),
        (
            "control-err-half-live",
            r#"
fn show(x: Result[Option[String], String]) { println(f"{x}"); }
fn main() {
    let mut i = 0;
    while i < 3 { let e = f"errerrerrerrerr{i}"; show(Err(e)); i = i + 1; }
    println("done");
}
"#,
            &[
                "Err(errerrerrerrerr0)",
                "Err(errerrerrerrerr1)",
                "Err(errerrerrerrerr2)",
                "done",
            ],
        ),
    ];
    for (label, src, want) in cases {
        assert_clean_asan_run(src, want, &format!("b6-nested-optres-param-{label}"));
    }
}

/// B-2026-09-02-46 — a fresh-temp `Option`/`Result` argument to a GENERIC
/// callee, whose payload THIS INSTANTIATION heap-boxes, must have an owner.
///
/// The box is malloc'd by `coerce_to_payload_words`' oversize arm at the
/// call site and, before the fix, was owned by nobody: `compile_generic_call`
/// registers no caller-side optres arg ownership, and the mono param
/// prologue registers only `user_enum_boxed_payload_variants`, which returns
/// nothing for the seeded `Option`/`Result` pair by design. 48 bytes per
/// call — one box, six i64 words — with the payload's own Strings freed by
/// the arm that binds them, so `indirectly lost` was 0 and only the envelope
/// was stranded.
///
/// EVERY NON-GENERIC TWIN IS CLEAN, which is what makes this a monomorph
/// defect rather than a boxed-payload one: `compile_function`'s owned-param
/// arms give the CALLEE the box, and `compile_call` disarms a binding
/// argument's let-site drop at the move so the two never collide. The
/// generic path runs neither half.
///
/// The four leaking cells are the shapes the row measured — an `Array`
/// payload, a two-`String` TUPLE payload, `Result` rather than `Option`, and
/// a generic METHOD — each 48 B, all four at every opt level from `-O0` to
/// `-O3` (measured; this is NOT an `-O0`-only defect, so the fixture asserts
/// something on the `-O2` build CI runs).
///
/// THE LAST TWO CELLS ARE THE OTHER DIRECTION and are the reason the fix
/// carries an escape gate. `passthru` RETURNS its param, so the box leaves
/// with the result and the caller's `back` binding owns it; owning it at the
/// arg site too was measured as a valgrind `Invalid free` on a program clean
/// both before the fix and after it. `bound` hands over a NAMED binding,
/// whose let-site box drop the generic path never disarms — so it is
/// already the sole owner and must stay that way. A double free in either
/// cell means the gate has been widened.
///
/// The alloc floor guards the whole fixture against folding away; a version
/// LLVM elided entirely would assert nothing while reporting green.
#[test]
fn asan_generic_callee_boxed_optres_temp_arg_frees_its_box() {
    let src = r#"
struct H { id: i64 }

impl H {
    fn show[T: Display](ref self, x: Option[T]) -> i64 {
        match x {
            Some(t) => { println(f"m{self.id}:{t}"); return 1; }
            None => { return 0; }
        }
    }
}

fn takesOpt[T: Display](x: Option[T]) -> i64 {
    match x {
        Some(t) => { println(f"o:{t}"); return 1; }
        None => { return 0; }
    }
}

fn takesRes[T: Display](x: Result[T, i64]) -> i64 {
    match x {
        Ok(t) => { println(f"r:{t}"); return 1; }
        Err(e) => { return e; }
    }
}

fn passthru[T](x: Option[T]) -> Option[T] { return x; }

fn main() {
    let n = env.args().len();

    // Cell 1 — an `Array[String, 2]` payload: 6 words, past `Option`'s
    // 3-word area, so the monomorph boxes it.
    let a: Array[String, 2] = [f"uv{n}", f"wx{n}"];
    let c1 = takesOpt(Some(a));

    // Cell 2 — a TUPLE payload, proving the array is only the shape that
    // made this reachable and not the cause. Two `String`s: 6 words, past the
    // 3-word area, so it boxes AND carries heap of its own inside the box.
    //
    // B-2026-09-06-48 — this cell was four SCALAR words until that row closed,
    // because a two-`String` tuple additionally lost its interior in a
    // monomorph and this fixture must not silently depend on a live defect.
    // Widening it back is what that row's note asked for, and it is now the
    // cell that would catch a regression of it here as well as in the row's
    // own fixture.
    let p = (f"ab{n}", f"cd{n}");
    let c2 = takesOpt(Some(p));

    // Cell 3 — `Result` rather than `Option`, boxing against the 5-word area.
    let b: Array[String, 2] = [f"yz{n}", f"za{n}"];
    let c3 = takesRes(Ok(b));

    // Cell 4 — a generic METHOD, which reaches the same arg site.
    let h = H { id: 7 };
    let d: Array[String, 2] = [f"mn{n}", f"op{n}"];
    let c4 = h.show(Some(d));

    // Cell 5 — the ESCAPE control: the callee hands the box back, so the
    // result binding is its owner and the arg site must NOT take it.
    let e: Array[String, 2] = [f"pq{n}", f"rs{n}"];
    let back = passthru(Some(e));
    let c5 = takesOpt(back);

    // Cell 6 — the BINDING control: a named binding's let-site box drop is
    // the sole owner on this path and is never disarmed.
    let g: Array[String, 2] = [f"tu{n}", f"vw{n}"];
    let bound: Option[Array[String, 2]] = Some(g);
    let c6 = takesOpt(bound);

    println(f"acc{c1 + c2 + c3 + c4 + c5 + c6}");
}
"#;
    assert_clean_asan_run_min_allocs(
        src,
        &[
            "o:[uv1, wx1]",
            "o:(ab1, cd1)",
            "r:[yz1, za1]",
            "m7:[mn1, op1]",
            "o:[pq1, rs1]",
            "o:[tu1, vw1]",
            "acc6",
        ],
        "b0902-46-generic-boxed-optres-temp-arg",
        20,
    );
}

/// B-2026-09-06-48 — a GENERIC callee's boxed `Option`/`Result` payload
/// frees its own heap interior, not just the box around it.
///
/// The generic half of B-2026-09-04-12. That row gave the NON-GENERIC
/// by-value param's `BoxedEnumDrop` the tuple's own drop; the generic path
/// never reached it, because the two spellings have DIFFERENT owners for
/// one box — there the callee (`compile_function`'s leg A), here the caller
/// (`track_boxed_optres_arg_temp`, B-2026-09-02-46), whose registration was
/// box-only. 54 B in 6 blocks over three calls, one lost element per heap
/// element of the payload.
///
/// THE CONSUMING-ARM CELL IS THE DOUBLE-FREE CONTROL, and it is the reason
/// the fix is a caller-side AST question rather than the obvious port. An
/// arm that TAKES the payload (`let u = t`) already owns and frees it, so
/// arming the caller's interior drop as well is a second owner. Codegen's
/// own retraction (`retract_boxed_tuple_inner_drop_for_arm`) cannot reach
/// the caller's action here — the monomorph body compiles with
/// `scope_cleanup_actions` SWAPPED, so `clear_boxed_enum_inner_drop` never
/// sees that frame — which is why the caller asks
/// `optres_payload_consuming_param_names` up front and declines to arm what
/// it could not retract. Get that backwards and this cell aborts rather
/// than leaks, so it fails LOUDLY in the dangerous direction.
///
/// THE BINDING CELL IS THE OTHER CONTROL. A named `Option` argument's
/// let-site drop already owns box AND interior on this path and is never
/// disarmed, so it was clean before this fix and must stay exactly one
/// owner after it.
#[test]
fn asan_generic_callee_boxed_optres_payload_frees_its_interior() {
    let src = r#"
fn readsIt[T: Display](x: Option[T]) -> i64 {
    match x {
        Some(t) => { println(f"o:{t}"); return 1; }
        None => { return 0; }
    }
}

fn takesIt[T: Display](x: Option[T]) -> i64 {
    match x {
        Some(t) => { let u = t; println("took"); return 1; }
        None => { return 0; }
    }
}

fn wildArm[T: Display](x: Option[T]) -> i64 {
    match x {
        Some(_) => { println("some"); return 1; }
        None => { return 0; }
    }
}

fn readsRes[T: Display](x: Result[T, i64]) -> i64 {
    match x {
        Ok(t) => { println(f"r:{t}"); return 1; }
        Err(e) => { return e; }
    }
}

fn main() {
    let n = env.args().len();

    // Cell 1 — the row's own shape: a two-`String` tuple through a fresh-temp
    // `Option` argument, read-only in the arm. This is the 54 B.
    let p = (f"aaaa{n}", f"bbbb{n}");
    let c1 = readsIt(Some(p));

    // Cell 2 — a THREE-element payload, so a fix that frees only the first
    // element is caught (the row measured 81 B in 9 for this one).
    let q = (f"cccc{n}", f"dddd{n}", f"eeee{n}");
    let c2 = readsIt(Some(q));

    // Cell 3 — a MIXED payload: the scalar words must not be walked as heap.
    let r = (f"ffff{n}", n, n + 1, n + 2);
    let c3 = readsIt(Some(r));

    // Cell 4 — the CONSUMING-arm control. Aborts on a double free if the
    // caller arms an interior drop the callee already owns.
    let s = (f"gggg{n}", f"hhhh{n}");
    let c4 = takesIt(Some(s));

    // Cell 5 — a wildcard arm binds nothing, so nothing in the callee can own
    // the interior and the caller must.
    let t = (f"iiii{n}", f"jjjj{n}");
    let c5 = wildArm(Some(t));

    // Cell 6 — `Result` rather than `Option`, boxing against the 5-word area.
    let u = (f"kkkk{n}", f"llll{n}", f"mmmm{n}", f"nnnn{n}");
    let c6 = readsRes(Ok(u));

    // Cell 7 — the BINDING control: sole owner is the let site, before and
    // after.
    let v = (f"oooo{n}", f"pppp{n}");
    let bound: Option[(String, String)] = Some(v);
    let c7 = readsIt(bound);

    println(f"acc{c1 + c2 + c3 + c4 + c5 + c6 + c7}");
}
"#;
    assert_clean_asan_run(
        src,
        &[
            "o:(aaaa1, bbbb1)",
            "o:(cccc1, dddd1, eeee1)",
            "o:(ffff1, 1, 2, 3)",
            "took",
            "some",
            "r:(kkkk1, llll1, mmmm1, nnnn1)",
            "o:(oooo1, pppp1)",
            "acc7",
        ],
        "b0906-48-generic-boxed-optres-payload-interior",
    );
}

/// B-2026-09-04-26 — the memory half of
/// `e2e_result_ctor_tuple_temp_arg_keeps_its_payload_drop_body`
/// (tests/codegen.rs), and the cell that decides the row.
///
/// The row was filed believing the `Result` element was memory-CLEAN and
/// that enriching its type would trade a lost `Drop` body for a 56-byte
/// leak. Both halves of that were a `-O2` artifact: the cell reports 17
/// allocs / 15 frees at `KARAC_OPT_LEVEL=0` pre-fix, and at `-O2` only 11
/// allocs, because dead-allocation elimination deletes a `malloc` nothing
/// reads. LSan here runs the default optimization level, so this asserts
/// the balanced state directly rather than the reading that hid it.
#[test]
fn asan_result_ctor_tuple_temp_arg_clean() {
    let label = "result_ctor_tuple_temp_arg";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.xs.len()}") } }
fn mk(k: i64) -> R { return R { id: k, tag: "t", xs: [1, 2, 3] } }
fn resArg(t: (R, Result[R, i64])) { println(f"rd{t.0.id}") }
fn resStr(t: (R, Result[R, String])) { println(f"rs{t.0.id}") }
fn both(a: (R, Result[R, i64]), b: (R, Option[R])) { println(f"bo{a.0.id}{b.0.id}") }
fn main() {
    resArg((mk(2), Result.Ok(mk(22))));
    resArg((mk(3), Result.Ok(mk(33))));
    both((mk(4), Result.Ok(mk(44))), (mk(5), Option.Some(mk(55))));
    resStr((mk(6), Result.Ok(mk(66))));
    resArg((mk(7), Result.Err(9)));
    { let t: (R, Result[R, i64]) = (mk(8), Result.Ok(mk(88))); resArg(t); }
    println("end")
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported an error (exit {:?}); stdout:\n{stdout}",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec![
            "rd2", "dR2/3", "dR22/3", "rd3", "dR3/3", "dR33/3", "bo45", "dR5/3", "dR55/3", "dR4/3",
            "dR44/3", "rs6", "dR6/3", "dR66/3", "rd7", "dR7/3", "rd8", "dR8/3", "dR88/3", "end",
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-04-22 — the memory half of
/// `e2e_result_agg_leaf_boxed_payload_by_value_call_keeps_body`, for the
/// borrow-only arms: with the leaf left armed, its drop frees the boxed
/// payload's contents AND envelope after the call. The row measured the
/// pre-fix cell as balanced at -O2; at -O0 it lost 56 B (15 allocs / 12
/// frees), and LSan here runs the default level, so this pins the real
/// state. The MOVING arms are the next fixture's subject
/// (`asan_agg_leaf_boxed_payload_moving_arm_frees_envelope`,
/// B-2026-09-05-9).
#[test]
fn asan_result_agg_leaf_boxed_payload_by_value_call_clean() {
    let label = "result_agg_leaf_boxed_payload_by_value_call";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { id: i64, x: String, y: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.x}{self.y}") } }
struct HoW { a: R, b: Result[W, String] }
fn eatw(w: W) { println(f"eat{w.id}") }
fn mkw(k: i64) -> W { return W { id: k, x: f"x{k}", y: f"y{k}" } }
fn main() {
    { let t: (R, Result[W, String]) = (R { id: 1 }, Result.Ok(mkw(11))); let (a, b) = t; match b { Result.Ok(w) => eatw(w), Result.Err(e) => println("err") } println("one") }
    { let h: HoW = HoW { a: R { id: 2 }, b: Result.Ok(mkw(22)) }; let HoW { a, b } = h; match b { Result.Ok(w) => eatw(w), Result.Err(e) => println("err") } println("two") }
    { let t: (R, Result[W, String]) = (R { id: 3 }, Result.Ok(mkw(33))); let (a, b) = t; if let Result.Ok(w) = b { eatw(w) } println("three") }
    { let t: (R, Result[W, String]) = (R { id: 4 }, Result.Err("e4")); let (a, b) = t; match b { Result.Ok(w) => eatw(w), Result.Err(e) => println(f"err{e}") } println("four") }
    { let t: (R, Option[W]) = (R { id: 5 }, Option.Some(mkw(55))); let (a, b) = t; match b { Option.Some(w) => eatw(w), Option.None => println("none") } println("five") }
    { let r: Result[W, String] = Result.Ok(mkw(66)); match r { Result.Ok(w) => eatw(w), Result.Err(e) => println("err") } println("six") }
    println("end")
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported an error (exit {:?}); stdout:\n{stdout}",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec![
            "dR1",
            "eat11",
            "dW11/x11y11",
            "one",
            "dR2",
            "eat22",
            "dW22/x22y22",
            "two",
            "dR3",
            "eat33",
            "dW33/x33y33",
            "three",
            "dR4",
            "erre4",
            "four",
            "dR5",
            "eat55",
            "dW55/x55y55",
            "five",
            "eat66",
            "dW66/x66y66",
            "six",
            "end"
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-07-59 — a `.clone()` whose receiver is a NICHE-ENCODED
/// `Option[shared T]` field. The field stores one nullable pointer while
/// the declared type's value shape is the seeded 4-i64 Option, so the
/// method-receiver hoist used to hand `karac_clone_Option_*` a 32-byte view
/// of an 8-byte slot: it read the pointer as the tag and three words off
/// the end of the heap object. The published fixture printed a silent `0`
/// (garbage tag ⇒ `None` ⇒ an empty chain); a one-node reduction segfaults
/// instead, which is the same read landing on a plausible `Some`.
///
/// Gated here rather than only in the E2E suite because the WRONG ANSWER
/// and the BAD READ are separate failures: the answer was fixed by
/// unpacking the niche, and the `+1` accounting by registering the cloned
/// binding. This asserts the second — two by-value uses of the clone,
/// which is what turns an unowned `+1` into a use-after-free on the inner
/// node's refcount word.
#[test]
fn asan_niche_option_field_clone_is_owned_on_every_use() {
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn clone_offset(node: Option[Node], delta: i64) -> Option[Node] {
    match node {
        None => None,
        Some(n) => Some(Node { val: n.val + delta, left: clone_offset(n.left, delta), right: clone_offset(n.right, delta) }),
    }
}
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let mut t = 0i64;
    let mut i = 0i64;
    while i < 20i64 {
        let n: Node = Node { val: 5, left: Some(Node { val: 6, left: None, right: None }), right: None };
        let s = n.left.clone();
        let l0 = clone_offset(s, 10);
        let l1 = clone_offset(s, 20);
        t = t + count_nodes(l0) + count_nodes(l1);
        i = i + 1i64;
    }
    println(f"{t}");
}
"#,
        &["40"],
        "asan-niche-option-field-clone",
    );
}

/// B-2026-09-07-60 — `mk().clone()` over a call returning
/// `Option[shared T]`. It failed codegen outright before, so there is no
/// prior memory behaviour to regress; this pins the ACCOUNTING of the
/// lowering that replaced the error. Clone is identity on a fresh owned
/// rvalue — the clone's `+1` and the discarded temporary's `-1` cancel — so
/// the binding must end up owning exactly one reference, which two by-value
/// uses and a loop make observable in both directions: a missing `+1` reads
/// freed memory on the second use, a spurious one leaks every iteration.
#[test]
fn asan_call_receiver_option_shared_clone_is_owned_once() {
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn clone_offset(node: Option[Node], delta: i64) -> Option[Node] {
    match node {
        None => None,
        Some(n) => Some(Node { val: n.val + delta, left: clone_offset(n.left, delta), right: clone_offset(n.right, delta) }),
    }
}
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn mk() -> Option[Node] { Some(Node { val: 3, left: Some(Node { val: 4, left: None, right: None }), right: None }) }
fn main() {
    let mut t = 0i64;
    let mut i = 0i64;
    while i < 20i64 {
        let s = mk().clone();
        let l0 = clone_offset(s, 10);
        let l1 = clone_offset(s, 20);
        t = t + count_nodes(l0) + count_nodes(l1);
        i = i + 1i64;
    }
    println(f"{t}");
}
"#,
        &["80"],
        "asan-call-receiver-option-shared-clone",
    );
}

/// B-2026-09-10-3 — a BOXED `Result` field's envelope when a consuming site
/// zeroes the source, the `Result` half of B-2026-09-09-19.
///
/// `place_optres_field_move_info_ex` admits a `Result` field whose payload
/// is heap-BOXED, so every consuming site zeroes the field's payload AREA
/// (`zero_result_payload_area`) to hand the value to the binding. For a
/// boxed payload that area IS the box word, so the field's own
/// `karac_drop_Result_<ok>_<err>` reads null and skips: the envelope is
/// neutralized and NOTHING takes it over. That arm's comment asserts the
/// opposite -- "the boxed `BoxedEnumDrop` guards on a non-null box word" --
/// and the guard is exactly what loses the box.
///
/// The `Option` twin has been immune since B-2026-08-06-10, which excludes a
/// boxed payload from the same classifier for the same reason. This mirrors
/// that exclusion onto the `Result` arm; declining leaves the field's drop
/// armed, and `emit_result_drop_fn`'s slice-3u boxed branch already runs the
/// contents' drop and then frees the box.
///
/// FOUR shapes leaked, 240 B in 3 blocks (the 80-byte envelopes) plus 36 B
/// INDIRECT in 9 (`R2`'s `String`s, reachable only through them) at -O0
/// each, and all four are clean after:
///
///   - `d`: the destructuring arm `Ok(K.A(r))`, the shape the row was
///     filed on;
///   - `wb`: the WHOLE-payload bind `Ok(kk)`. This is the one that separates
///     the `Result` side from the `Option` side rather than merely lagging
///     it: the `Option` spelling `Some(kk)` is CLEAN and is pinned as
///     correct by `asan_b04_7_option_heap_enum_struct_field_drop_no_leak`,
///     whose own comment calls it a correct division of one allocation each.
///     The `ow` cell holds that twin here so the two are read together;
///   - `am`: the ARG-MOVE `eat(f2.k)`, through the whole-move sibling
///     `place_optres_field_whole_move_info`. Not in the row -- found by
///     probing the other callers of the classifier this exclusion sits in;
///   - `be`: the boxed `Err` SIDE (`Result[i64, K]`), likewise not in the
///     row. The exclusion tests both halves because the zero is of the
///     shared payload area, so whichever side is live is the one lost.
///
/// FIVE controls, clean before AND after with identical alloc/free counts,
/// each a direction this exclusion could have turned into a double free by
/// leaving a source armed that something else already owns:
///
///   - `vw` / `wc`: non-binding arms (`Ok(K.A(_))`, `Ok(_)`). No consuming
///     site fires, so the field's drop always owned it -- these are what
///     show the source zero is the cause rather than a missing registration;
///   - `lm`: the LET-MOVE `let lm = e2.k`. The load-bearing control: it
///     binds the field to a local whose own cleanup runs, so if the arm
///     binding were a second owner this is where the double free would
///     appear. 23 allocs / 23 frees, 0 invalid, both before and after;
///   - `ub`: an UNBOXED `Result[String, i64]` field. The inline width keeps
///     the old behaviour exactly -- its buffer really is the binding's to
///     free and the source zero really is required -- which is what makes
///     the exclusion narrow rather than a blanket retreat;
///   - `ow`: the `Option` twin of `wb`, unchanged.
#[test]
fn asan_boxed_result_field_keeps_an_owner_for_its_envelope() {
    assert_clean_asan_run(
        r#"
struct R2 { s: String, t: String, u: String }
enum K { A(R2), B }
struct HolderR { k: Result[K, i64], n: i64 }
struct HolderE { k: Result[i64, K], n: i64 }
struct HolderS { k: Result[String, i64], n: i64 }
struct HolderO { k: Option[K], n: i64 }

fn mkr(i: i64) -> R2 { return R2 { s: f"ssssssss{i}", t: f"tttttttt{i}", u: f"uuuuuuuu{i}" }; }
fn eat(x: Result[K, i64]) -> i64 {
    match x { Result.Ok(K.A(r)) => { return r.s.len(); } Result.Ok(K.B) => { return 0; } Result.Err(e) => { return e; } }
}

fn main() {
    let mut i = 0;
    while i < 3 {
        let a = HolderR { k: Result.Ok(K.A(mkr(i))), n: i };
        match a.k { Result.Ok(K.A(r)) => { println(f"d:{r.s}"); } Result.Ok(K.B) => {} Result.Err(e) => {} }

        let b = HolderR { k: Result.Ok(K.A(mkr(i))), n: i };
        match b.k { Result.Ok(kk) => { println("wb"); } Result.Err(e) => {} }

        let c = HolderR { k: Result.Ok(K.A(mkr(i))), n: i };
        match c.k { Result.Ok(K.A(_)) => { println("vw"); } Result.Ok(K.B) => {} Result.Err(e) => {} }

        let d = HolderR { k: Result.Ok(K.A(mkr(i))), n: i };
        match d.k { Result.Ok(_) => { println("wc"); } Result.Err(e) => {} }

        let e2 = HolderR { k: Result.Ok(K.A(mkr(i))), n: i };
        let lm = e2.k;
        match lm { Result.Ok(K.A(r)) => { println(f"lm:{r.s}"); } Result.Ok(K.B) => {} Result.Err(e) => {} }

        let f2 = HolderR { k: Result.Ok(K.A(mkr(i))), n: i };
        println(f"am:{eat(f2.k)}");

        let g = HolderE { k: Result.Err(K.A(mkr(i))), n: i };
        match g.k { Result.Ok(v) => {} Result.Err(K.A(r)) => { println(f"be:{r.s}"); } Result.Err(K.B) => {} }

        let h = HolderS { k: Result.Ok(f"pppppppp{i}"), n: i };
        match h.k { Result.Ok(s) => { println(f"ub:{s}"); } Result.Err(e) => {} }

        let o = HolderO { k: Option.Some(K.A(mkr(i))), n: i };
        match o.k { Option.Some(kk) => { println("ow"); } Option.None => {} }

        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "d:ssssssss0",
            "wb",
            "vw",
            "wc",
            "lm:ssssssss0",
            "am:9",
            "be:ssssssss0",
            "ub:pppppppp0",
            "ow",
            "d:ssssssss1",
            "wb",
            "vw",
            "wc",
            "lm:ssssssss1",
            "am:9",
            "be:ssssssss1",
            "ub:pppppppp1",
            "ow",
            "d:ssssssss2",
            "wb",
            "vw",
            "wc",
            "lm:ssssssss2",
            "am:9",
            "be:ssssssss2",
            "ub:pppppppp2",
            "ow",
            "end",
        ],
        "asan_boxed_result_field_keeps_an_owner_for_its_envelope",
    );
}

/// B-2026-09-09-9 — the shapes the nested-indexed-read fix newly makes
/// buildable, under ASAN.
///
/// Every cell here refused to compile before the fix ("nested indexed read
/// on '<x>' — element TypeExpr unknown"), so none of them had ever executed
/// on a compiled backend: widening what builds is exactly the change that
/// needs a memory gate behind it, since a program that never linked cannot
/// have been leaking. All four are valgrind-clean at `KARAC_OPT_LEVEL=0`
/// and `2` alike.
///
/// The cells were CHOSEN by measuring, not assumed. Two neighbouring shapes
/// that the same fix unblocks were deliberately absent because they were NOT
/// clean at `-O0` — a user-enum `Array[Vec[String], 2]` payload lost 48 B
/// in 1 block, and an `Array[String, 2]` payload indexed once lost 18 B in
/// 2. The second needed no part of this fix to build (a single index never
/// reaches the nested-read path), which is what placed both in the
/// pre-existing Array-element-interior class rather than in this one.
///
/// THAT EXCLUSION IS LIFTED (B-2026-09-09-24). Both are clean now — shape 1
/// at `4dc4bdf4e`, shape 2 not until `b98707ee9` — and both are pinned, with
/// the row's own two controls, in
/// `asan_indexed_array_payload_interior_has_exactly_one_owner` just below.
/// The cells stay out of THIS fixture rather than being folded in, because
/// what they guard is a different fix; the paragraph above is kept in the
/// past tense as the record of why they were ever apart.
/// B-2026-09-09-18 — the fresh-temp `Option`/`Result` argument now runs its
/// payload's `Drop` body in the CALLER's frame, and this is the guard on
/// the one way that could go wrong.
///
/// The body itself is memory-safe by kind: it rides `ContainerElemBodies`,
/// which frees nothing. The hazard is the one
/// `freshtemp_payload_bodies_action` documents for the match-scrutinee
/// sibling — a `Drop` body that MUTATES a heap field (`self.xs.clear()`)
/// run against a reconstructed COPY of the payload's `{ptr,len,cap}` frees
/// the buffer and zeroes the copy's cap, while the real owner keeps the
/// stale pointer and frees it again.
///
/// It does not arise here, and the reason is structural rather than lucky:
/// this registration stages the ARGUMENT AGGREGATE — the same words the
/// callee was handed — so for a boxed payload the staged slot holds the box
/// POINTER and the walker reaches the one heap object through it. No copy
/// of the payload words is made, so there is no second cap to zero.
///
/// Cell 1 is the named-local control (correct before this row, and the
/// shape whose behaviour must not change), 2 and 3 the two fresh-temp
/// spellings the fix adds — no arm, and an arm that binds the payload out.
#[test]
fn asan_freshtemp_optres_arg_payload_body_mutating_heap_frees_once() {
    let prog = |call: &str| {
        format!(
                "struct Rv {{ id: i64, xs: Vec[i64] }}\n\
                 impl Drop for Rv {{\n\
                 \x20   fn drop(mut ref self) {{\n\
                 \x20       println(f\"dv:{{self.id}}:{{self.xs.len()}}\");\n\
                 \x20       self.xs.clear();\n\
                 \x20       println(f\"cleared:{{self.xs.len()}}\");\n\
                 \x20   }}\n\
                 }}\n\
                 fn mkv(i: i64) -> Rv {{ return Rv {{ id: i, xs: [i, i + 1, i + 2, i + 3, i + 4, i + 5] }}; }}\n\
                 fn ignore(x: Option[Rv]) {{ println(\"  ig\"); }}\n\
                 fn matchit(x: Option[Rv]) {{ match x {{ Option.Some(r) => {{ println(f\"  m:{{r.id}}\"); }} Option.None => {{ println(\"  mn\"); }} }} }}\n\
                 fn main() {{ {call} }}\n"
            )
    };
    // 1 — named local: the caller's let site owns the bodies, as before.
    assert_clean_asan_run(
        &prog("let a = Option.Some(mkv(1)); ignore(a);"),
        &["ig", "dv:1:6", "cleared:0"],
        "b18-named-local-control",
    );
    // 2 — fresh temp, callee never matches. The row's base shape.
    assert_clean_asan_run(
        &prog("ignore(Option.Some(mkv(2)));"),
        &["ig", "dv:2:6", "cleared:0"],
        "b18-freshtemp-no-arm",
    );
    // 3 — fresh temp, callee binds the payload out. The arm is not the
    //     axis (it was never the discriminator), but it is the shape where
    //     a second owner would show up if the arm took one.
    assert_clean_asan_run(
        &prog("matchit(Option.Some(mkv(3)));"),
        &["m:3", "dv:3:6", "cleared:0"],
        "b18-freshtemp-consuming-arm",
    );
}

/// B-2026-09-09-8 — a boxed `Result` payload's box had NO OWNER at all,
/// because the param-site arm that registers one ran for `Option` only.
///
/// The row is filed as an INTERIOR leak ("54 B in 6 blocks", the payload's
/// `String`s) on the reading that the box was already owned. Re-measured at
/// `-O0` under valgrind on the tree this fixes, three calls of
/// `plainR(Result.Ok(..))` over `Result[Array[String, 2], i64]`:
///
///     definitely lost  144 B in 3 blocks   the BOXES     <- unrecorded
///     indirectly lost   54 B in 6 blocks   the Strings
///
/// The interior is INDIRECT — it hangs off a box nobody freed — and the
/// `Option` twin, which does reach the registration, loses the 54 B alone
/// with no boxes. So the row's number was read off the wrong enum.
///
/// The gate it names (`option_generic_arg_type_expr` has no `Result`
/// sibling) is real but is NOT what holds this up: it sits BELOW
/// `if enum_lit != "Option" { continue; }`, so no `Result` reaches it. Both
/// had to move, and the extractor had to become per-VARIANT — a `Result`
/// boxes `Ok` and `Err` independently, so "the payload" is not a property
/// of the type (cell 5 is the shape that proves it).
///
/// EVERY CELL HERE IS A TUPLE PAYLOAD, deliberately. The row's own
/// `Array[String, 2]` spelling still loses its 54 B interior after this
/// fix, because an `Array` payload is admitted by neither the tuple filter
/// nor the leaf drop — that is B-2026-09-06-49, which is OPEN and reverted
/// (its param-site walk needs a caller-side suppressor that breaks generic
/// callees). Asserting the array cell clean here would put the `-O0` ASAN
/// leg red on someone else's open row. The boxes it loses ARE fixed.
#[test]
fn asan_boxed_result_param_payload_box_is_owned() {
    const PRE: &str = "fn seed() -> i64 { env.args().len() }\n";

    // 1 — the row's shape with a tuple payload: a fresh temp handed
    //     straight to a by-value param, so no binding exists anywhere.
    //     198 B lost per program before the fix (144 boxes + 54 interior).
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn plainR(x: Result[(String, String), i64]) {{\n\
                 \x20  match x {{ Ok(t) => {{ println(f\"s:{{t.0}}\") }} Err(e) => {{ println(f\"e{{e}}\") }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{\n\
                 \x20  plainR(Result.Ok((f\"aaaaaaaa{{i}}{{seed()}}\", f\"bbbbbbbb{{i}}\"))); i = i + 1; }} }}\n"
            ),
            &["s:aaaaaaaa01", "s:aaaaaaaa11"],
            "b8-result-tuple-fresh-temp",
        );

    // 2 — NAMED binding of the whole `Result`. The caller's let site
    //     registers a box drop and then disarms it at the call-arg move
    //     (`suppress_inline_option_result_binding_move`), which is why the
    //     callee has to take over: with neither owning it, the box leaked
    //     here exactly as in cell 1. This is the cell that would double-free
    //     if that disarm did NOT reach `Result`.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn takeT(x: Result[(String, String), i64]) {{\n\
                 \x20  match x {{ Ok(t) => {{ println(f\"s:{{t.0}}\") }} Err(e) => {{ println(f\"e{{e}}\") }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{\n\
                 \x20  let r: Result[(String, String), i64] = Result.Ok((f\"aaaaaaaa{{i}}{{seed()}}\", f\"bbbbbbbb{{i}}\"));\n\
                 \x20  takeT(r); i = i + 1; }} }}\n"
            ),
            &["s:aaaaaaaa01", "s:aaaaaaaa11"],
            "b8-result-tuple-named-binding",
        );

    // 3 — a DESTRUCTURING arm. The leaves take the interior and the
    //     registration is retracted to box-only
    //     (`retract_boxed_tuple_inner_drop_for_arm`, which already admitted
    //     `Ok`/`Err` patterns — the retraction was never the `Option`-only
    //     half). Without that retraction this cell double-frees rather than
    //     leaking, so it fails LOUDER than the others if the widening ever
    //     outruns it.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn takeD(x: Result[(String, String), i64]) {{\n\
                 \x20  match x {{ Ok((a, b)) => {{ println(f\"s:{{a}}{{b}}\") }} Err(e) => {{ println(f\"e{{e}}\") }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{\n\
                 \x20  takeD(Result.Ok((f\"aaaaaaaa{{i}}{{seed()}}\", f\"bbbbbbbb{{i}}\"))); i = i + 1; }} }}\n"
            ),
            &["s:aaaaaaaa01bbbbbbbb0", "s:aaaaaaaa11bbbbbbbb1"],
            "b8-result-tuple-destructured",
        );

    // 4 — CONTROL, an ESCAPING param: `outerT` hands its param straight
    //     back, so it must register NOTHING and leave the terminal consumer
    //     the only owner. A registration that ignored
    //     `optres_by_value_nonescaping_param_names` frees a box it already
    //     handed on — a double free, not a leak.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn innerT(x: Result[(String, String), i64]) {{\n\
                 \x20  match x {{ Ok((a, b)) => {{ println(f\"s:{{a}}{{b}}\") }} Err(e) => {{ println(f\"e{{e}}\") }} }} }}\n\
                 fn outerT(x: Result[(String, String), i64]) -> Result[(String, String), i64] {{ return x; }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{\n\
                 \x20  innerT(outerT(Result.Ok((f\"aaaaaaaa{{i}}{{seed()}}\", f\"bbbbbbbb{{i}}\")))); i = i + 1; }} }}\n"
            ),
            &["s:aaaaaaaa01bbbbbbbb0", "s:aaaaaaaa11bbbbbbbb1"],
            "b8-result-tuple-escaping-param-control",
        );

    // 5 — BOTH VARIANTS BOX, and the two payloads have different types.
    //     This is why the extractor is per-variant: `Ok` reads generic arg
    //     0 and `Err` arg 1, and the loop registers a separate tag-guarded
    //     box drop for each. Exactly one fires at runtime. Reading the
    //     `Ok` type for the `Err` arm would free a 3-tuple as a 2-tuple.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn plainB(x: Result[(String, String), (String, String, String)]) {{\n\
                 \x20  match x {{ Ok(t) => {{ println(f\"o:{{t.0}}\") }} Err(e) => {{ println(f\"e:{{e.0}}\") }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 3 {{\n\
                 \x20  if i == 1 {{ plainB(Result.Err((f\"pppppppp{{i}}{{seed()}}\", f\"qqqqqqqq{{i}}\", f\"rrrrrrrr{{i}}\"))); }}\n\
                 \x20  else {{ plainB(Result.Ok((f\"aaaaaaaa{{i}}{{seed()}}\", f\"bbbbbbbb{{i}}\"))); }}\n\
                 \x20  i = i + 1; }} }}\n"
            ),
            &["o:aaaaaaaa01", "e:pppppppp11", "o:aaaaaaaa21"],
            "b8-result-both-variants-box",
        );

    // 6 — CONTROL, the `Err` path over a payload that does NOT box. The
    //     registration is tag-guarded, so a program that never constructs
    //     the boxing variant must free nothing extra.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn plainR4(x: Result[(String, String), i64]) {{\n\
                 \x20  match x {{ Ok(t) => {{ println(f\"s:{{t.0}}\") }} Err(e) => {{ println(f\"e{{e}}\") }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{ plainR4(Result.Err(i + seed())); i = i + 1; }} }}\n"
            ),
            &["e1", "e2"],
            "b8-result-err-path-control",
        );

    // 7 — CONTROL, the `Option` twin, unchanged by this commit and clean
    //     before it. It is here so a later change that breaks the shared
    //     extractor shows up on both enums rather than only the new one.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn plainO(x: Option[(String, String)]) {{\n\
                 \x20  match x {{ Some(t) => {{ println(f\"s:{{t.0}}\") }} None => {{ println(\"n\") }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{\n\
                 \x20  plainO(Some((f\"aaaaaaaa{{i}}{{seed()}}\", f\"bbbbbbbb{{i}}\"))); i = i + 1; }} }}\n"
            ),
            &["s:aaaaaaaa01", "s:aaaaaaaa11"],
            "b8-option-tuple-twin-control",
        );
}

/// B-2026-09-19-36 — the MEMORY half. The widened hand-back disarm is
/// allowed to be wrong only in the STRANDING direction, so these are the
/// cells that catch it going wrong: every one of them is a leg where the
/// callee KEEPS the argument and the caller must still free its own box.
///
/// `optF` / `resF` / `bareF` return a payload-free variant
/// (`Ho { g: Option.None }`, `Hr { g: Result.Err(7) }`, `Option.None`), so
/// nothing of the argument leaves the frame and disarming would strand the
/// box. `discard` hands the result to nobody, which is the window
/// B-2026-09-16-16 guards and which the disarm must stay out of.
///
/// The `c == true` legs are deliberately ABSENT, and that is the whole
/// reason this fixture is only four cells: they leak 24 bytes each, because
/// the caller's result binding arms no box drop for a generic enum inside a
/// returned aggregate's field once the argument is disarmed. That is
/// B-2026-09-19-35's missing drop, pre-dating this change (the all-paths
/// spelling already leaked on the parent tree), and it would fail the Linux
/// LeakSanitizer leg here rather than measure this row. Their VALUES are
/// pinned by the output fixture pair instead.
///
/// THIS FIXTURE ONLY REPORTS ON THE `-O0` LEG, and that is worth knowing
/// before trusting a green run of it. Verified by fault injection rather
/// than assumed: forcing the disarm unconditionally (`handed = live`, so
/// the argument stands down whatever the return holds) strands all three
/// cells — `valgrind` puts them at 24 bytes each where they were 0 — and
/// this fixture still PASSED under a plain `cargo test --features llvm`.
/// At `-O2` LLVM deletes an allocation nothing observes, so the leak never
/// reaches LeakSanitizer. Re-run at `KARAC_OPT_LEVEL=0` and the same
/// injected fault fails it, which is what `scripts/asan-o0-leg.sh` does and
/// why that leg is the authoritative one for a cell of this class.
#[test]
fn asan_option_wrapped_handback_does_not_strand_the_dies_inside_legs() {
    const DECLS: &str = "enum G1[T] { Y(T), N }\n\
             struct Ho[T] { g: Option[G1[T]] }\n\
             struct Hr[T] { g: Result[G1[T], i64] }\n\
             fn optW[T](g: G1[T], c: bool) -> Ho[T] { if c { return Ho { g: Option.Some(g) } } return Ho { g: Option.None }; }\n\
             fn resW[T](g: G1[T], c: bool) -> Hr[T] { if c { return Hr { g: Result.Ok(g) } } return Hr { g: Result.Err(7) }; }\n\
             fn optBare[T](g: G1[T], c: bool) -> Option[G1[T]] { if c { return Option.Some(g) } return Option.None; }\n\
             fn shwO(o: Option[G1[String]]) { match o { Option.Some(i) => { match i { G1.Y(v) => { println(f\"mx {v.len()}\") } G1.N => { println(\"mx 0\") } } } Option.None => { println(\"none\") } } }\n\
             fn shwR(r: Result[G1[String], i64]) { match r { Result.Ok(i) => { match i { G1.Y(v) => { println(f\"mx {v.len()}\") } G1.N => { println(\"mx 0\") } } } Result.Err(e) => { println(\"err\") } } }\n";

    // The callee keeps the argument and returns a payload-free variant —
    // through a struct field, on both the `Option` and `Result` channels.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"aaaaaaaa-1\"); let h = optW(g, false); shwO(h.g) }}\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"bbbbbbbb-2\"); let h = resW(g, false); shwR(h.g) }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["none", "err", "end"],
            "b91936-dies-inside",
        );

    // The BARE return with no surrounding struct, same leg.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"cccccccc-3\"); let o = optBare(g, false); shwO(o) }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["none", "end"],
            "b91936-bare-dies-inside",
        );

    // The result consumed by nobody — the discarded-statement window.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"dddddddd-4\"); optW(g, false); println(\"x\") }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["x", "end"],
            "b91936-discarded",
        );
}

/// B-2026-09-19-48 — A HEAP-CARRYING NAMED-STRUCT `Option` PAYLOAD HANDED
/// TO A BY-VALUE CALLEE HAD TWO OWNERS FOR ITS FIELD BODIES.
///
/// The codegen twin is `tests/codegen.rs`'s
/// `e2e_named_struct_optres_payload_field_body_runs_once`, where the
/// inline-payload cells are BODY-ONLY — they double a `println` and free
/// nothing, so no sanitizer leg can see them. These cells give the payload
/// a `String` and a `Vec[i64]`, which does two things: it puts real memory
/// under the doubled body, and it makes the payload too wide to ride
/// inline, so the second owner is minted at a DIFFERENT site
/// (`disarm_struct_field_bodies_at`'s `$keep` mint rather than
/// `bind_pattern_values`' registration).
///
/// Both spellings the row names are here — the arm that only READS the
/// payload and the arm that MOVES a field into an in-frame local — run in
/// a loop so a double free has somewhere to land. Measured at
/// `KARAC_OPT_LEVEL=0` under valgrind on the fix: 56 allocs, 56 frees,
/// 0 bytes in use at exit, 0 errors.
#[test]
fn asan_named_struct_optres_payload_field_bodies_have_one_owner() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct Q { r: R, s: R }

fn mkr(k: i64) -> R { return R { id: k, tag: f"tag-{k}-padded-out-well-past-any-inline-capacity", xs: [k, k, k] } }

fn peek(o: Option[Q]) -> i64 { match o { Option.Some(t) => { return t.r.id + 100; } Option.None => { return 0; } } }
fn take(o: Option[Q]) -> i64 { match o { Option.Some(t) => { let x = t.r; return x.tag.len(); } Option.None => { return 0; } } }

fn main() {
    let mut n = 0;
    while n < 3 {
        { let g = peek(Option.Some(Q { r: mkr(n * 2), s: mkr(n * 2 + 1) })); println(f"p{g}"); }
        { let a = Option.Some(Q { r: mkr(n * 2), s: mkr(n * 2 + 1) }); let g = take(a); println(f"t{g}"); }
        n = n + 1;
    }
    println("end");
}
"#,
        &[
            "dR1", "dR0", "p100", "dR0", "dR1", "t46", "dR3", "dR2", "p102", "dR2", "dR3", "t46",
            "dR5", "dR4", "p104", "dR4", "dR5", "t46", "end",
        ],
        "b91948-named-struct-payload-bodies",
    );
}

/// B-2026-09-19-56 — THE MEMORY HALF OF THE METHOD-CALL-RESULT ARGUMENT,
/// which is where a repair of this shape goes wrong if it goes wrong.
///
/// The row is a LOST `Drop` body: a method result passed by value ran the
/// callee's own param body on no surface. Fixing it means giving that temp
/// an owner, and in this family a new owner is the standard way a lost body
/// becomes a DOUBLE FREE — the caller's fresh wrapper registered beside a
/// callee's, freeing one buffer twice (B-2026-09-06-60 is the nearest
/// precedent, `free(): double free detected in tcache 2` under the JIT and
/// at `-O0`, a SEGV at `-O2`).
///
/// So the body half alone would not have been enough evidence: it is
/// scalar, and a scalar double is invisible to every sanitizer. Every
/// element here carries a `String` past inline capacity and a `Vec`, and
/// every printed line reads BOTH lengths back, so a freed buffer cannot
/// pass for a live one and a doubled body shows up as a doubled line.
///
/// Three producers per iteration, in a loop so a per-iteration leak
/// accumulates instead of rounding to one block: a plain method
/// (`h.mk(i)`), a method reached through `self` (`h.viaself`), and an
/// inherent `dup` whose receiver is a live local — that last one is the
/// cell where the ORIGINAL and the copy are both owed a body, which is the
/// count a double-free repair gets wrong first.
///
/// Valgrind at `KARAC_OPT_LEVEL=0`: 83 allocs, 83 frees, 0 errors.
///
/// The body half is
/// `e2e_method_call_result_argument_runs_the_callees_param_drop_body`
/// (tests/codegen.rs), which carries the mechanism and the seven controls.
#[test]
fn asan_method_call_result_argument_no_double_free() {
    assert_clean_asan_run(
        r#"
struct P { k: i64, name: String, xs: Vec[i64] }
impl Drop for P { fn drop(mut ref self) { println(f"d{self.k}n{self.name.len()}x{self.xs.len()}") } }
impl P { fn dup(ref self) -> P { return P { k: self.k + 100, name: f"{self.name}-dup", xs: [self.k] } } }
struct H { n: i64 }
impl H {
    fn mk(ref self, k: i64) -> P { return P { k: k, name: "alpha-padded-out-well-past-any-inline-capacity", xs: [k, k] } }
    fn viaself(ref self, k: i64) -> P { return self.mk(k) }
}
fn sink(r: P) { println(f"s{r.k}n{r.name.len()}x{r.xs.len()}") }

fn main() {
    let h = H { n: 1 };
    let mut i = 0;
    while i < 3 {
        sink(h.mk(i));
        sink(h.viaself(i + 10));
        { let p = h.mk(i + 20); sink(p.dup()); }
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "s0n46x2",
            "d0n46x2",
            "s10n46x2",
            "d10n46x2",
            "s120n50x1",
            "d120n50x1",
            "d20n46x2",
            "s1n46x2",
            "d1n46x2",
            "s11n46x2",
            "d11n46x2",
            "s121n50x1",
            "d121n50x1",
            "d21n46x2",
            "s2n46x2",
            "d2n46x2",
            "s12n46x2",
            "d12n46x2",
            "s122n50x1",
            "d122n50x1",
            "d22n46x2",
            "end",
        ],
        "b56-method-call-result-argument",
    );
}

/// B-2026-09-24-21 — a tuple literal that moves an inline `Option`/`Result`
/// local now disarms it, and every owner of the tuple frees the payload: a
/// returned tuple (the row's own cell, a `let t = (..); t` return, a by-value
/// param handed back inside it, a conditional `return`), an annotated or
/// unannotated `let`, a destructure, a `Vec` push, a struct field, a `Some(..)`
/// wrap, a `match (a, b)` scrutinee, a temp argument, and a discarded
/// `let _ = (..)`, which leaves the local its owner. On `main` 21 of these
/// spellings double freed and the temp argument leaked.
#[test]
fn asan_tuple_literal_moves_inline_optres_local() {
    assert_clean_asan_run(
        r#"struct S { t: (Option[String], i64) }
fn mk(k: i64) -> (Option[String], i64) { let label = Some(f"heap-string-longer-than-sso-{k}"); (label, k) }
fn mkt(k: i64) -> (Option[String], i64) { let label = Some(f"heap-string-longer-than-sso-{k}"); let t = (label, k); t }
fn mkp(label: Option[String], k: i64) -> (Option[String], i64) { (label, k) }
fn mkv(k: i64) -> (Option[Vec[i64]], i64) { let v: Vec[i64] = [k, 2, 3]; let o = Some(v); (o, k) }
fn mkr(k: i64) -> (Result[String, i64], i64) { let r: Result[String, i64] = Ok(f"heap-string-longer-than-sso-{k}"); (r, k) }
fn mkc(k: i64) -> (Option[String], i64) { let label = Some(f"heap-string-longer-than-sso-{k}"); if k > 5 { return (label, k); } (None, 0) }
fn eat(t: (Option[String], i64)) -> i64 { t.1 }
fn txt(o: ref Option[String]) -> String { match o { Some(s) => s.clone(), None => "none" } }
fn main() {
    let a = mk(1); println(f"{txt(a.0)} {a.1}");
    let b = mkt(2); println(f"{txt(b.0)} {b.1}");
    let l = Some(f"heap-string-longer-than-sso-3"); let c = mkp(l, 3); println(f"{txt(c.0)} {c.1}");
    let v = mkv(4); println(v.1);
    let r = mkr(5); println(r.1);
    let (d0, d1) = mk(6); println(f"{txt(d0)} {d1}");
    let e = mkc(7); let f = mkc(1); println(f"{txt(e.0)} {e.1} {f.1}");
    let mut n = 0; for i in 0..3 { let q = mk(i); n = n + q.1; } println(n);
    let x = Some(f"heap-string-longer-than-sso-8"); let tx: (Option[String], i64) = (x, 8); println(f"{txt(tx.0)} {tx.1}");
    let y = Some(f"heap-string-longer-than-sso-9"); let ty = (y, 9); let (y0, y1) = ty; println(f"{txt(y0)} {y1}");
    let z = Some(f"heap-string-longer-than-sso-10"); let mut vs: Vec[(Option[String], i64)] = []; vs.push((z, 10)); println(vs.len());
    let w = Some(f"heap-string-longer-than-sso-11"); let s = S { t: (w, 11) }; println(s.t.1);
    let u = Some(f"heap-string-longer-than-sso-12"); let o = Some((u, 12)); match o { Some(p) => println(p.1), None => println("n") }
    let g = Some(f"heap-string-longer-than-sso-13"); let h = Some(f"heap-string-longer-than-sso-14"); match (g, h) { (Some(p), Some(q)) => println(f"{p} {q}"), _ => println("n") }
    let m = Some(f"heap-string-longer-than-sso-15"); println(eat((m, 15)));
    let dd = Some(f"heap-string-longer-than-sso-16"); let _ = (dd, 16);
    let lt = Some(f"heap-string-longer-than-sso-17"); let tt = (lt, 17); println(eat(tt));
    println("end")
}
"#,
        &[
            "heap-string-longer-than-sso-1 1",
            "heap-string-longer-than-sso-2 2",
            "heap-string-longer-than-sso-3 3",
            "4",
            "5",
            "heap-string-longer-than-sso-6 6",
            "heap-string-longer-than-sso-7 7 0",
            "3",
            "heap-string-longer-than-sso-8 8",
            "heap-string-longer-than-sso-9 9",
            "1",
            "11",
            "12",
            "heap-string-longer-than-sso-13 heap-string-longer-than-sso-14",
            "15",
            "17",
            "end",
        ],
        "asan_tuple_literal_moves_inline_optres_local",
    );
}

/// B-2026-09-24-23 — a local that ownership promotes to an RC box (it is
/// consumed in one arm and read after the `match`) keeps its `Option` /
/// `Result` payload in the box, and the box now frees it: the box is named by
/// the full type and given that type's value drop, the slot registrars stand
/// down for the handle slot, and the pattern bindings of a later `match` /
/// `if let` / `while let` / `let … else` are views of the box, cloned when
/// they escape. On `main` every one of these spellings leaked the payload
/// (518 B over the program), and the `Result` ones printed wrong values at
/// `-O0` and under the JIT.
#[test]
fn asan_rc_fallback_optres_local_frees_its_payload() {
    assert_clean_asan_run(
        r#"struct P { pos: i64 }
struct S { s: String, k: i64 }
impl P {
    fn take(mut ref self, doc: Option[String]) -> i64 { self.pos = self.pos + 1; match doc { Some(s) => s.len(), None => 0 } }
    fn item(mut ref self, t: i64) -> i64 { let doc = Some(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => self.take(doc), _ => 5 }; let b = match doc { Some(s) => s.len(), None => 0 }; a + b }
}
fn take(doc: Option[String]) -> i64 { match doc { Some(s) => s.len(), None => 0 } }
fn takev(doc: Option[Vec[i64]]) -> i64 { match doc { Some(s) => s.len(), None => 0 } }
fn takes(doc: Option[S]) -> i64 { match doc { Some(s) => s.s.len(), None => 0 } }
fn taker(doc: Result[String, i64]) -> i64 { match doc { Ok(s) => s.len(), Err(e) => e } }
fn takee(doc: Result[i64, String]) -> i64 { match doc { Ok(v) => v, Err(e) => e.len() } }
fn back(doc: Option[String]) -> Option[String] { doc }
fn read(t: i64) -> i64 { let doc = Some(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => take(doc), _ => 5 }; let b = match doc { Some(s) => s.len(), None => 0 }; a + b }
fn armmove(t: i64) -> String { let doc = Some(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => take(doc), _ => 5 }; let b = match doc { Some(s) => s, None => f"none" }; f"{a} {b}" }
fn handback(t: i64) -> i64 { let doc = Some(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => match back(doc) { Some(s) => s.len(), None => 0 }, _ => 5 }; let b = match doc { Some(s) => s.len(), None => 0 }; a + b }
fn whilelet(t: i64) -> i64 { let doc = Some(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => take(doc), _ => 5 }; let mut k = 0; while let Some(s) = doc { k = k + s.len(); break }; a + k }
fn letelse(t: i64) -> i64 { let doc = Some(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => take(doc), _ => 5 }; let Some(s) = doc else { return 0 }; a + s.len() }
fn optvec(t: i64) -> i64 { let doc = Some([1, 2, t]); let a = match t { 0 => takev(doc), _ => 5 }; let b = match doc { Some(s) => s.len(), None => 0 }; a + b }
fn optstruct(t: i64) -> i64 { let doc = Some(S { s: f"heap-string-longer-than-sso-{t}", k: t }); let a = match t { 0 => takes(doc), _ => 5 }; let b = match doc { Some(s) => s.k, None => 0 }; a + b }
fn resok(t: i64) -> i64 { let doc: Result[String, i64] = Ok(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => taker(doc), _ => 5 }; let b = match doc { Ok(s) => s.len(), Err(e) => e }; a + b }
fn resmove(t: i64) -> String { let doc: Result[String, i64] = Ok(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => taker(doc), _ => 5 }; let b = match doc { Ok(s) => s, Err(e) => f"e{e}" }; f"{a} {b}" }
fn reserr(t: i64) -> i64 { let doc: Result[i64, String] = Err(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => takee(doc), _ => 5 }; let b = match doc { Ok(v) => v, Err(e) => e.len() }; a + b }
fn main() {
    let mut p = P { pos: 0 };
    println(f"{p.item(0)} {p.item(1)}");
    println(f"{read(0)} {read(1)}");
    println(f"{armmove(0)} / {armmove(1)}");
    println(f"{handback(0)} {handback(1)}");
    println(f"{whilelet(0)} {whilelet(1)}");
    println(f"{letelse(0)} {letelse(1)}");
    println(f"{optvec(0)} {optvec(1)}");
    println(f"{optstruct(0)} {optstruct(1)}");
    println(f"{resok(0)} {resok(1)}");
    println(f"{resmove(0)} / {resmove(1)}");
    println(f"{reserr(0)} {reserr(1)}");
    println("end")
}
"#,
        &[
            "58 34",
            "58 34",
            "29 heap-string-longer-than-sso-0 / 5 heap-string-longer-than-sso-1",
            "58 34",
            "58 34",
            "58 34",
            "6 8",
            "29 6",
            "58 34",
            "29 heap-string-longer-than-sso-0 / 5 heap-string-longer-than-sso-1",
            "58 34",
            "end",
        ],
        "asan_rc_fallback_optres_local_frees_its_payload",
    );
}

/// B-2026-09-24-28 — the payload shapes B-2026-09-24-23 did not reach: an
/// RC-promoted local (consumed in one arm, read after the `match`) whose
/// payload is a tuple, a three-tuple or a nested `Option`. The box now takes
/// its value drop for these types, the boxed-enum chain registrar stands down
/// for the handle slot as the inline ones already did, an arm binding of the
/// box is not treated as a move out of it, and the argument handed to a
/// callee (a free function or a method) is a deep copy, since the callee
/// frees what it is given and the box still owns its own. On `main` the
/// tuple and nested-`Option` spellings segfaulted on every compiled surface
/// (invalid read, 40 B lost).
#[test]
fn asan_rc_fallback_boxed_optres_payload_handed_a_copy() {
    assert_clean_asan_run(
        r#"struct P { pos: i64 }
impl P {
    fn take(mut ref self, doc: Option[(String, i64)]) -> i64 { self.pos = self.pos + 1; match doc { Some((s, k)) => s.len() + k, None => 0 } }
    fn item(mut ref self, t: i64) -> i64 { let doc = Some((f"heap-string-longer-than-sso-{t}", t)); let a = match t { 0 => self.take(doc), _ => 5 }; let b = match doc { Some((s, k)) => s.len() + k, None => 0 }; a + b }
}
fn take(doc: Option[(String, i64)]) -> i64 { match doc { Some((s, k)) => s.len() + k, None => 0 } }
fn take3(doc: Option[(String, String, i64)]) -> i64 { match doc { Some((s, u, k)) => s.len() + u.len() + k, None => 0 } }
fn takeo(doc: Option[Option[String]]) -> i64 { match doc { Some(Some(s)) => s.len(), _ => 0 } }
fn back(doc: Option[(String, i64)]) -> Option[(String, i64)] { doc }
fn tuple(t: i64) -> i64 { let doc = Some((f"heap-string-longer-than-sso-{t}", t)); let a = match t { 0 => take(doc), _ => 5 }; let b = match doc { Some((s, k)) => s.len() + k, None => 0 }; a + b }
fn tuple3(t: i64) -> i64 { let doc = Some((f"heap-string-longer-than-sso-{t}", f"second-heap-string-longer-than-sso", t)); let a = match t { 0 => take3(doc), _ => 5 }; let b = match doc { Some((s, u, k)) => s.len() + u.len() + k, None => 0 }; a + b }
fn nested(t: i64) -> i64 { let doc = Some(Some(f"heap-string-longer-than-sso-{t}")); let a = match t { 0 => takeo(doc), _ => 5 }; let b = match doc { Some(Some(s)) => s.len(), _ => 0 }; a + b }
fn handback(t: i64) -> i64 { let doc = Some((f"heap-string-longer-than-sso-{t}", t)); let a = match t { 0 => match back(doc) { Some((s, k)) => s.len() + k, None => 0 }, _ => 5 }; let b = match doc { Some((s, k)) => s.len() + k, None => 0 }; a + b }
fn armmove(t: i64) -> String { let doc = Some((f"heap-string-longer-than-sso-{t}", t)); let a = match t { 0 => take(doc), _ => 5 }; let b = match doc { Some((s, k)) => s, None => f"none" }; f"{a} {b}" }
fn iflet(t: i64) -> i64 { let doc = Some((f"heap-string-longer-than-sso-{t}", t)); let a = match t { 0 => take(doc), _ => 5 }; let b = if let Some((s, k)) = doc { s.len() + k } else { 0 }; a + b }
fn main() {
    let mut p = P { pos: 0 };
    println(f"{p.item(0)} {p.item(1)}");
    println(f"{tuple(0)} {tuple(1)}");
    println(f"{tuple3(0)} {tuple3(1)}");
    println(f"{nested(0)} {nested(1)}");
    println(f"{handback(0)} {handback(1)}");
    println(f"{armmove(0)} / {armmove(1)}");
    println(f"{iflet(0)} {iflet(1)}");
    let doc = Some((f"heap-string-longer-than-sso-1", 1));
    let mut n = 0;
    for i in 0..3 { if i == 2 { n = n + take(doc); } else { n = n + match doc { Some((s, k)) => s.len(), None => 0 }; } }
    println(n);
    println("end")
}
"#,
        &[
            "58 35",
            "58 35",
            "126 69",
            "58 34",
            "58 35",
            "29 heap-string-longer-than-sso-0 / 5 heap-string-longer-than-sso-1",
            "58 35",
            "88",
            "end",
        ],
        "asan_rc_fallback_boxed_optres_payload_handed_a_copy",
    );
}

/// B-2026-09-24-31 — an `Option[Map]` / `Option[Set]` local moved on by
/// value is freed once. The `Map`/`Set` handle channel
/// (`inline_option_map_payload_vars`) was missing from the move disarm, the
/// hand-back alias, the discarded hand-back temp and the arm suppressor's
/// alias lookup. So every spelling here, including a call, a `let`, a `return`,
/// a method, a field, a conditional, a loop and a hand-back kept both source and
/// destination armed. On `main` the compiled program printed nothing and
/// valgrind counted 668 errors.
#[test]
fn asan_option_map_local_moved_on_is_freed_once() {
    assert_clean_asan_run(
        r#"struct H { d: Option[Map[i64, String]] }
impl H { fn eat(self, doc: Option[Map[i64, String]]) -> i64 { match doc { Some(m) => m.len(), None => 0 } } }
fn mk(t: i64) -> Map[i64, String] { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); m }
fn take(doc: Option[Map[i64, String]]) -> i64 { match doc { Some(m) => m.len(), None => 0 } }
fn takes(doc: Option[Set[i64]]) -> i64 { match doc { Some(s) => s.len(), None => 0 } }
fn keep(doc: Option[Map[i64, String]]) -> Option[Map[i64, String]] { doc }
fn ret(t: i64) -> Option[Map[i64, String]] { let d = Some(mk(t)); d }
fn cond(t: i64) -> i64 { let d = Some(mk(t)); if t == 0 { take(d) } else { 7 } }
fn main() {
    let m = mk(1); let d = Some(m); println(take(d));
    let d = Some(mk(2)); let q = d; println(take(q));
    let d = ret(3); println(take(d));
    let h = H { d: None }; let d = Some(mk(4)); println(h.eat(d));
    let d = Some(mk(5)); let g = H { d: d }; println(take(g.d));
    let mut s: Set[i64] = Set.new(); s.insert(6); let d = Some(s); println(takes(d));
    println(f"{cond(0)} {cond(1)}");
    let mut n = 0; for i in 0..3 { let d = Some(mk(i)); n = n + take(d); } println(n);
    let d = Some(mk(7)); let e = keep(d); println(take(e));
    let d = Some(mk(8)); keep(d);
    let d = Some(mk(9)); let e = keep(d); let k = match e { Some(x) => { let mut v: Vec[Map[i64, String]] = Vec.new(); v.push(x); v.len() }, None => 0 }; println(k);
    println("end")
}
"#,
        &["1", "1", "1", "1", "1", "1", "1 7", "3", "1", "1", "end"],
        "asan_option_map_local_moved_on_is_freed_once",
    );
}
